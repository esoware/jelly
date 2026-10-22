//! Every statement a data module runs is declared through [`statements`], which names it
//! and lists it for the plan tests below, so they check exactly the text that ships.
//!
//! Every statement is cached, so each is parsed once per connection.

use rusqlite::{Connection, Params, Row};

use crate::error::StoreError;

/// Declares the statements of one module and the table the plan test reads.
macro_rules! statements {
    ($(
        $(#[$meta:meta])*
        $name:ident = $sql:expr ;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: &str = $sql;
        )*

        /// Every statement this module prepares, named for the plan test by its path.
        #[cfg(test)]
        pub(crate) const STATEMENTS: &[(&str, &str)] =
            &[ $( (concat!(module_path!(), "::", stringify!($name)), $name) ),* ];
    };
}

pub(crate) use statements;

pub(crate) fn optional<T, P, F>(
    connection: &Connection,
    sql: &str,
    params: P,
    map: F,
) -> Result<Option<T>, StoreError>
where
    P: Params,
    F: FnMut(&Row<'_>) -> Result<T, StoreError>,
{
    let mut statement = connection.prepare_cached(sql)?;
    let mut rows = statement.query_and_then(params, map)?;
    rows.next().transpose()
}

/// For a statement that always yields a row, such as a `RETURNING` clause, a `COUNT`,
/// or an `EXISTS`. An empty answer is an internal error named by `missing`.
pub(crate) fn required<T, P, F>(
    connection: &Connection,
    sql: &str,
    params: P,
    map: F,
    missing: &'static str,
) -> Result<T, StoreError>
where
    P: Params,
    F: FnMut(&Row<'_>) -> Result<T, StoreError>,
{
    optional(connection, sql, params, map)?.ok_or(StoreError::Internal { context: missing })
}

pub(crate) fn all<T, P, F>(
    connection: &Connection,
    sql: &str,
    params: P,
    map: F,
) -> Result<Vec<T>, StoreError>
where
    P: Params,
    F: FnMut(&Row<'_>) -> Result<T, StoreError>,
{
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_and_then(params, map)?;
    rows.collect()
}

/// How many rows the statement changed.
pub(crate) fn run<P: Params>(
    connection: &Connection,
    sql: &str,
    params: P,
) -> Result<usize, StoreError> {
    Ok(connection.prepare_cached(sql)?.execute(params)?)
}

/// Every statement the data modules prepare, with its name.
#[cfg(test)]
pub(crate) fn shipped_statements() -> Vec<(&'static str, &'static str)> {
    use crate::{address, balance, contact, cursor, endpoint, pin, tx, wallet};

    [
        wallet::STATEMENTS,
        address::STATEMENTS,
        contact::STATEMENTS,
        balance::STATEMENTS,
        tx::STATEMENTS,
        cursor::STATEMENTS,
        endpoint::STATEMENTS,
        pin::STATEMENTS,
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use jelly_core::test_util::FakeClock;
    use rusqlite::{Connection, params_from_iter, types::Value};
    use tempfile::TempDir;

    use super::shipped_statements;
    use crate::db;

    /// The two tables that grow with history. Every other table is bounded by the number of
    /// wallets, addresses, contacts, chains, or endpoints, where a scan is the right plan.
    const UNBOUNDED: [&str; 2] = ["txs", "balances"];
    const TEMP_SORT: &str = "USE TEMP B-TREE FOR ORDER BY";

    /// A file the shipped open and close wrote, so every plan sees the shipped schema. The
    /// directory is returned so it outlives the connection.
    fn shipped_file() -> (TempDir, Connection) {
        let scratch = tempfile::tempdir().unwrap();
        {
            let (writer, readers, _) = db::open(scratch.path(), &FakeClock::default()).unwrap();
            db::close(&writer, &readers).unwrap();
        }
        let connection = Connection::open(scratch.path().join("jelly.sqlite")).unwrap();
        (scratch, connection)
    }

    /// The unbounded table a plan step scans, if any. A plan names a table by the alias the
    /// statement gives it, so `FROM txs t` scans as `SCAN t`.
    fn unbounded_scan(sql: &str, step: &str) -> Option<&'static str> {
        let target = step.strip_prefix("SCAN ")?.split_whitespace().next()?;
        let words: Vec<&str> =
            sql.split_whitespace().filter(|word| !word.eq_ignore_ascii_case("as")).collect();
        UNBOUNDED
            .into_iter()
            .find(|table| target == *table || words.windows(2).any(|pair| pair == [*table, target]))
    }

    /// The plan is fixed at prepare time, so every parameter is bound null. The statement's
    /// shape is under test, not any value.
    fn plan(connection: &Connection, sql: &str) -> Vec<String> {
        let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![Value::Null; statement.parameter_count()];
        let rows = statement.query_map(params_from_iter(nulls), |row| row.get(3)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    }

    /// Indexes `SQLite` creates for a UNIQUE or PRIMARY KEY constraint are left out, because
    /// those are the constraint, not a choice.
    fn declared_indexes(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name ASC",
            )
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn no_shipped_statement_scans_a_table_that_grows_without_bound() {
        let (_scratch, connection) = shipped_file();
        let probe = "SELECT t.id FROM txs t NOT INDEXED";
        let caught: Vec<&str> = plan(&connection, probe)
            .iter()
            .filter_map(|step| unbounded_scan(probe, step))
            .collect();
        assert_eq!(caught, ["txs"], "the check cannot see a deliberate scan");

        for (name, sql) in shipped_statements() {
            for step in plan(&connection, sql) {
                assert_eq!(unbounded_scan(sql, &step), None, "{name} scans: {step}");
            }
        }
    }

    #[test]
    fn no_shipped_statement_sorts_in_a_temporary_tree() {
        let (_scratch, connection) = shipped_file();
        for (name, sql) in shipped_statements() {
            for step in plan(&connection, sql) {
                assert!(!step.contains(TEMP_SORT), "{name} sorts in a temporary tree: {step}");
            }
        }
    }

    #[test]
    fn the_history_page_is_driven_by_the_history_index() {
        let (_scratch, connection) = shipped_file();
        let (_, sql) = shipped_statements()
            .into_iter()
            .find(|(name, _)| *name == "jelly_store::tx::LIST")
            .expect("the statement is shipped");
        let joined = plan(&connection, sql).join(" | ");
        assert!(joined.contains("idx_txs_wallet_time"), "LIST plan: {joined}");
    }

    #[test]
    fn no_index_ships_that_no_statement_can_be_planned_onto() {
        let (_scratch, connection) = shipped_file();
        let mut used = String::new();
        for (_, sql) in shipped_statements() {
            for step in plan(&connection, sql) {
                used.push_str(&step);
                used.push('\n');
            }
        }
        for index in declared_indexes(&connection) {
            assert!(used.contains(&index), "{index} is declared and never planned onto:\n{used}");
        }
    }
}
