//! The database file and its connections. One writer and a separate read pool prevent
//! reads from queuing behind batch writes.
//!
//! Reads use deferred transactions so history counts and page rows share a snapshot.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jelly_core::Clock;
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::error::StoreError;
use crate::schema;

const DB_FILE: &str = "jelly.sqlite";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 67_108_864;
const IDLE_READERS: usize = 2;
/// Above the number of statements this crate ships, so the cache never evicts one.
const STATEMENT_CACHE_CAPACITY: usize = 64;

/// Whether the opened file is the one that was there before.
///
/// An unusable file is moved aside rather than deleted, and `moved_to` says where. The
/// replacement starts empty. Balances and history re-sync from the chains, but wallets and
/// their settings, chain state, addresses, contacts, endpoints, and pins remain only in the
/// moved-aside file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    No,
    Rebuilt { moved_to: PathBuf },
}

#[derive(Debug)]
pub(crate) struct ReadPool {
    idle: Mutex<Vec<Connection>>,
    path: PathBuf,
}

impl ReadPool {
    pub(crate) fn read<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let pooled = self.idle.lock().pop();
        let connection = match pooled {
            Some(connection) => connection,
            None => open_reader(&self.path)?,
        };
        // Nothing a read scope runs writes, so the deferred transaction is released
        // when it is dropped.
        let outcome = connection
            .unchecked_transaction()
            .map_err(StoreError::from)
            .and_then(|transaction| work(&transaction));
        let mut idle = self.idle.lock();
        if idle.len() < IDLE_READERS {
            idle.push(connection);
        }
        outcome
    }
}

/// The transaction takes the write lock up front, so a scope that reads before it writes
/// waits out `BUSY_TIMEOUT` rather than failing when its read lock cannot be upgraded.
pub(crate) fn write<T>(
    writer: &mut Connection,
    work: impl FnOnce(&Connection) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = work(&transaction)?;
    transaction.commit()?;
    Ok(outcome)
}

/// Checkpoints the write-ahead log and lets `SQLite` refresh its statistics. Skipping it
/// leaves a full WAL beside the database.
///
/// Idle readers are dropped first because a live reader can hold the log open. Another
/// process reading the file can still refuse the checkpoint. That is logged rather than
/// raised, because the file is consistent either way and the next open replays the log.
pub(crate) fn close(writer: &Connection, readers: &ReadPool) -> Result<(), StoreError> {
    readers.idle.lock().clear();
    writer.execute_batch("PRAGMA optimize;")?;
    let is_busy: bool =
        writer.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    if is_busy {
        tracing::warn!(
            target: "jelly::store",
            "the write-ahead log was left for the next open to replay"
        );
    }
    Ok(())
}

/// Opens or creates `jelly.sqlite` in `directory`, and answers with the writer, the
/// readers, and whether the old file was usable.
pub(crate) fn open(
    directory: &Path,
    clock: &dyn Clock,
) -> Result<(Connection, ReadPool, Recovered), StoreError> {
    fs::create_dir_all(directory)
        .map_err(|error| StoreError::io("creating the data directory", error))?;
    let path = directory.join(DB_FILE);
    // Read up front so a broken clock fails the open rather than the first write.
    let now_unix_secs = clock.now_secs()?;

    let (writer, recovered) = match open_checked(&path) {
        Ok(connection) => (connection, Recovered::No),
        Err(StoreError::Corrupt) => rebuild(&path, now_unix_secs)?,
        Err(other) => return Err(other),
    };
    let mut idle = Vec::with_capacity(IDLE_READERS);
    for _ in 0..IDLE_READERS {
        idle.push(open_reader(&path)?);
    }
    Ok((writer, ReadPool { idle: Mutex::new(idle), path }, recovered))
}

/// A file that fails its integrity check is `Corrupt`, the same as one `SQLite` cannot
/// read. Every early return drops the connection, so the caller can move the file aside.
fn open_checked(path: &Path) -> Result<Connection, StoreError> {
    let mut connection = open_writer(path)?;
    let check: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if !check.eq_ignore_ascii_case("ok") {
        return Err(StoreError::Corrupt);
    }
    schema::create_or_check(&mut connection)?;
    Ok(connection)
}

/// The file and its sidecars move aside under one timestamp so they stay together.
fn rebuild(path: &Path, now_unix_secs: i64) -> Result<(Connection, Recovered), StoreError> {
    let moved_to = path.with_file_name(format!("jelly.damaged.{now_unix_secs}.sqlite"));
    move_aside(path, &moved_to)?;
    for suffix in ["-wal", "-shm"] {
        move_aside(&sidecar(path, suffix), &sidecar(&moved_to, suffix))?;
    }
    tracing::warn!(target: "jelly::store", "the wallet database was unusable and was replaced");
    // A replacement that is itself unusable means the problem is the directory,
    // not the file, and there is nothing left to recover to.
    let connection = open_checked(path)?;
    Ok((connection, Recovered::Rebuilt { moved_to }))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn move_aside(from: &Path, to: &Path) -> Result<(), StoreError> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::io("moving an unusable database aside", error)),
    }
}

/// `synchronous = FULL` rather than the usual WAL advice of NORMAL, because a lost
/// commit can mean a forgotten broadcast or a rewound cursor. The write volume is a
/// handful of rows per sync pass.
fn open_writer(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open(path)
        .map_err(|error| StoreError::sqlite("opening the database", error))?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    Ok(connection)
}

/// `query_only` rather than a read-only handle, so the connection can still recover the
/// write-ahead log but a bug cannot write through the pool.
fn open_reader(path: &Path) -> Result<Connection, StoreError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags)
        .map_err(|error| StoreError::sqlite("opening the database for reading", error))?;
    connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::STATEMENT_CACHE_CAPACITY;
    use crate::sql::shipped_statements;

    #[test]
    fn the_cache_holds_every_statement_this_crate_prepares() {
        let shipped = shipped_statements().len();
        assert!(
            shipped <= STATEMENT_CACHE_CAPACITY,
            "{shipped} statements ship, the cache holds {STATEMENT_CACHE_CAPACITY}"
        );
    }
}
