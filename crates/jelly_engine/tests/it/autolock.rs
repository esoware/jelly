use std::time::Duration;

use jelly_core::{Amount, AssetId, BalanceUpdate, ChainId, CoreError, WalletEvent};
use jelly_engine::{LockReason, LockState, TxQuery, test_util};

use crate::common::{PASSWORD, harness, record, secret};

const BACKGROUND_WRITES: usize = 100;

#[tokio::test]
async fn background_writes_do_not_hold_the_session_open() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("busy").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.set_auto_lock_minutes(1).await.unwrap();

    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);
    for n in 0..BACKGROUND_WRITES {
        let detected = record(added.id, &format!("background-{n}"));
        sink.emit(WalletEvent::TxDetected(detected)).await;
        // The writes span the whole one-minute window.
        harness.clock.advance(Duration::from_millis(600));
    }
    for _ in 0..BACKGROUND_WRITES {
        events.recv().await.unwrap();
    }

    // None of the writes is user activity, so the window has run out. Whether the idle
    // watcher or this poll closes it, the reason is idle.
    test_util::poll_auto_lock(&harness.engine).await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Idle));

    harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    let page = harness.engine.list_txs(TxQuery { limit: 200, ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.len(), BACKGROUND_WRITES);
}

#[tokio::test]
async fn the_timeout_can_be_retuned_including_from_off() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("tunable").await.unwrap();

    harness.engine.set_auto_lock_minutes(0).await.unwrap();
    harness.clock.advance(Duration::from_secs(3600));
    assert!(
        !test_util::poll_auto_lock(&harness.engine).await,
        "auto-lock is off and must not fire"
    );
    assert!(harness.engine.lock_state_now().is_unlocked());

    harness.engine.set_auto_lock_minutes(1).await.unwrap();
    test_util::poll_auto_lock(&harness.engine).await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Idle));
}

#[tokio::test]
async fn user_activity_restarts_the_countdown() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("touched").await.unwrap();
    harness.engine.set_auto_lock_minutes(1).await.unwrap();

    for _ in 0..4 {
        harness.clock.advance(Duration::from_secs(30));
        harness.engine.touch_session().await.unwrap();
    }
    assert!(!test_util::poll_auto_lock(&harness.engine).await, "every step was inside the window");

    harness.clock.advance(Duration::from_secs(61));
    test_util::poll_auto_lock(&harness.engine).await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Idle));
}

#[tokio::test]
async fn an_expired_session_is_refused_by_the_next_operation() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("expiring").await.unwrap();
    harness.engine.set_auto_lock_minutes(1).await.unwrap();
    harness.clock.advance(Duration::from_secs(61));

    let refused = harness.engine.list_addresses(None).await;
    assert!(matches!(refused, Err(CoreError::Locked)), "{refused:?}");
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Idle));
}

#[tokio::test]
async fn an_explicit_lock_is_not_reported_as_a_timeout() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("explicit").await.unwrap();
    harness.engine.set_auto_lock_minutes(1).await.unwrap();
    harness.engine.lock_wallet().await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Requested));

    // A poll with nothing open must not invent a timeout.
    assert!(!test_util::poll_auto_lock(&harness.engine).await);
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Requested));
}

/// Every pass restates each balance, and the interface re-reads balances whenever one
/// arrives. Neither of those is the user.
#[tokio::test]
async fn refreshing_after_a_background_event_does_not_hold_the_session_open() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("refreshing").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.set_auto_lock_minutes(1).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    for units in 1..=4u128 {
        harness.clock.advance(Duration::from_secs(30));
        sink.emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: added.id,
            asset: AssetId::Native(ChainId::Bitcoin),
            balance: Amount::from_base_units(units),
        }))
        .await;
        while !matches!(events.recv().await.unwrap(), WalletEvent::BalanceChanged(_)) {}
        let _ = harness.engine.list_balances().await;
    }

    // Two minutes passed without user activity against a one-minute timeout.
    test_util::poll_auto_lock(&harness.engine).await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Idle));
}

#[tokio::test]
async fn the_lock_state_channel_reports_every_transition() {
    let harness = harness().await.unwrap();
    let mut states = harness.engine.lock_state();
    assert_eq!(states.borrow_and_update().clone(), LockState::locked(LockReason::Requested));

    let created = harness.seed_wallet("watched").await.unwrap();
    states.changed().await.unwrap();
    assert_eq!(states.borrow_and_update().wallet_id(), Some(created.wallet.id));

    harness.engine.set_auto_lock_minutes(1).await.unwrap();
    harness.clock.advance(Duration::from_secs(61));
    test_util::poll_auto_lock(&harness.engine).await;
    states.changed().await.unwrap();
    assert_eq!(states.borrow_and_update().clone(), LockState::locked(LockReason::Idle));
}
