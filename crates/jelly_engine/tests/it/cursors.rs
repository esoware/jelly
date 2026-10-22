use std::time::Duration;

use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainId, ChainSyncProgress, CoreError, CursorBytes,
    CursorScope, ReservationSlot, ScanStream, WalletEvent,
};
use jelly_engine::{EngineConfig, TxQuery, test_util};

use crate::common::{PASSWORD, harness, harness_with, record, secret, settled};

const SECRET: ScanStream = ScanStream::MoneroScan;
const PLAIN: ScanStream = ScanStream::BitcoinElectrumStatus;
const SECRET_SLOT: ReservationSlot = ReservationSlot::MoneroSends;
const PLAIN_SLOT: ReservationSlot = ReservationSlot::EvmNonces;
const WALLET: CursorScope = CursorScope::Wallet;
const SEVENTH: CursorScope = CursorScope::Address(7);

fn is_holding(cursor: Option<CursorBytes>, expected: &[u8]) -> bool {
    cursor.is_some_and(|bytes| bytes.with_bytes(|read| read == expected))
}

#[tokio::test]
async fn a_secret_cursor_round_trips_through_the_vault() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Monero]);
    let created = harness.seed_wallet("scanning").await.unwrap();
    let cursors = harness.builder.cursors().expect("a build was recorded");

    assert_eq!(cursors.load_scan(WALLET, SECRET).await.unwrap(), None);
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"scan state")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(cursors.load_scan(WALLET, SECRET).await.unwrap(), b"scan state"));

    // A second write replaces rather than accumulating.
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"further along")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(cursors.load_scan(WALLET, SECRET).await.unwrap(), b"further along"));

    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    let after = harness.builder.cursors().expect("a build was recorded");
    assert!(is_holding(after.load_scan(WALLET, SECRET).await.unwrap(), b"further along"));
}

#[tokio::test]
async fn a_secret_cursor_that_outlived_its_wallet_is_dropped() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Monero]);
    let first = harness.seed_wallet("one").await.unwrap();
    let cursors = harness.builder.cursors().expect("a build was recorded");

    let second = harness.seed_wallet("two").await.unwrap();
    assert_ne!(first.wallet.id, second.wallet.id);
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"scan state")).await.unwrap();
    settled(&harness.engine).await.unwrap();

    let now = harness.builder.cursors().expect("a build was recorded");
    assert_eq!(
        now.load_scan(WALLET, SECRET).await.unwrap(),
        None,
        "one wallet's output keys must not reach another wallet's vault"
    );

    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(first.wallet.id, secret(PASSWORD), None).await.unwrap();
    let back = harness.builder.cursors().expect("a build was recorded");
    assert_eq!(back.load_scan(WALLET, SECRET).await.unwrap(), None, "it was dropped, not misfiled");
}

#[tokio::test]
async fn nothing_of_the_scan_state_reaches_the_database() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[ChainId::Monero]);
    harness.seed_wallet("scanning").await.unwrap();
    let cursors = harness.builder.cursors().expect("a build was recorded");
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"scan state")).await.unwrap();
    cursors.reserve(WALLET, SECRET_SLOT, CursorBytes::copied(b"key images")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(cursors.load_scan(WALLET, SECRET).await.unwrap(), b"scan state"));
    assert!(is_holding(
        cursors.load_reservation(WALLET, SECRET_SLOT).await.unwrap(),
        b"key images"
    ));

    let engine = &harness.engine;
    assert!(test_util::stored_scans(engine, &[WALLET], &[SECRET]).await.unwrap().is_empty());
    assert!(
        test_util::stored_reservations(engine, &[WALLET], &[SECRET_SLOT]).await.unwrap().is_empty()
    );
}

