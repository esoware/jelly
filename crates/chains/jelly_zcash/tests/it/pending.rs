use std::slice::from_ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, CursorBytes, CursorScope, CursorStore as _, Deadline,
    ReservationSlot, RetryPolicy, ScanStream, SendAmount, TxOutcome, WalletEvent,
    test_util::{ControlledCursorStore, FakeEventSink},
};
use zcash_protocol::consensus::{MAIN_NETWORK, NetworkUpgrade, Parameters as _};

use super::common::{Node, assert_refused, key, request, watched};

fn assert_held(error: &ChainError) {
    assert!(
        matches!(error, ChainError::InsufficientFunds { .. }),
        "expected the inputs to be held, got {error:?}"
    );
}

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_does_not_restore_a_settled_send() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let saved = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::ZcashSends)
        .await
        .unwrap()
        .unwrap();
    {
        let mut state = node.state.lock();
        state.height = state.height.saturating_add(42);
    }
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    node.cursors
        .reserve(CursorScope::Address(1), ReservationSlot::ZcashSends, saved)
        .await
        .unwrap();
    let watched = [watched()];
    let (first, second) =
        tokio::join!(adapter.set_watch_set(&watched), adapter.set_watch_set(&watched));
    first.unwrap();
    second.unwrap();
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_reads_no_stored_coverage_again() {
    let node = Node::new().unwrap();
    let cursors = Arc::new(ControlledCursorStore::default());
    let adapter = node.adapter_over(Arc::<ControlledCursorStore>::clone(&cursors));
    let watched = [watched()];
    adapter.set_watch_set(&watched).await.unwrap();
    assert_eq!(cursors.scan_read_count(), 1, "admission validates durable coverage");
    adapter.set_watch_set(&watched).await.unwrap();
    adapter.set_watch_set(&watched).await.unwrap();
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    assert_eq!(
        cursors.scan_read_count(),
        2,
        "unchanged watches must not duplicate the sync scan read"
    );
    let (first, second) =
        tokio::join!(adapter.set_watch_set(&watched), adapter.set_watch_set(&watched));
    first.unwrap();
    second.unwrap();
    assert_eq!(cursors.scan_read_count(), 2);
}

