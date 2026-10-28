use std::io;

use jelly_core::{ClockError, CoreError};
use rusqlite::{ErrorCode, ffi};
use tokio::task::JoinError;

/// Messages never carry user data. A missing row's id is an integer or a redacted endpoint
/// label, never an address or a full URL.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("there is no {entity} with id {id}")]
    NotFound { entity: &'static str, id: String },

    #[error("this {entity} has the same {columns} as one already stored")]
    AlreadyExists { entity: &'static str, columns: &'static str },

    /// A reference that shouldn't have failed, such as one to a row resolved earlier in
    /// the same transaction. A missing parent the caller named is [`StoreError::NotFound`]
    /// instead.
    #[error("this {entity} refers to a row that isn't there")]
    MissingReference { entity: &'static str },

    #[error("the database refused this {entity}: {constraint}")]
    Rejected { entity: &'static str, constraint: String },

    #[error("the wallet database is schema v{found} but this build supports v{supported}")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("the wallet database is damaged")]
    Corrupt,

    #[error("another program is holding the wallet database")]
    Busy,

    #[error("the {entity} column {column} holds a value this build doesn't recognize")]
    Decode { entity: &'static str, column: &'static str },

    #[error("the {entity} {column} total overflowed")]
    Overflow { entity: &'static str, column: &'static str },

    #[error("a file operation failed while {context}")]
    Io {
        context: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("the system clock is unusable")]
    Clock(#[from] ClockError),

    #[error("SQLite failed while {context}")]
    Sqlite {
        context: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("the wallet database didn't behave as its schema promises: {context}")]
    Internal { context: &'static str },

    #[error("a storage task didn't finish")]
    Task(#[from] JoinError),
}

impl StoreError {
    pub(crate) fn not_found(entity: &'static str, id: i64) -> Self {
        Self::NotFound { entity, id: id.to_string() }
    }

    pub(crate) const fn decode(entity: &'static str, column: &'static str) -> Self {
        Self::Decode { entity, column }
    }

    pub(crate) const fn io(context: &'static str, source: io::Error) -> Self {
        Self::Io { context, source }
    }

    /// The only place a damaged or locked file is recognized, so every path reports them
    /// the same way.
    pub(crate) fn sqlite(context: &'static str, source: rusqlite::Error) -> Self {
        match source.sqlite_error_code() {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => Self::Corrupt,
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            _ => Self::Sqlite { context, source },
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(source: rusqlite::Error) -> Self {
        Self::sqlite("running a statement", source)
    }
}

// A missing row and a duplicate are answers for the caller. Everything else is a storage
// failure whose message names only the wallet database, with the details left to the
// source chain.
impl From<StoreError> for CoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound { entity, id } => Self::NotFound { entity, id },
            StoreError::AlreadyExists { entity, .. } => Self::AlreadyExists { entity },
            other @ (StoreError::MissingReference { .. }
            | StoreError::Rejected { .. }
            | StoreError::SchemaTooNew { .. }
            | StoreError::Corrupt
            | StoreError::Busy
            | StoreError::Decode { .. }
            | StoreError::Overflow { .. }
            | StoreError::Io { .. }
            | StoreError::Clock(_)
            | StoreError::Sqlite { .. }
            | StoreError::Internal { .. }
            | StoreError::Task(_)) => Self::storage("the wallet database", other),
        }
    }
}

/// Reclassifies a failure with what the statement knows.
pub(crate) trait Classify<T> {
    /// `entity` and `columns` are schema text chosen by the statement, never caller
    /// input.
    fn constrained(self, entity: &'static str, columns: &'static str) -> Result<T, StoreError>;

    /// Reports a foreign-key failure as the parent row `id` not being found. Use it where
    /// the caller named that parent, and before `constrained` on a statement that needs
    /// both.
    fn referring_to(self, entity: &'static str, id: i64) -> Result<T, StoreError>;
}

impl<T> Classify<T> for Result<T, StoreError> {
    fn constrained(self, entity: &'static str, columns: &'static str) -> Self {
        self.map_err(|error| {
            let StoreError::Sqlite { source, .. } = &error else {
                return error;
            };
            match source.sqlite_extended_error_code() {
                Some(ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY) => {
                    StoreError::AlreadyExists { entity, columns }
                }
                Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY) => StoreError::MissingReference { entity },
                Some(ffi::SQLITE_CONSTRAINT_CHECK | ffi::SQLITE_CONSTRAINT_NOTNULL) => {
                    // The name `SQLite` gives the constraint is the tail of its message.
                    let tail = if let rusqlite::Error::SqliteFailure(_, Some(message)) = source {
                        message.split_once(": ").map(|(_, tail)| tail)
                    } else {
                        None
                    };
                    StoreError::Rejected {
                        entity,
                        constraint: tail.unwrap_or("unnamed").to_owned(),
                    }
                }
                _ => error,
            }
        })
    }

    fn referring_to(self, entity: &'static str, id: i64) -> Self {
        self.map_err(|error| {
            if let StoreError::Sqlite { source, .. } = &error
                && source.sqlite_extended_error_code() == Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
            {
                StoreError::not_found(entity, id)
            } else {
                error
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use jelly_core::{ClockError, CoreError, ErrorCode as CoreCode};
    use rusqlite::{ErrorCode, ffi};

    use super::{Classify as _, StoreError};

    fn failure(code: ErrorCode, extended: i32, message: &str) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            ffi::Error { code, extended_code: extended },
            Some(message.to_owned()),
        )
    }

    fn constraint(extended: i32, message: &str) -> Result<(), StoreError> {
        Err(StoreError::from(failure(ErrorCode::ConstraintViolation, extended, message)))
    }

    #[test]
    fn each_constraint_kind_is_told_apart() {
        let unique = constraint(ffi::SQLITE_CONSTRAINT_UNIQUE, "UNIQUE constraint failed: a.b")
            .constrained("wallet", "name")
            .expect_err("unique");
        assert!(
            matches!(unique, StoreError::AlreadyExists { entity: "wallet", columns: "name" }),
            "{unique:?}"
        );

        let primary = constraint(ffi::SQLITE_CONSTRAINT_PRIMARYKEY, "UNIQUE constraint failed")
            .constrained("wallet", "name")
            .expect_err("primary key");
        assert!(matches!(primary, StoreError::AlreadyExists { .. }), "{primary:?}");

        let foreign =
            constraint(ffi::SQLITE_CONSTRAINT_FOREIGNKEY, "FOREIGN KEY constraint failed")
                .constrained("address", "wallet_id")
                .expect_err("foreign key");
        assert!(
            matches!(foreign, StoreError::MissingReference { entity: "address" }),
            "{foreign:?}"
        );

        let check = constraint(
            ffi::SQLITE_CONSTRAINT_CHECK,
            "CHECK constraint failed: kind IN ('seed','imported')",
        )
        .constrained("wallet", "kind")
        .expect_err("check");
        let StoreError::Rejected { entity, constraint } = check else {
            panic!("expected a rejection, got {check:?}");
        };
        assert_eq!(entity, "wallet");
        assert_eq!(constraint, "kind IN ('seed','imported')");
    }

    #[test]
    fn a_foreign_key_failure_can_be_reported_as_the_missing_row() {
        let error = constraint(ffi::SQLITE_CONSTRAINT_FOREIGNKEY, "FOREIGN KEY constraint failed")
            .referring_to("address", 404)
            .expect_err("missing");
        let StoreError::NotFound { entity, id } = error else {
            panic!("expected a missing row, got {error:?}");
        };
        assert_eq!(entity, "address");
        assert_eq!(id, "404");
    }

    #[test]
    fn a_damaged_or_locked_file_is_told_apart_from_other_failures() {
        let corrupt =
            StoreError::from(failure(ErrorCode::DatabaseCorrupt, ffi::SQLITE_CORRUPT, "malformed"));
        assert!(matches!(corrupt, StoreError::Corrupt), "{corrupt:?}");

        let not_a_database =
            StoreError::from(failure(ErrorCode::NotADatabase, ffi::SQLITE_NOTADB, "not a db"));
        assert!(matches!(not_a_database, StoreError::Corrupt), "{not_a_database:?}");

        let busy = StoreError::from(failure(ErrorCode::DatabaseBusy, ffi::SQLITE_BUSY, "busy"));
        assert!(matches!(busy, StoreError::Busy), "{busy:?}");

        let locked =
            StoreError::from(failure(ErrorCode::DatabaseLocked, ffi::SQLITE_LOCKED, "locked"));
        assert!(matches!(locked, StoreError::Busy), "{locked:?}");

        let other = StoreError::from(rusqlite::Error::QueryReturnedNoRows);
        assert!(matches!(other, StoreError::Sqlite { .. }), "{other:?}");
    }

    #[test]
    fn a_missing_row_and_a_duplicate_are_answers_and_the_rest_is_storage() {
        let missing =
            CoreError::from(StoreError::NotFound { entity: "endpoint", id: "x".to_owned() });
        assert_eq!(missing.code(), CoreCode::NotFound);
        let duplicate =
            CoreError::from(StoreError::AlreadyExists { entity: "wallet", columns: "name" });
        assert_eq!(duplicate.code(), CoreCode::AlreadyExists);

        let failures = [
            StoreError::MissingReference { entity: "address" },
            StoreError::Rejected { entity: "wallet", constraint: "kind".to_owned() },
            StoreError::SchemaTooNew { found: 9, supported: 1 },
            StoreError::Corrupt,
            StoreError::Busy,
            StoreError::Decode { entity: "wallet", column: "color" },
            StoreError::Overflow { entity: "balance", column: "amount" },
            StoreError::Io {
                context: "opening the database",
                source: io::Error::other("C:\\Users\\me\\jelly.sqlite is locked"),
            },
            StoreError::Internal { context: "a write returned no row" },
            StoreError::Clock(ClockError::OutOfRange),
        ];
        for failure in failures {
            let shown = failure.to_string();
            let storage = CoreError::from(failure);
            assert_eq!(storage.code(), CoreCode::Storage, "{shown}");
            let message = storage.to_string();
            for fragment in [".sqlite", ":\\", "/"] {
                assert!(!message.contains(fragment), "{fragment} reached the message: {message}");
            }
        }
    }
}
