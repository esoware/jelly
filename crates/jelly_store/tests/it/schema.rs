use std::fs;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::sync::Arc;

use jelly_core::{
    ChainId, Clock, ClockError, PassphrasePolicy, ScriptKind, TxDetection, TxDirection, TxOutcome,
    test_util::FailingClock,
};
use jelly_store::{Recovered, Store, StoreError};
use rusqlite::{Connection, ffi};

use crate::common::{self, Fallible};

fn table_sql(connection: &Connection, table: &str) -> Fallible<String> {
    Ok(connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        rusqlite::params![table],
        |row| row.get(0),
    )?)
}

#[tokio::test]
async fn an_unusable_clock_stops_the_open() {
    let scratch = tempfile::tempdir().unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FailingClock);
    let error = Store::open(scratch.path(), clock).await.expect_err("the open is refused");
    assert!(matches!(error, StoreError::Clock(ClockError::BeforeEpoch)), "{error:?}");
}

#[tokio::test]
async fn a_fresh_file_is_stamped_and_journalled() {
    let harness = common::harness().await.unwrap();
    assert_eq!(harness.recovered, Recovered::No);
    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, common::SCHEMA_VERSION);

    let mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");

    let check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0)).unwrap();
    assert_eq!(check, "ok");
}

#[tokio::test]
async fn closing_leaves_no_write_ahead_log_to_replay() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "journal").await.unwrap();
    for n in 0..32 {
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, &format!("bc1qwal{n}"))
            .await
            .unwrap();
    }

    let log = common::sidecar(&common::db_path(&harness.directory), "-wal");
    let filled = fs::metadata(&log).unwrap().len();
    assert!(filled > 0, "the fixture wrote nothing to the log");

    let closed = harness.close().await.unwrap();
    let left = fs::metadata(&log).map_or(0, |metadata| metadata.len());
    assert_eq!(left, 0, "the log was not truncated");

    let reopened = closed.open().await.unwrap();
    assert_eq!(reopened.recovered, Recovered::No);
    assert_eq!(reopened.store.list_wallets().await.unwrap().len(), 1);
    assert_eq!(reopened.store.list_addresses(wallet.id, None).await.unwrap().len(), 32);
}

#[tokio::test]
async fn every_vocabulary_the_schema_pins_is_complete() {
    let harness = common::harness().await.unwrap();
    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    let settings = table_sql(&connection, "wallet_settings").unwrap();
    for policy in PassphrasePolicy::ALL {
        let quoted = format!("'{}'", policy.as_str());
        assert!(settings.contains(&quoted), "wallet_settings.passphrase_policy: {policy:?}");
    }

    let addresses = table_sql(&connection, "addresses").unwrap();
    for script in ScriptKind::ALL {
        let quoted = format!("'{}'", script.as_str());
        assert!(addresses.contains(&quoted), "addresses.script_kind: {script:?}");
    }
    for discriminant in ["derived", "imported", "subaddress"] {
        assert!(addresses.contains(&format!("'{discriminant}'")), "{discriminant}");
    }

    let txs = table_sql(&connection, "txs").unwrap();
    for direction in TxDirection::ALL {
        assert!(txs.contains(&format!("'{}'", direction.as_str())), "{direction:?}");
    }
    for outcome in TxOutcome::ALL {
        assert!(txs.contains(&format!("'{}'", outcome.as_str())), "{outcome:?}");
    }
    for detection in TxDetection::ALL {
        assert!(txs.contains(&format!("'{}'", detection.as_str())), "{detection:?}");
    }
}

#[tokio::test]
async fn the_vocabularies_that_grow_are_not_pinned_by_the_schema() {
    let harness = common::harness().await.unwrap();
    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    for table in [
        "txs",
        "balances",
        "scan_cursors",
        "addresses",
        "contacts",
        "wallet_chain_state",
        "endpoint_overrides",
        "endpoint_health",
        "tls_pins",
    ] {
        let sql = table_sql(&connection, table).unwrap();
        assert!(!sql.contains("'bitcoin'"), "{table} pins the chain list");
        assert!(!sql.contains("native:"), "{table} pins the asset list");
    }
}

