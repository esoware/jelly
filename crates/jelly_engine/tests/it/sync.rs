use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{ChainId, ChainSyncProgress, WalletEvent};
use jelly_engine::{EngineConfig, TxQuery, test_util};
use tokio::sync::broadcast;
use tokio::time::{pause, sleep};

use crate::common::{self, Fallible, Harness, drain, harness, harness_with, next_detected};

const CHAIN: ChainId = ChainId::Bitcoin;
const TIP: u64 = 100;

/// A wallet with one watched address on a chain that has an adapter, as the wallet's id
/// and the address's id.
///
/// Unlocking starts a pass, and the address is added only once it has finished.
/// A pass still running could otherwise find the address and sync it alongside the
/// one a test kicks.
async fn watching(harness: &Harness) -> Fallible<(i64, i64)> {
    harness.builder.install(&[CHAIN]);
    let created = harness.seed_wallet("syncing").await?;
    test_util::wait_for_pass(&harness.engine, CHAIN, 0).await;
    let added = harness.engine.add_address(CHAIN, None, None).await?;
    let state = harness.builder.state(CHAIN);
    state.tip.store(TIP, Ordering::SeqCst);
    state.emit_for(added.id);
    Ok((created.wallet.id, added.id))
}

#[tokio::test]
async fn a_pass_persists_what_the_adapter_reports() {
    let harness = harness().await.unwrap();
    let (_, address_id) = watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();

    let before = test_util::sync_passes(&harness.engine, CHAIN);
    assert_eq!(harness.engine.sync_now(Some(CHAIN)).await.unwrap(), vec![CHAIN]);
    next_detected(&mut events).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    let balances = harness.engine.list_balances().await.unwrap();
    assert_eq!(balances.len(), 1);
    assert_eq!(balances.first().map(|row| row.address_id), Some(address_id));

    let page = harness.engine.list_txs(TxQuery { limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows.first().map(|row| row.confirmations), Some(1));

    assert_eq!(test_util::last_sync_failure(&harness.engine, CHAIN), None);
    assert_eq!(test_util::tip_height(&harness.engine, CHAIN), Some(TIP));
}

#[tokio::test]
async fn the_watch_set_hands_the_adapter_the_stored_address() {
    let harness = harness().await.unwrap();
    let (_, address_id) = watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();

    let state = harness.builder.state(CHAIN);
    let watched = state.last_watch_set.lock().clone();
    assert_eq!(watched.len(), 1);
    assert_eq!(watched.first().map(|row| row.id), Some(address_id));
    let stored = harness.engine.list_addresses(Some(CHAIN)).await.unwrap();
    let stored = stored.iter().find(|row| row.id == address_id).expect("the address is listed");
    assert_eq!(
        watched.first().map(|row| row.address.value()),
        Some(stored.address.as_str()),
        "an adapter is handed the stored address and nothing derived"
    );
    assert!(state.watch_set_count() >= 1);
}

/// Every pass hands the adapter its watch set first, so a second pass beside the one
/// that hangs would show up as a second watch set.
#[tokio::test]
async fn concurrent_kicks_never_stack_a_second_pass() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let state = harness.builder.state(CHAIN);
    state.should_hang_sync.store(true, Ordering::SeqCst);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    state.watch_sets_reach(1).await;
    pause();

    let (first, second) =
        tokio::join!(harness.engine.sync_now(Some(CHAIN)), harness.engine.sync_now(Some(CHAIN)));
    first.unwrap();
    second.unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(state.watch_set_count(), 1, "a kick started a pass of its own");
}

#[tokio::test]
async fn a_pass_that_moves_the_tip_reannounces_confirmation_depths() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();
    // That pass announces the first confirmation only if the pump wrote the row before
    // the pass looked, so whatever it announced is set aside.
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
    drain(&mut events);

    // The chain grows a block, and nothing new happens to the recorded transaction.
    // The fake stops reporting new ones, so the announcement cannot depend on whether
    // the pump wrote one before the pass read the store.
    let state = harness.builder.state(CHAIN);
    state.stop_emitting();
    state.tip.store(TIP + 1, Ordering::SeqCst);
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    let announced = confirmations_in(drain(&mut events));
    assert_eq!(announced, vec![2], "the deeper count is announced once, unprompted");

    let page = harness.engine.list_txs(TxQuery { limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.first().map(|row| row.confirmations), Some(2));

    // A pass that finds the tip where it was announces nothing.
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
    let has_repeat =
        drain(&mut events).into_iter().any(|event| matches!(event, WalletEvent::TxConfirmed(_)));
    assert!(!has_repeat, "an unmoved tip is not news");
}

/// A tip that moves many blocks at once, such as the first pass after an unlock, carries
/// a transaction straight past settled.
///
/// It was unsettled at the tip the interface last counted from, so it is still announced.
#[tokio::test]
async fn a_tip_that_jumps_past_settled_announces_what_was_unsettled() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
    drain(&mut events);

    let state = harness.builder.state(CHAIN);
    state.stop_emitting();
    state.tip.store(TIP + 50, Ordering::SeqCst);
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    assert_eq!(confirmations_in(drain(&mut events)), vec![51]);
}

