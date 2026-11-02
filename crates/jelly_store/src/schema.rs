use rusqlite::{Connection, TransactionBehavior};

use crate::error::StoreError;

pub(crate) type Migration = fn(&Connection) -> rusqlite::Result<()>;

/// `user_version` counts the steps a file has run, and a fresh file runs them all. A
/// shipped step never changes: a schema change appends one, and `tests/golden` records the
/// schema each version builds.
pub(crate) const MIGRATIONS: &[Migration] = &[|connection| connection.execute_batch(V1)];

const V1: &str = "
CREATE TABLE wallets (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  color      TEXT NOT NULL DEFAULT '#71717a'
    CHECK (color GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]')
) STRICT;

CREATE TABLE wallet_settings (
  wallet_id         INTEGER PRIMARY KEY REFERENCES wallets(id) ON DELETE CASCADE,
  passphrase_policy TEXT    NOT NULL CHECK (passphrase_policy IN ('none','stored','prompt')),
  account_index    INTEGER NOT NULL DEFAULT 0 CHECK (account_index >= 0)
) STRICT;

CREATE TABLE wallet_chain_state (
  wallet_id             INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain                 TEXT    NOT NULL,
  enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  poll_secs             INTEGER CHECK (poll_secs IS NULL OR poll_secs > 0),
  restore_height INTEGER CHECK (restore_height IS NULL OR restore_height >= 0),
  synced_height         INTEGER CHECK (synced_height IS NULL OR synced_height >= 0),
  PRIMARY KEY (wallet_id, chain)
) STRICT, WITHOUT ROWID;

CREATE TABLE addresses (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id        INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain            TEXT    NOT NULL,
  address          TEXT    NOT NULL,
  key_source       TEXT    NOT NULL CHECK (key_source IN ('derived','imported','subaddress')),
  derivation_path  TEXT,
  vault_entry      TEXT,
  subaddress_major     INTEGER CHECK (subaddress_major IS NULL OR subaddress_major >= 0),
  subaddress_minor     INTEGER CHECK (subaddress_minor IS NULL OR subaddress_minor >= 0),
  script_kind      TEXT CHECK (script_kind IS NULL OR script_kind IN
    ('p2tr','p2wpkh','p2sh_p2wpkh','p2pkh','ton_v5r1','ton_v4r2','ton_v3r2')),
  derivation_index INTEGER CHECK (derivation_index IS NULL OR derivation_index >= 0),
  label            TEXT,
  created_at       INTEGER NOT NULL,
  CHECK (
    (key_source = 'derived'
       AND derivation_path IS NOT NULL AND vault_entry IS NULL
       AND subaddress_major IS NULL AND subaddress_minor IS NULL)
    OR (key_source = 'imported'
       AND vault_entry IS NOT NULL AND derivation_path IS NULL
       AND subaddress_major IS NULL AND subaddress_minor IS NULL)
    OR (key_source = 'subaddress'
       AND subaddress_major IS NOT NULL AND subaddress_minor IS NOT NULL
       AND derivation_path IS NULL AND vault_entry IS NULL)
  ),
  UNIQUE (wallet_id, chain, address)
) STRICT;

CREATE TABLE balances (
  address_id INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  asset      TEXT    NOT NULL,
  amount     BLOB    NOT NULL CHECK (length(amount) = 16),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (address_id, asset)
) STRICT, WITHOUT ROWID;

CREATE TABLE txs (
  id           INTEGER PRIMARY KEY,
  wallet_id    INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  address_id   INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  chain        TEXT    NOT NULL,
  asset        TEXT    NOT NULL,
  txid         TEXT    NOT NULL,
  direction    TEXT    NOT NULL CHECK (direction IN ('incoming','outgoing','self')),
  outcome      TEXT    NOT NULL CHECK (outcome IN ('success','failed','replaced','unknown')),
  detection    TEXT    NOT NULL CHECK (detection IN ('full','balance_delta')),
  amount       BLOB    NOT NULL CHECK (length(amount) = 16),
  fee          BLOB    CHECK (fee IS NULL OR length(fee) = 16),
  rent         BLOB    CHECK (rent IS NULL OR length(rent) = 16),
  counterparty TEXT,
  pays_for     TEXT,
  height       INTEGER CHECK (height IS NULL OR height >= 0),
  timestamp    INTEGER,
  created_at   INTEGER NOT NULL,
  sort_time    INTEGER NOT NULL GENERATED ALWAYS AS (COALESCE(timestamp, created_at)) STORED,
  UNIQUE (address_id, asset, txid)
) STRICT;
CREATE INDEX idx_txs_wallet_time ON txs (wallet_id, sort_time DESC, id DESC);
CREATE INDEX idx_txs_chain_txid ON txs (chain, txid);

