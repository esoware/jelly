use std::future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, ChainId, ChainSyncProgress, Clock as _, CursorBytes,
    CursorScope, CursorStore as _, ErrorCode, ScanStream, TxDirection, TxOutcome,
    test_util::{
        ControlledCursorStore, FakeClock, FakeEventSink, MemoryCursorStore, NullCursorStore,
        records,
    },
};
use jelly_monero::{
    MoneroAdapter, MoneroConfig,
    test_util::{account_balance, stored_scan},
};
use tokio::sync::Notify;
use tokio::task::yield_now;
use tokio::time::{sleep, timeout};

use crate::common::{
    CHAIN_BLOCKS, ChainBuilder, FakeNode, Harness, HookedNode, PRIMARY_ID, account,
    balance_event_count, balances_announced, base_chain, config, fixture_adapter,
    progress_event_count, recorded, recorded_block, recorded_height, watched,
};

#[tokio::test]
async fn history_lost_with_a_discarded_checkpoint_is_reported_again_on_the_next_pass() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let node = Arc::new(FakeNode::new(height));
    node.serve(vec![recorded_block(&recorded).unwrap()]);
    let cursors = Arc::new(ControlledCursorStore::default());
    let clock = Arc::new(FakeClock::default());
    let adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&node),
        Arc::<ControlledCursorStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();
    let events = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(records(&events.drain()).len(), 1);
    cursors.discard();
    assert!(stored_scan(cursors.committed()).await.unwrap().is_none());

    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let replay = events.drain();
    assert_eq!(
        records(&replay).len(),
        1,
        "lost history must replay from the committed scan, not the live frontier"
    );
    assert!(balance_event_count(&replay) > 0, "the lost balance must replay with its history");
    cursors.commit().await.unwrap();
    let committed = stored_scan(cursors.committed()).await.unwrap().unwrap();

    cursors
        .advance(
            CursorScope::Address(99),
            ScanStream::BitcoinElectrumStatus,
            CursorBytes::copied(b"lost bitcoin history"),
        )
        .await
        .unwrap();
    cursors.discard();
    node.set_tip(height.saturating_add(1));
    node.serve(base_chain(height).unwrap().finish().unwrap());
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(records(&events.drain()).is_empty());
    cursors.commit().await.unwrap();
    assert_ne!(
        stored_scan(cursors.committed()).await.unwrap(),
        Some(committed),
        "a loss on another chain must not permanently refuse Monero advances"
    );
}

#[tokio::test]
async fn a_checkpoint_discarded_on_another_chain_does_not_hold_back_monero_checkpoints() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let node = Arc::new(FakeNode::new(height));
    node.serve(base_chain(height).unwrap().finish().unwrap());
    let cursors = Arc::new(ControlledCursorStore::default());
    let clock = Arc::new(FakeClock::default());
    let adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&node),
        Arc::<ControlledCursorStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();
    let events = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    cursors.commit().await.unwrap();
    let committed = stored_scan(cursors.committed()).await.unwrap().unwrap();
    events.drain();
    cursors
        .advance(
            CursorScope::Address(99),
            ScanStream::BitcoinElectrumStatus,
            CursorBytes::copied(b"lost bitcoin history"),
        )
        .await
        .unwrap();
    cursors.discard();
    node.set_tip(height.saturating_add(1));
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    cursors.commit().await.unwrap();
    assert_ne!(
        stored_scan(cursors.committed()).await.unwrap(),
        Some(committed),
        "a loss on another chain must not permanently refuse Monero advances"
    );
}

#[tokio::test]
async fn a_pass_finds_the_recorded_payment_and_persists_its_progress() {
    let harness = Harness::unsynced(config()).await.unwrap();
    let (progress, events) = harness.sync().await.unwrap();

    assert_eq!(progress.chain(), ChainId::Monero);
    assert_eq!(progress.synced_height(), Some(harness.tip));
    assert_eq!(progress.tip_height(), Some(harness.tip));
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);

    let detected = records(&events);
    assert_eq!(detected.len(), 1, "two outputs of one transaction are one payment");
    let record = detected.first().expect("one record");
    assert_eq!(record.txid.value(), harness.recorded.tx_hash);
    assert_eq!(record.direction, TxDirection::Incoming);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.address_id, PRIMARY_ID);
    assert_eq!(record.fee, None, "a receipt pays no fee");
    assert_eq!(record.amount, Amount::from(harness.recorded.output_amount.saturating_mul(2)));
    assert!(balance_event_count(&events) >= 1);
    assert!(progress_event_count(&events) >= 1);

    let blob = stored_scan(&*harness.cursors).await.unwrap().expect("a checkpoint was written");
    assert!(!blob.is_empty());
}