#[tokio::test]
async fn a_scope_and_a_stream_are_both_part_of_the_key() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let cursors = harness.builder.cursors().expect("a build was recorded");

    cursors.reserve(WALLET, SECRET_SLOT, CursorBytes::copied(b"zero")).await.unwrap();
    cursors.reserve(SEVENTH, SECRET_SLOT, CursorBytes::copied(b"seven")).await.unwrap();
    assert!(is_holding(cursors.load_reservation(WALLET, SECRET_SLOT).await.unwrap(), b"zero"));
    assert!(is_holding(cursors.load_reservation(SEVENTH, SECRET_SLOT).await.unwrap(), b"seven"));

    let rows = cursors.load_reservations(&[WALLET, SEVENTH], &[SECRET_SLOT]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.slot == SECRET_SLOT));

    // A stream nobody wrote is absent rather than confused with another.
    assert_eq!(cursors.load_scan(WALLET, SECRET).await.unwrap(), None);
    assert!(cursors.load_reservations(&[], &[SECRET_SLOT]).await.unwrap().is_empty());
    assert!(cursors.load_reservations(&[WALLET], &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn one_read_answers_from_both_sides() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;
    let scope = CursorScope::Address(id);
    let cursors = harness.builder.cursors().expect("a build was recorded");

    cursors.advance(scope, PLAIN, CursorBytes::copied(b"electrum")).await.unwrap();
    cursors.advance(scope, SECRET, CursorBytes::copied(b"scan state")).await.unwrap();
    settled(&harness.engine).await.unwrap();

    let mut rows = cursors.load_scans(&[scope], &[PLAIN, SECRET]).await.unwrap();
    assert_eq!(rows.len(), 2);
    rows.sort_by_key(|row| row.stream);
    assert_eq!(rows.first().map(|row| row.stream), Some(PLAIN));
    assert_eq!(rows.get(1).map(|row| row.stream), Some(SECRET));

    assert!(cursors.load_scans(&[], &[PLAIN]).await.unwrap().is_empty());
    assert!(cursors.load_scans(&[scope], &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_reservation_is_durable_before_it_returns() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let added = harness.engine.add_address(ChainId::Ethereum, None, None).await.unwrap();
    let id = added.id;
    let scope = CursorScope::Address(id);
    let cursors = harness.builder.cursors().expect("a build was recorded");

    cursors.reserve(scope, PLAIN_SLOT, CursorBytes::copied(b"nonce 7")).await.unwrap();
    let rows =
        test_util::stored_reservations(&harness.engine, &[scope], &[PLAIN_SLOT]).await.unwrap();
    assert_eq!(rows.len(), 1, "a reservation is a row before the call returns");
    assert!(rows.first().is_some_and(|row| row.payload.with_bytes(|read| read == b"nonce 7")));

    // A reservation the wallet database refuses is an error the caller sees,
    // which is what stops the send that was about to go out.
    let refused =
        cursors.reserve(CursorScope::Address(987_654), PLAIN_SLOT, CursorBytes::copied(b"x")).await;
    assert!(matches!(refused, Err(CoreError::NotFound { entity: "address", .. })), "{refused:?}");
}

#[tokio::test]
async fn progress_commits_with_the_rows_that_justify_it() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;
    let scope = CursorScope::Address(id);
    let cursors = harness.builder.cursors().expect("a build was recorded");
    let sink = test_util::event_sink(&harness.engine);

    sink.emit(WalletEvent::TxDetected(record(id, "detected"))).await;
    cursors.advance(scope, PLAIN, CursorBytes::copied(b"past detected")).await.unwrap();

    settled(&harness.engine).await.unwrap();
    assert!(is_holding(cursors.load_scan(scope, PLAIN).await.unwrap(), b"past detected"));
    let page = harness.engine.list_txs(TxQuery::default()).await.unwrap();
    assert_eq!(page.rows.len(), 1, "the cursor is durable and so is the row it came with");
}

#[tokio::test]
async fn a_refused_window_advances_no_cursor() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;
    let scope = CursorScope::Address(id);
    let cursors = harness.builder.cursors().expect("a build was recorded");
    let sink = test_util::event_sink(&harness.engine);
    let mut events = harness.engine.subscribe();

    // The database refuses a balance for an address the wallet does not have, and the
    // transaction row and the cursor in the same window go down with it. The window's
    // progress is published whether or not the write landed, so it marks the attempt.
    sink.emit(WalletEvent::BalanceChanged(BalanceUpdate {
        address_id: 987_654,
        asset: AssetId::Native(ChainId::Bitcoin),
        balance: Amount::from_base_units(1),
    }))
    .await;
    sink.emit(WalletEvent::TxDetected(record(id, "refused"))).await;
    cursors.advance(scope, PLAIN, CursorBytes::copied(b"past refused")).await.unwrap();
    sink.emit(WalletEvent::SyncProgress(ChainSyncProgress::new(ChainId::Bitcoin))).await;
    while !matches!(events.recv().await.unwrap(), WalletEvent::SyncProgress(_)) {}

    assert!(
        cursors.load_scans(&[scope], &[PLAIN]).await.unwrap().is_empty(),
        "the cursor cannot outrun a row that was never written"
    );
    assert!(harness.engine.list_txs(TxQuery::default()).await.unwrap().rows.is_empty());
}

#[tokio::test]
async fn a_dropped_window_keeps_critical_checkpoints_and_reservations() {
    let config = EngineConfig {
        write_attempts: 1,
        retry_delay: Duration::from_millis(10),
        event_flush: Duration::from_millis(10),
        ..test_util::config()
    };
    let harness = harness_with(config).await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;
    let scope = CursorScope::Address(id);
    let cursors = harness.builder.cursors().expect("a build was recorded");
    let sink = test_util::event_sink(&harness.engine);

    cursors.advance(scope, PLAIN, CursorBytes::copied(b"earlier")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    cursors.reserve(scope, PLAIN_SLOT, CursorBytes::copied(b"held")).await.unwrap();
    let disposable = ScanStream::SolanaSignatures;
    for stream in [disposable, ScanStream::EvmLogScan, ScanStream::ZcashTransactions] {
        cursors.advance(scope, stream, CursorBytes::copied(b"checkpoint")).await.unwrap();
    }
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(cursors.load_scan(scope, disposable).await.unwrap(), b"checkpoint"));

    sink.emit(WalletEvent::BalanceChanged(BalanceUpdate {
        address_id: 987_654,
        asset: AssetId::Native(ChainId::Bitcoin),
        balance: Amount::from_base_units(1),
    }))
    .await;
    sink.emit(WalletEvent::TxDetected(record(id, "dropped"))).await;

    settled(&harness.engine).await.unwrap();
    assert_eq!(cursors.load_scan(scope, disposable).await.unwrap(), None);
    assert!(is_holding(cursors.load_scan(scope, PLAIN).await.unwrap(), b"earlier"));
    for stream in [ScanStream::EvmLogScan, ScanStream::ZcashTransactions] {
        assert!(is_holding(cursors.load_scan(scope, stream).await.unwrap(), b"checkpoint"));
    }
    assert_eq!(cursors.load_reservations(&[scope], &[PLAIN_SLOT]).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_loss_on_another_chain_refuses_a_secret_cursor_until_its_scope_is_read_again() {
    let config = EngineConfig {
        write_attempts: 1,
        event_flush: Duration::from_millis(10),
        retry_delay: Duration::from_millis(10),
        ..test_util::config()
    };
    let harness = harness_with(config).await.unwrap();
    let created = harness.seed_wallet("cross-chain replay").await.unwrap();
    let address = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let bitcoin = CursorScope::Address(address.id);
    let cursors = test_util::cursors(&harness.engine, created.wallet.id);
    let observer = test_util::cursors(&harness.engine, created.wallet.id);
    let sink = test_util::event_sink(&harness.engine);
    let disposable = ScanStream::SolanaSignatures;
    cursors.load_scan(WALLET, SECRET).await.unwrap();
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"committed secret scan")).await.unwrap();
    cursors.advance(bitcoin, disposable, CursorBytes::copied(b"loss sentinel")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(observer.load_scan(bitcoin, disposable).await.unwrap(), b"loss sentinel"));
    assert!(is_holding(
        observer.load_scan(WALLET, SECRET).await.unwrap(),
        b"committed secret scan"
    ));

    sink.emit(WalletEvent::TxDetected(record(987_654, "invalid bitcoin address"))).await;
    sink.emit(WalletEvent::TxDetected(record(address.id, "lost bitcoin row"))).await;
    settled(&harness.engine).await.unwrap();
    assert_eq!(observer.load_scan(bitcoin, disposable).await.unwrap(), None);
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"stale secret scan")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(
        observer.load_scan(WALLET, SECRET).await.unwrap(),
        b"committed secret scan"
    ));

    assert!(is_holding(cursors.load_scan(WALLET, SECRET).await.unwrap(), b"committed secret scan"));
    sink.emit(WalletEvent::TxDetected(record(address.id, "replayed row"))).await;
    cursors.advance(WALLET, SECRET, CursorBytes::copied(b"replayed secret scan")).await.unwrap();
    settled(&harness.engine).await.unwrap();
    assert!(is_holding(observer.load_scan(WALLET, SECRET).await.unwrap(), b"replayed secret scan"));
    assert_eq!(harness.engine.list_txs(TxQuery::default()).await.unwrap().rows.len(), 1);
    assert!(
        test_util::stored_scans(&harness.engine, &[WALLET], &[SECRET]).await.unwrap().is_empty()
    );
}