CREATE TABLE scan_cursors (
  address_id INTEGER NOT NULL REFERENCES addresses(id) ON DELETE CASCADE,
  namespace  TEXT    NOT NULL,
  cursor     BLOB    NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (address_id, namespace)
) STRICT, WITHOUT ROWID;

CREATE TABLE endpoint_overrides (
  chain   TEXT    NOT NULL,
  url     TEXT    NOT NULL,
  display TEXT    NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  pinned  INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  PRIMARY KEY (chain, url)
) STRICT, WITHOUT ROWID;

CREATE TABLE endpoint_health (
  chain                TEXT    NOT NULL,
  url                  TEXT    NOT NULL,
  score_permille       INTEGER NOT NULL CHECK (score_permille BETWEEN 0 AND 1000),
  successes            INTEGER NOT NULL CHECK (successes >= 0),
  failures             INTEGER NOT NULL CHECK (failures >= 0),
  consecutive_failures INTEGER NOT NULL CHECK (consecutive_failures >= 0),
  latency_ms           INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  last_success         INTEGER,
  last_failure         INTEGER,
  last_error           TEXT,
  PRIMARY KEY (chain, url)
) STRICT, WITHOUT ROWID;

CREATE TABLE tls_pins (
  chain       TEXT    NOT NULL,
  host        TEXT    NOT NULL,
  spki_sha256 BLOB    NOT NULL CHECK (length(spki_sha256) = 32),
  first_seen  INTEGER NOT NULL,
  PRIMARY KEY (chain, host)
) STRICT, WITHOUT ROWID;

CREATE TABLE contacts (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  wallet_id  INTEGER NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
  chain      TEXT    NOT NULL,
  address    TEXT    NOT NULL,
  name       TEXT    NOT NULL CHECK (length(name) > 0),
  note       TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE (wallet_id, chain, address)
) STRICT;
";