#[tokio::test]
async fn a_second_pass_reports_nothing_new_and_keeps_its_frontier() {
    let harness = Harness::new().await.unwrap();
    let (progress, events) = harness.sync().await.unwrap();
    assert_eq!(progress.synced_height(), Some(harness.tip));
    assert!(records(&events).is_empty(), "nothing is reported twice");
    assert_eq!(balance_event_count(&events), 0, "an unchanged balance is not an event");
}

#[tokio::test]
async fn the_recorded_payment_is_spendable_only_past_its_lock() {
    // Five confirmations do not clear the ten-block lock.
    let harness = Harness::unsynced(config()).await.unwrap();
    let height = harness.height;
    let total = Amount::from(harness.recorded.output_amount.saturating_mul(2));
    harness.node.set_tip(height.saturating_add(4));
    harness.node.serve(
        ChainBuilder::new([0; 32])
            .recorded(recorded_block(&harness.recorded).unwrap())
            .run(height.saturating_add(1), 4, 1)
            .finish()
            .unwrap(),
    );
    harness.sync().await.unwrap();

    let locked = account_balance(&harness.adapter, 0);
    assert_eq!(locked.total(), total);
    assert_eq!(locked.spendable(), Amount::ZERO, "still inside the ten-block lock");
    assert_eq!(locked.locked(), total);

    // Past the lock, the same coins are spendable.
    harness.node.set_tip(harness.tip);
    harness.node.serve(base_chain(height).unwrap().finish().unwrap());
    harness.sync().await.unwrap();
    let unlocked = account_balance(&harness.adapter, 0);
    assert_eq!(unlocked.spendable(), total);
    assert_eq!(unlocked.locked(), Amount::ZERO);
}

#[tokio::test]
async fn a_hardfork_above_the_ceiling_halts_the_scan_without_committing_progress() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let mut block = recorded_block(&recorded).unwrap();
    block.block.header.hardfork_version = 17;

    let node = Arc::new(FakeNode::new(height.saturating_add(20)));
    node.serve(vec![block]);

    let clock = Arc::new(FakeClock::default());
    let cursors = Arc::new(MemoryCursorStore::new());
    let adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&node),
        Arc::<MemoryCursorStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();

    let events = FakeEventSink::new();
    let deadline = clock.deadline_in(Duration::from_secs(30));
    let error = adapter.sync(deadline, &events).await.expect_err("the ceiling halts the scan");
    assert_eq!(error.code(), ErrorCode::ProtocolTooNew, "{error}");
    assert!(
        matches!(
            error,
            ChainError::ProtocolTooNew { chain: ChainId::Monero, ref version } if version == "17"
        ),
        "{error}"
    );
    assert!(
        stored_scan(&*cursors).await.unwrap().is_none(),
        "nothing past the ceiling is committed"
    );
}

#[tokio::test]
async fn a_wallet_born_above_the_tip_reports_a_finished_restore() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let node = Arc::new(FakeNode::new(height.saturating_sub(1)));
    let clock = Arc::new(FakeClock::default());
    let adapter = MoneroAdapter::builder(
        Arc::<FakeNode>::clone(&node),
        account(&recorded).unwrap(),
        Arc::new(NullCursorStore),
        Arc::<FakeClock>::clone(&clock),
        height,
    )
    .config(config())
    .build()
    .await
    .unwrap();

    let events = FakeEventSink::new();
    let progress = adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(progress.tip_height(), Some(height.saturating_sub(1)));
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
    assert_eq!(node.block_calls(), 0, "there was nothing to fetch");
}

#[tokio::test]
async fn a_restore_wider_than_one_pass_returns_partial_progress_and_resumes_on_the_next() {
    let harness =
        Harness::unsynced(MoneroConfig { blocks_per_pass: 6, batch_blocks: 3, ..config() })
            .await
            .unwrap();

    let (first, _) = harness.sync().await.unwrap();
    assert_eq!(first.synced_height(), Some(harness.height.saturating_add(5)));
    assert!(first.progress_permille() < ChainSyncProgress::MAX_PERMILLE, "a partial pass says so");

    let (second, _) = harness.sync().await.unwrap();
    assert_eq!(second.synced_height(), Some(harness.height.saturating_add(11)));
    assert!(second.progress_permille() > first.progress_permille());
}