#[tokio::test]
async fn a_locked_wallet_has_no_secret_cursors() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("scanning").await.unwrap();
    let cursors = harness.builder.cursors().expect("a build was recorded");
    harness.engine.lock_wallet().await;

    assert!(matches!(cursors.load_scan(WALLET, SECRET).await, Err(CoreError::Locked)));
    assert!(matches!(cursors.load_scans(&[WALLET], &[SECRET]).await, Err(CoreError::Locked)));
    assert!(matches!(
        cursors.load_reservations(&[WALLET], &[SECRET_SLOT]).await,
        Err(CoreError::Locked)
    ));
    assert!(matches!(
        cursors.reserve(WALLET, SECRET_SLOT, CursorBytes::copied(b"x")).await,
        Err(CoreError::Locked)
    ));
}

#[tokio::test]
async fn a_cursor_store_holds_the_engine_weakly() {
    let cursors = {
        let harness = harness().await.unwrap();
        let created = harness.seed_wallet("scanning").await.unwrap();
        let cursors = test_util::cursors(&harness.engine, created.wallet.id);
        cursors.reserve(WALLET, SECRET_SLOT, CursorBytes::copied(b"state")).await.unwrap();
        cursors
    };
    assert!(matches!(cursors.load_scan(WALLET, SECRET).await, Err(CoreError::Locked)));
    assert!(matches!(
        cursors.advance(WALLET, PLAIN, CursorBytes::copied(b"x")).await,
        Err(CoreError::Locked)
    ));
}