/// Runs the steps the file hasn't had in one transaction, so a failed upgrade leaves it as
/// the last build wrote it. A file from a newer build is refused.
pub(crate) fn migrate(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let failed = |error| StoreError::sqlite("migrating the schema", error);
    // Changing a table means copying it and dropping the original, and with foreign keys on,
    // the drop cascades into every table that refers to it. The pragma is a no-op inside a
    // transaction.
    connection.pragma_update(None, "foreign_keys", false).map_err(failed)?;
    // Immediate, so another connection can't upgrade between this read and the commit.
    let transaction =
        connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(failed)?;
    let stamp: i64 =
        transaction.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(failed)?;
    let found = u32::try_from(stamp)
        .map_err(|_| StoreError::Decode { entity: "pragma", column: "user_version" })?;
    let steps = (1..).zip(migrations);
    let supported = steps.clone().last().map_or(0, |(version, _)| version);
    if found > supported {
        return Err(StoreError::SchemaTooNew { found, supported });
    }
    if found < supported {
        tracing::info!(target: "jelly::store", found, supported, "migrating the wallet database");
        for (_, migration) in steps.filter(|&(version, _)| version > found) {
            migration(&transaction).map_err(failed)?;
        }
        let mut check = transaction.prepare("PRAGMA foreign_key_check").map_err(failed)?;
        if check.exists([]).map_err(failed)? {
            return Err(StoreError::Internal { context: "a migration left a dangling reference" });
        }
        transaction.pragma_update(None, "user_version", supported).map_err(failed)?;
    }
    transaction.commit().map_err(failed)?;
    connection.pragma_update(None, "foreign_keys", true).map_err(failed)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use rusqlite::Connection;

    use super::{MIGRATIONS, Migration, migrate};
    use crate::error::StoreError;

    const PARENT_AND_CHILD: &str = "
        CREATE TABLE parent (id INTEGER PRIMARY KEY) STRICT;
        CREATE TABLE child (parent_id INTEGER NOT NULL REFERENCES parent(id) ON DELETE CASCADE) STRICT;
        INSERT INTO parent (id) VALUES (1);
        INSERT INTO child (parent_id) VALUES (1);
    ";

    /// Foreign keys on, as the shipped writer has them.
    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.pragma_update(None, "foreign_keys", true).unwrap();
        connection
    }

    fn scalar(connection: &Connection, sql: &str) -> i64 {
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// Each table followed by its indexes, as the steps wrote them.
    fn schema(steps: &[Migration]) -> String {
        let mut connection = connection();
        migrate(&mut connection, steps).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT GLOB 'sqlite_*' \
                 ORDER BY tbl_name, type DESC, name",
            )
            .unwrap();
        let statements: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|sql| format!("{};\n", sql.unwrap()))
            .collect();
        statements.join("\n")
    }

    fn golden(version: usize) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/v{version}.sql"))
    }

    /// Records each version that has no file yet. A recorded one is never rewritten, since
    /// the step behind it has shipped.
    #[test]
    #[ignore = "records the schema of a new version; run with --ignored after appending a step"]
    fn regenerate_golden() {
        for version in 1..=MIGRATIONS.len() {
            let path = golden(version);
            if !path.exists() {
                fs::write(&path, schema(&MIGRATIONS[..version])).unwrap();
            }
        }
    }

    #[test]
    fn every_version_still_builds_the_schema_it_shipped() {
        for version in 1..=MIGRATIONS.len() {
            let path = golden(version);
            let recorded = fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("read {}: {error}; record it with regenerate_golden", path.display())
            });
            assert_eq!(
                schema(&MIGRATIONS[..version]),
                recorded,
                "v{version} changed after it shipped; append a step instead"
            );
        }
    }

    #[test]
    fn a_file_runs_only_the_steps_it_has_not_had() {
        let steps: [Migration; 2] = [
            |connection| connection.execute_batch("CREATE TABLE first (id INTEGER) STRICT;"),
            |connection| connection.execute_batch("CREATE TABLE second (id INTEGER) STRICT;"),
        ];
        let mut connection = connection();
        migrate(&mut connection, &steps[..1]).unwrap();
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 1);

        // Running the first step again would fail, because its table exists.
        migrate(&mut connection, &steps).unwrap();
        migrate(&mut connection, &steps).unwrap();
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 2);
    }

    #[test]
    fn a_failed_step_leaves_the_file_as_it_was() {
        let steps: [Migration; 3] = [
            |connection| connection.execute_batch("CREATE TABLE first (id INTEGER) STRICT;"),
            |connection| connection.execute_batch("CREATE TABLE second (id INTEGER) STRICT;"),
            |connection| connection.execute_batch("INSERT INTO missing VALUES (1);"),
        ];
        let mut connection = connection();
        migrate(&mut connection, &steps[..1]).unwrap();

        let error = migrate(&mut connection, &steps).expect_err("the last step fails");
        assert!(matches!(error, StoreError::Sqlite { .. }), "{error:?}");
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 1);
        let second = "SELECT count(*) FROM sqlite_master WHERE name = 'second'";
        assert_eq!(scalar(&connection, second), 0);
    }

    #[test]
    fn rebuilding_a_table_keeps_the_rows_that_refer_to_it() {
        let steps: [Migration; 2] = [
            |connection| connection.execute_batch(PARENT_AND_CHILD),
            |connection| {
                connection.execute_batch(
                    "CREATE TABLE rebuilt (id INTEGER PRIMARY KEY, name TEXT) STRICT;
                     INSERT INTO rebuilt (id) SELECT id FROM parent;
                     DROP TABLE parent;
                     ALTER TABLE rebuilt RENAME TO parent;",
                )
            },
        ];
        let mut connection = connection();
        migrate(&mut connection, &steps).unwrap();

        assert_eq!(scalar(&connection, "SELECT count(*) FROM child"), 1);
        assert_eq!(scalar(&connection, "PRAGMA foreign_keys"), 1);
    }

    #[test]
    fn a_step_that_leaves_a_dangling_reference_is_refused() {
        let steps: [Migration; 2] = [
            |connection| connection.execute_batch(PARENT_AND_CHILD),
            |connection| connection.execute_batch("DELETE FROM parent;"),
        ];
        let mut connection = connection();
        migrate(&mut connection, &steps[..1]).unwrap();

        let error = migrate(&mut connection, &steps).expect_err("the child is left dangling");
        assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");
        assert_eq!(scalar(&connection, "PRAGMA user_version"), 1);
        assert_eq!(scalar(&connection, "SELECT count(*) FROM parent"), 1);
    }

    #[test]
    fn a_deleted_rows_id_is_never_handed_out_again() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection, MIGRATIONS).unwrap();
        connection
            .execute("INSERT INTO wallets (name, created_at, updated_at) VALUES ('kept', 0, 0)", [])
            .unwrap();
        for (table, values) in [
            ("wallets", "(name, created_at, updated_at) VALUES ('next', 0, 0)"),
            (
                "addresses",
                "(wallet_id, chain, address, key_source, derivation_path, created_at) VALUES (1, 'bitcoin', 'next', 'derived', 'm/0', 0)",
            ),
            (
                "contacts",
                "(wallet_id, chain, address, name, created_at) VALUES (1, 'bitcoin', 'next', 'next', 0)",
            ),
        ] {
            let insert = format!("INSERT INTO {table} {values} RETURNING id");
            let first: i64 = connection.query_row(&insert, [], |row| row.get(0)).unwrap();
            connection.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [first]).unwrap();
            let second: i64 = connection.query_row(&insert, [], |row| row.get(0)).unwrap();
            assert!(second > first, "{table}");
        }
    }
}