#[tokio::test]
async fn a_subaddress_inside_the_window_costs_no_rescan_and_one_outside_forces_one() {
    let harness =
        Harness::with(MoneroConfig { lookahead_major: 4, lookahead_minor: 4, ..config() })
            .await
            .unwrap();
    let inside = harness.account.address(1, 3).unwrap();
    harness
        .adapter
        .set_watch_set(&[watched(PRIMARY_ID, 0, 0, &harness.primary), watched(8, 1, 3, &inside)])
        .await
        .unwrap();
    let (progress, events) = harness.sync().await.unwrap();
    assert_eq!(progress.synced_height(), Some(harness.tip), "the frontier did not move backwards");
    assert!(records(&events).is_empty(), "a rescan would have re-reported the payment");

    let outside = harness.account.address(9, 9).unwrap();
    harness
        .adapter
        .set_watch_set(&[watched(PRIMARY_ID, 0, 0, &harness.primary), watched(9, 9, 9, &outside)])
        .await
        .unwrap();
    let subaddresses = harness.adapter.subaddresses().expect("monero mints subaddresses");
    // Already wide enough now, so this is a no-op rather than a second rescan.
    subaddresses.register_window(9, 9).await.unwrap();

    let (rescanned, events) = harness.sync().await.unwrap();
    assert_eq!(rescanned.synced_height(), Some(harness.tip));
    assert_eq!(
        records(&events).len(),
        1,
        "a rescan re-reports the payment it re-found, which the store folds by id"
    );
}

#[tokio::test]
async fn a_batch_that_skips_a_block_is_refused_rather_than_scanned_past() {
    let harness = Harness::unsynced(config()).await.unwrap();
    let mut blocks = base_chain(harness.height).unwrap().finish().unwrap();
    let hole = blocks.remove(20);
    let missing = u64::try_from(hole.block.number()).unwrap();
    harness.node.serve(blocks);

    let events = FakeEventSink::new();
    let deadline = harness.clock.deadline_in(Duration::from_secs(30));
    let error = harness.adapter.sync(deadline, &events).await.expect_err("a hole in the chain");
    assert_eq!(error.code(), ErrorCode::Rpc, "{error}");
    assert!(error.to_string().contains(&missing.to_string()), "{error}");
    assert_eq!(
        account_balance(&harness.adapter, 0).total(),
        Amount::ZERO,
        "nothing past the hole was scanned"
    );
    assert!(
        stored_scan(&*harness.cursors).await.unwrap().is_none(),
        "and no frontier past it was committed"
    );
}

#[tokio::test]
async fn coins_that_unlock_between_passes_announce_their_new_balance() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let total = Amount::from(recorded.output_amount.saturating_mul(2));

    let harness = Harness::unsynced(config()).await.unwrap();
    harness.node.set_tip(height.saturating_add(4));
    harness.node.serve(
        ChainBuilder::new([0; 32])
            .recorded(recorded_block(&recorded).unwrap())
            .run(height.saturating_add(1), 4, 1)
            .finish()
            .unwrap(),
    );
    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(
        balances_announced(&events, PRIMARY_ID).last(),
        Some(&Amount::ZERO),
        "the payment was announced while still inside its lock"
    );

    harness.node.set_tip(harness.tip);
    harness.node.serve(base_chain(height).unwrap().finish().unwrap());
    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(account_balance(&harness.adapter, 0).spendable(), total);
    assert_eq!(
        balances_announced(&events, PRIMARY_ID).last(),
        Some(&total),
        "the coins unlocked during this pass, so the interface must be told"
    );
}

#[tokio::test(start_paused = true)]
async fn a_pass_its_deadline_stops_returns_the_progress_it_made() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let tip = height.saturating_add(CHAIN_BLOCKS);
    let fake = Arc::new(FakeNode::new(tip));
    fake.serve(base_chain(height).unwrap().finish().unwrap());
    // Each batch costs the same on the adapter's clock and on tokio's paused one,
    // so a timeout wrapped around the pass would also fire at the deadline.
    let clock = Arc::new(FakeClock::default());
    let node = HookedNode::new(fake, {
        let clock = Arc::clone(&clock);
        move |_| {
            clock.advance(Duration::from_millis(100));
            Box::pin(sleep(Duration::from_millis(100)))
        }
    });
    let adapter = fixture_adapter(
        Arc::new(node),
        Arc::new(NullCursorStore),
        Arc::<FakeClock>::clone(&clock),
        MoneroConfig { batch_blocks: 100, ..config() },
    )
    .await
    .unwrap();

    // Thirty-one batches at a tenth of a second each, against half a second.
    let events = FakeEventSink::new();
    let deadline = clock.deadline_in(Duration::from_millis(500));
    let progress = adapter
        .sync(deadline, &events)
        .await
        .expect("a pass its deadline stops reports the progress it made");
    let synced = progress.synced_height().expect("a synced height");
    assert!(synced > height && synced < tip, "synced to {synced} of {height}..={tip}");
}