/// Unlocking publishes the session before the adapter set is built, so the interface
/// can read history before any pass has reported a tip.
///
/// Those rows count from the height the last session reached rather than reading as one
/// block deep.
#[tokio::test]
async fn history_read_before_the_adapters_are_built_counts_from_the_stored_height() {
    let harness = harness().await.unwrap();
    let (wallet_id, _) = watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    let state = harness.builder.state(CHAIN);
    state.stop_emitting();
    state.tip.store(TIP + 99, Ordering::SeqCst);
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    harness.engine.lock_wallet().await;
    harness.builder.hold_next_build();
    let engine = harness.engine.clone();
    let unlocking = tokio::spawn(async move {
        engine.unlock_wallet(wallet_id, common::secret(common::PASSWORD), None).await
    });
    harness.builder.build_started().await;

    let page = harness.engine.list_txs(TxQuery { limit: 10, ..Default::default() }).await;
    harness.builder.release();
    unlocking.await.unwrap().unwrap();
    assert_eq!(page.unwrap().rows.first().map(|row| row.confirmations), Some(100));
}

/// The confirmation counts announced among `events`, in order.
fn confirmations_in(events: Vec<WalletEvent>) -> Vec<u32> {
    events
        .into_iter()
        .filter_map(|event| match event {
            WalletEvent::TxConfirmed(confirmation) => Some(confirmation.confirmations),
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxDetected(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn a_failed_pass_records_its_failure() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    harness.builder.state(CHAIN).should_fail_sync.store(true, Ordering::SeqCst);

    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    let failure = test_util::last_sync_failure(&harness.engine, CHAIN).expect("the pass failed");
    assert!(failure.contains("refused"), "{failure}");
}

#[tokio::test]
async fn a_watch_set_failure_fails_the_pass_until_one_succeeds() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let state = harness.builder.state(CHAIN);
    state.should_fail_watch_set.store(true, Ordering::SeqCst);

    for _ in 0..2 {
        let before = test_util::sync_passes(&harness.engine, CHAIN);
        harness.engine.sync_now(Some(CHAIN)).await.unwrap();
        test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
        let failure =
            test_util::last_sync_failure(&harness.engine, CHAIN).expect("watch preparation failed");
        assert!(failure.contains("watch set"), "{failure}");
    }
    assert_eq!(state.sync_count(), 0);

    state.should_fail_watch_set.store(false, Ordering::SeqCst);
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
    assert_eq!(test_util::last_sync_failure(&harness.engine, CHAIN), None);
}

#[tokio::test]
async fn an_adapter_that_never_returns_is_abandoned() {
    let budget = EngineConfig {
        pass_budget: Duration::from_millis(20),
        pass_grace: Duration::from_millis(20),
        ..test_util::config()
    };
    let harness = harness_with(budget).await.unwrap();
    watching(&harness).await.unwrap();
    harness.builder.state(CHAIN).should_hang_sync.store(true, Ordering::SeqCst);

    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    assert_eq!(
        test_util::last_sync_failure(&harness.engine, CHAIN).as_deref(),
        Some("the adapter did not return inside its budget")
    );
}

/// The pass hangs and the shipped watchdog is minutes long, so a lock that waited for
/// the pass would hang the test.
#[tokio::test]
async fn locking_does_not_wait_for_a_pass_in_flight() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let state = harness.builder.state(CHAIN);
    state.should_hang_sync.store(true, Ordering::SeqCst);

    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    // The watch set reaches the adapter just before it hangs.
    state.watch_sets_reach(1).await;

    harness.engine.lock_wallet().await;
    assert!(!harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn a_chain_with_no_addresses_is_already_synced() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("empty").await.unwrap();

    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    assert_eq!(test_util::last_sync_failure(&harness.engine, CHAIN), None);
    assert_eq!(harness.builder.state(CHAIN).sync_count(), 0, "the adapter was not asked");
}

/// Waits for the pump to publish an in-flight progress report, which it does only after
/// writing that report to the board, and hands back everything received on the way.
async fn through_partway(
    events: &mut broadcast::Receiver<WalletEvent>,
) -> Fallible<Vec<WalletEvent>> {
    let mut received = Vec::new();
    loop {
        let event = events.recv().await?;
        let is_partway = matches!(
            event,
            WalletEvent::SyncProgress(progress) if !progress.is_complete()
        );
        received.push(event);
        if is_partway {
            return Ok(received);
        }
    }
}

/// Adapters report the tip they are scanning against while the pass runs, so the board
/// holds the new tip before the pass returns.
#[tokio::test]
async fn a_tip_first_reported_in_flight_is_still_announced() {
    let harness = harness().await.unwrap();
    watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;
    drain(&mut events);

    let state = harness.builder.state(CHAIN);
    state.stop_emitting();
    state.tip.store(TIP + 1, Ordering::SeqCst);
    let partway =
        ChainSyncProgress::new(CHAIN).with_progress(500).with_heights(None, Some(TIP + 1));
    test_util::event_sink(&harness.engine).emit(WalletEvent::SyncProgress(partway)).await;
    let mut received = through_partway(&mut events).await.unwrap();

    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    received.extend(drain(&mut events));
    let announced = confirmations_in(received);
    assert_eq!(announced, vec![2], "the block this pass found was never announced");
}

#[tokio::test]
async fn the_synced_height_survives_a_lock() {
    let harness = harness().await.unwrap();
    let (wallet_id, _) = watching(&harness).await.unwrap();
    let mut events = harness.engine.subscribe();
    let before = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await.unwrap();
    next_detected(&mut events).await.unwrap();
    // The pass can still be recording its height when the transaction is announced.
    test_util::wait_for_pass(&harness.engine, CHAIN, before).await;

    harness.engine.lock_wallet().await;
    // Locking clears the status board.
    harness.engine.unlock_wallet(wallet_id, common::secret(common::PASSWORD), None).await.unwrap();

    assert_eq!(test_util::synced_height(&harness.engine, CHAIN), Some(TIP));
}
