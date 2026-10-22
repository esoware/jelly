use std::fs;
use std::sync::Arc;

use jelly_core::{ChainId, CoreError, CursorScope, ScanStream, SpkiPin, WalletEvent};
use jelly_engine::{TxQuery, test_util};

use crate::common::{self, PASSWORD, harness, harness_in, record, secret};

#[tokio::test]
async fn shutdown_completes_with_the_event_pump_idle() {
    let harness = harness().await.unwrap();
    harness.shut_down().await.unwrap();
}

#[tokio::test]
async fn shutdown_completes_with_a_wallet_open_and_chains_running() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Bitcoin]);
    harness.seed_wallet("running").await.unwrap();
    harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.sync_now(Some(ChainId::Bitcoin)).await.unwrap();

    harness.shut_down().await.unwrap();
}

#[tokio::test]
async fn shutdown_retires_the_adapters_of_the_open_wallet() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Bitcoin]);
    harness.seed_wallet("running").await.unwrap();
    let weak = harness.builder.weak(ChainId::Bitcoin);

    harness.shut_down().await.unwrap();
    assert!(weak.upgrade().is_none(), "an adapter outlived the shutdown");
}

#[tokio::test]
async fn a_written_window_is_on_disk_after_shutdown() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("closing").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();

    let sink = test_util::event_sink(&harness.engine);
    sink.emit(WalletEvent::TxDetected(record(added.id, "last-second"))).await;
    common::next_detected(&mut events).await.unwrap();

    let keystore = Arc::clone(&harness.keystore);
    let scratch = harness.shut_down().await.unwrap();

    // Reopening the same directory proves the row is on disk and the database was
    // closed cleanly.
    let reopened = harness_in(test_util::config(), scratch, keystore).await.unwrap();
    reopened.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert_eq!(reopened.engine.list_txs(TxQuery::default()).await.unwrap().rows.len(), 1);
    reopened.shut_down().await.unwrap();
}

/// The network keeps the pin store and an adapter keeps its cursor port past shutdown,
/// so neither may hold the database open.
#[tokio::test]
async fn shutdown_checkpoints_the_database_while_its_ports_live_on() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("closing").await.unwrap();
    let pins = harness.engine.pin_store();
    let cursors = test_util::cursors(&harness.engine, created.wallet.id);
    let log = harness.scratch.path().join("jelly.sqlite-wal");
    assert!(fs::metadata(&log).unwrap().len() > 0, "nothing was written to the log");

    let _scratch = harness.shut_down().await.unwrap();
    let remaining = fs::metadata(&log).map_or(0, |found| found.len());
    assert_eq!(remaining, 0, "the log was left for the next open to replay");

    let pin = SpkiPin::new([5u8; 32], 6);
    pins.put(ChainId::Bitcoin, "node.example", pin).unwrap();
    assert_eq!(pins.get(ChainId::Bitcoin, "node.example").unwrap(), Some(pin));
    let refused = cursors.load_scan(CursorScope::Wallet, ScanStream::BitcoinElectrumStatus).await;
    assert!(matches!(refused, Err(CoreError::Locked)), "{refused:?}");
}

#[tokio::test]
async fn a_locked_engine_still_shuts_down() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("closing").await.unwrap();
    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    harness.engine.lock_wallet().await;

    harness.shut_down().await.unwrap();
}