#[tokio::test]
async fn a_window_widened_during_a_pass_loses_no_payment_below_the_frontier() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let tip = height.saturating_add(CHAIN_BLOCKS);
    let fake = Arc::new(FakeNode::new(tip));
    fake.serve(base_chain(height).unwrap().finish().unwrap());

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let is_armed = Arc::new(AtomicBool::new(false));
    let node = HookedNode::new(Arc::clone(&fake), {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let is_armed = Arc::clone(&is_armed);
        move |_| {
            let should_hold = is_armed.swap(false, Ordering::SeqCst);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Box::pin(async move {
                if should_hold {
                    entered.notify_one();
                    release.notified().await;
                }
            })
        }
    });
    let clock = Arc::new(FakeClock::default());
    let adapter = fixture_adapter(
        Arc::new(node),
        Arc::new(NullCursorStore),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();
    let events = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let held = account_balance(&adapter, 0).total();
    assert!(held > Amount::ZERO, "the payment was found");

    // The chain grows, and the next pass is held inside its block request while a new
    // subaddress outside the window is registered.
    fake.serve(base_chain(height).unwrap().run(tip.saturating_add(1), 10, 1).finish().unwrap());
    fake.set_tip(tip.saturating_add(10));
    is_armed.store(true, Ordering::SeqCst);
    let subaddresses = adapter.subaddresses().expect("monero mints subaddresses");
    let pass = adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events);
    let widen = async {
        entered.notified().await;
        let register = subaddresses.register_window(0, 5);
        tokio::pin!(register);
        let early = timeout(Duration::from_millis(200), &mut register).await;
        release.notify_one();
        match early {
            Ok(registered) => registered,
            Err(_) => register.await,
        }
    };
    let (passed, registered) =
        timeout(Duration::from_secs(20), async { tokio::join!(pass, widen) })
            .await
            .expect("the pass and the registration both finish");
    passed.unwrap();
    registered.unwrap();

    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(
        account_balance(&adapter, 0).total(),
        held,
        "the payment below the frontier is still this wallet's after the window grew"
    );
}

/// Sets its flag when dropped, which is how a test sees a future it cannot reach
/// being cancelled.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_pass_that_fails_leaves_no_block_fetch_running() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let mut block = recorded_block(&recorded).unwrap();
    block.block.header.hardfork_version = 17;
    let fake = Arc::new(FakeNode::new(height.saturating_add(20)));
    fake.serve(vec![block]);

    let is_stalled = Arc::new(AtomicBool::new(false));
    let is_dropped = Arc::new(AtomicBool::new(false));
    let node = HookedNode::new(fake, {
        let is_stalled = Arc::clone(&is_stalled);
        let is_dropped = Arc::clone(&is_dropped);
        move |range| {
            let guard = (*range.start() > height).then(|| {
                is_stalled.store(true, Ordering::SeqCst);
                SetOnDrop(Arc::clone(&is_dropped))
            });
            Box::pin(async move {
                if let Some(_guard) = guard {
                    future::pending::<()>().await;
                }
            })
        }
    });
    let clock = Arc::new(FakeClock::default());
    let adapter = fixture_adapter(
        Arc::new(node),
        Arc::new(NullCursorStore),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();

    let events = FakeEventSink::new();
    let error = timeout(
        Duration::from_secs(5),
        adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events),
    )
    .await
    .expect("a failed pass returns rather than waiting on the stalled request")
    .expect_err("the hardfork ceiling halts the scan");
    assert_eq!(error.code(), ErrorCode::ProtocolTooNew, "{error}");
    for _ in 0..20 {
        yield_now().await;
    }
    // The scan can fail before the runtime first polls the prefetch, and a task
    // aborted unpolled never makes its request, so only a request that started
    // has anything to drop.
    assert!(
        !is_stalled.load(Ordering::SeqCst) || is_dropped.load(Ordering::SeqCst),
        "the block request the failed pass started is still running with nobody to stop it"
    );
}