#[tokio::test]
async fn the_shipped_schema_enforces_its_checks() {
    let harness = common::harness().await.unwrap();
    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    connection
        .execute("INSERT INTO wallets (name, created_at, updated_at) VALUES ('x', 0, 0)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO addresses \
               (wallet_id, chain, address, key_source, derivation_path, created_at) \
             VALUES (1, 'bitcoin', 'bc1q', 'derived', 'm/0', 0)",
            [],
        )
        .unwrap();

    let short_amount = connection
        .execute(
            "INSERT INTO balances (address_id, asset, amount, updated_at) \
             VALUES (1, 'native:bitcoin', x'0001', 0)",
            [],
        )
        .expect_err("balances.amount accepted a blob that is not 16 bytes");
    assert_eq!(
        short_amount.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{short_amount}"
    );

    let bad_script = connection
        .execute(
            "INSERT INTO addresses \
               (wallet_id, chain, address, key_source, derivation_path, script_kind, created_at) \
             VALUES (1, 'bitcoin', 'bc1qtwo', 'derived', 'm/0', 'p2wsh', 0)",
            [],
        )
        .expect_err("addresses.script_kind accepted a value outside its check");
    assert_eq!(
        bad_script.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{bad_script}"
    );

    let nameless = connection
        .execute(
            "INSERT INTO contacts (wallet_id, chain, address, name, created_at) \
             VALUES (1, 'bitcoin', 'bc1q', '', 0)",
            [],
        )
        .expect_err("contacts.name accepted an empty name");
    assert_eq!(
        nameless.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{nameless}"
    );

    let short_pin = connection
        .execute(
            "INSERT INTO tls_pins (chain, host, spki_sha256, first_seen) \
             VALUES ('bitcoin', 'h', x'0001', 0)",
            [],
        )
        .expect_err("tls_pins.spki_sha256 accepted a blob that is not 32 bytes");
    assert_eq!(
        short_pin.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{short_pin}"
    );
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused_rather_than_downgraded() {
    let harness = common::harness().await.unwrap();
    let closed = harness.close().await.unwrap();
    {
        let connection = Connection::open(common::db_path(&closed.directory)).unwrap();
        connection.pragma_update(None, "user_version", 99i64).unwrap();
    }

    let error =
        common::open_in(&closed.directory, &closed.clock).await.expect_err("the open is refused");
    assert!(
        matches!(
            error,
            StoreError::SchemaTooNew { found: 99, supported }
                if i64::from(supported) == common::SCHEMA_VERSION
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_damaged_file_is_moved_aside_and_replaced() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "before").await.unwrap();
    for n in 0..64 {
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, &format!("bc1q{n}"))
            .await
            .unwrap();
    }
    let closed = harness.close().await.unwrap();
    let path = common::db_path(&closed.directory);

    let page_size: i64 = {
        let connection = Connection::open(&path).unwrap();
        connection.query_row("PRAGMA page_size", [], |row| row.get(0)).unwrap()
    };
    {
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(u64::try_from(page_size).unwrap())).unwrap();
        file.write_all(&[0xAAu8; 512]).unwrap();
        file.sync_all().unwrap();
    }

    let (store, recovered) = common::open_in(&closed.directory, &closed.clock).await.unwrap();
    let Recovered::Rebuilt { moved_to } = recovered else {
        panic!("the damaged file was opened as if it were healthy");
    };
    assert!(moved_to.exists(), "the damaged file was deleted instead of kept");
    let moved_name = moved_to.file_name().and_then(|name| name.to_str()).unwrap();
    assert!(moved_name.starts_with("jelly.damaged."), "{moved_name}");
    assert!(store.list_wallets().await.unwrap().is_empty());

    let fresh = common::wallet(&store, "after").await.unwrap();
    assert!(store.list_addresses(fresh.id, None).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_color_column_accepts_only_canonical_opaque_hex() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "color check").await.unwrap();
    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();
    for color in ["", "red", "#fff", "#12345678", "123456", "#gggggg", "#ABCDEF", "#123456 "] {
        let error = connection
            .execute(
                "UPDATE wallets SET color = ?1 WHERE id = ?2",
                rusqlite::params![color, wallet.id],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_extended_error_code(),
            Some(ffi::SQLITE_CONSTRAINT_CHECK),
            "{color}"
        );
    }
}