#[tokio::test]
async fn a_failed_read_of_stored_state_can_be_retried_and_blocks_nothing() {
    for is_scan in [false, true] {
        for is_initial in [true, false] {
            let node = Node::new().unwrap();
            let cursors = Arc::new(ControlledCursorStore::default());
            let adapter = node.adapter_over(Arc::<ControlledCursorStore>::clone(&cursors));
            let watched = watched();
            if !is_initial {
                adapter.set_watch_set(from_ref(&watched)).await.unwrap();
                adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
            }
            let failure =
                if is_scan { cursors.scan_failure() } else { cursors.reservation_failure() };
            failure.store(true, Ordering::SeqCst);
            if is_scan && !is_initial {
                adapter.set_watch_set(from_ref(&watched)).await.unwrap();
                adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
            } else {
                adapter.set_watch_set(from_ref(&watched)).await.unwrap_err();
            }
            if is_initial {
                assert!(
                    adapter.build_tx(&request()).await.is_err(),
                    "failed admission must not publish a watch"
                );
            }
            failure.store(false, Ordering::SeqCst);
            let retried = adapter.set_watch_set(&[watched]).await;
            assert!(retried.is_ok(), "temporary read failure must be retryable: {retried:?}");
            adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
            adapter.build_tx(&request()).await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_discarded_build_releases_its_coins() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    assert_held(&adapter.build_tx(&request()).await.unwrap_err());
    adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn ambiguous_broadcasts_survive_discard_and_restart_until_expiry() {
    let node = Node::new().unwrap();
    node.state.lock().has_submission_error = true;
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let error = adapter.broadcast(&signed).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    assert_held(&adapter.build_tx(&request()).await.unwrap_err());
    drop(adapter);
    let adapter = node.adapter().await.unwrap();
    assert_held(&adapter.build_tx(&request()).await.unwrap_err());
    {
        let mut state = node.state.lock();
        state.height = state.height.saturating_add(42);
    }
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let is_failed_send = |event: &WalletEvent| {
        matches!(event, WalletEvent::TxDetected(record)
            if record.txid == signed.txid && record.outcome == TxOutcome::Failed)
    };
    assert!(sink.drain().iter().any(is_failed_send));
    adapter.build_tx(&request()).await.unwrap();
    assert_eq!(node.state.lock().submissions.len(), 1);
}

#[tokio::test]
async fn an_expired_send_reports_no_amount_or_fee() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    assert!(!unsigned.preview.fee.is_zero());
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    {
        let mut state = node.state.lock();
        state.height = state.height.saturating_add(42);
    }
    let events = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &events).await.unwrap();
    let failed = events
        .drain()
        .into_iter()
        .find_map(|event| {
            if let WalletEvent::TxDetected(record) = event
                && record.txid == signed.txid
                && record.outcome == TxOutcome::Failed
            {
                Some(record)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(failed.amount, Amount::ZERO);
    assert_eq!(failed.fee, Some(Amount::ZERO));
}

#[tokio::test]
async fn broadcast_txid_mismatches_remain_reserved() {
    let node = Node::new().unwrap();
    node.state.lock().has_wrong_txid = true;
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let error = adapter.broadcast(&signed).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    let adapter = node.adapter().await.unwrap();
    assert_held(&adapter.quote_fee(&request()).await.unwrap_err());
}

#[tokio::test]
async fn a_broadcast_refused_before_submission_releases_its_coins() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    node.state.lock().block_age_secs = 3_601;
    let error = adapter.broadcast(&signed).await.unwrap_err();
    assert!(node.state.lock().submissions.is_empty());
    let is_released_by_the_engine = matches!(
        &error,
        ChainError::Rejected(rejection)
            if matches!(rejection.retry, RetryPolicy::DidNotLand | RetryPolicy::FinalEverywhere)
    );
    if is_released_by_the_engine {
        adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    }
    node.state.lock().block_age_secs = 0;
    let rebuilt = adapter.build_tx(&request()).await;
    assert!(
        rebuilt.is_ok(),
        "expected the coin of a payment that was never submitted to be spendable again after \
         the refusal {error:?}, but building the payment again failed with {rebuilt:?}"
    );
}

#[tokio::test]
async fn expiry_is_capped_at_network_upgrades_and_rechecked_before_broadcast() {
    let activation = u32::from(MAIN_NETWORK.activation_height(NetworkUpgrade::Nu6_3).unwrap());
    let node = Node::new().unwrap();
    node.state.lock().height = activation.saturating_sub(3);
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    let unsigned = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(unsigned.payload.as_bytes()).unwrap();
    assert_eq!(payload["expiry_height"], activation.saturating_sub(1));
    node.state.lock().height = activation;
    assert_refused(
        &adapter.broadcast(&signed).await.unwrap_err(),
        "This Zcash payment has expired. Build it again.",
    );
    assert!(node.state.lock().submissions.is_empty());
}

#[tokio::test]
async fn unreadable_stored_state_blocks_every_build_broadcast_and_pass_and_is_left_as_it_was() {
    for is_scan in [false, true] {
        for bytes in [b"not json".as_slice(), br#"{"version":99,"pending":[]}"#] {
            let node = Node::new().unwrap();
            let adapter = node.adapter().await.unwrap();
            let unsigned = adapter.build_tx(&request()).await.unwrap();
            let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
            adapter.broadcast(&signed).await.unwrap();
            let saved = node
                .cursors
                .load_reservation(CursorScope::Address(1), ReservationSlot::ZcashSends)
                .await
                .unwrap();
            if is_scan {
                node.cursors
                    .advance(
                        CursorScope::Address(1),
                        ScanStream::ZcashTransactions,
                        CursorBytes::copied(bytes),
                    )
                    .await
                    .unwrap();
            } else {
                node.cursors
                    .reserve(
                        CursorScope::Address(1),
                        ReservationSlot::ZcashSends,
                        CursorBytes::copied(bytes),
                    )
                    .await
                    .unwrap();
            }
            let writes = node.cursors.reserve_count();
            if is_scan {
                adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
                adapter.build_tx(&request()).await.unwrap_err();
                adapter.broadcast(&signed).await.unwrap_err();
            }
            for _ in 0..2 {
                assert!(adapter.set_watch_set(&[watched()]).await.is_err());
                adapter.build_tx(&request()).await.unwrap_err();
                adapter.broadcast(&signed).await.unwrap_err();
                adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
                node.adapter().await.unwrap_err();
            }
            let stored = if is_scan {
                assert_eq!(
                    node.cursors
                        .load_reservation(CursorScope::Address(1), ReservationSlot::ZcashSends)
                        .await
                        .unwrap(),
                    saved
                );
                node.cursors
                    .load_scan(CursorScope::Address(1), ScanStream::ZcashTransactions)
                    .await
                    .unwrap()
            } else {
                node.cursors
                    .load_reservation(CursorScope::Address(1), ReservationSlot::ZcashSends)
                    .await
                    .unwrap()
            };
            assert!(stored.unwrap().with_bytes(|stored| stored == bytes));
            assert_eq!(node.cursors.reserve_count(), writes);
            assert_eq!(node.state.lock().submissions.len(), 1);
        }
    }
}
