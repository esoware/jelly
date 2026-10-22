use std::slice::from_ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use jelly_core::{
    Amount, ChainAdapter as _, CursorBytes, CursorScope, CursorStore as _, Deadline,
    ReservationSlot, ScanStream, TxOutcome, WalletEvent,
    test_util::{ControlledCursorStore, FakeEventSink},
};

use super::common::{BUSY, Node, assert_refused, key, request, transaction, watched};

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_does_not_restore_a_settled_send() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let saved = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
        .await
        .unwrap()
        .unwrap();
    {
        let mut state = node.state.lock();
        state.ledger = 90_000_021;
        state.has_searched_all = true;
    }
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    node.cursors.reserve(CursorScope::Address(1), ReservationSlot::XrpSends, saved).await.unwrap();
    let watched = [watched()];
    let (first, second) =
        tokio::join!(adapter.set_watch_set(&watched), adapter.set_watch_set(&watched));
    first.unwrap();
    second.unwrap();
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_reads_no_stored_coverage_again() {
    let node = Node::new();
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
            let node = Node::new();
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
async fn ambiguous_submission_survives_restart_and_retries_identical_bytes() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    node.state.lock().has_submission_error = true;
    adapter.broadcast(&signed).await.unwrap_err();
    adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    assert_refused(&adapter.build_tx(&request()).await.unwrap_err(), BUSY);
    drop(adapter);
    let restored = node.adapter().await.unwrap();
    assert_refused(&restored.build_tx(&request()).await.unwrap_err(), BUSY);
    restored.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    assert_eq!(node.state.lock().submissions, vec![hex::encode_upper(signed.raw.as_bytes()); 2]);
}

#[tokio::test]
async fn expiry_needs_a_complete_history_proof() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    node.state.lock().ledger = 90_000_021;
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert_refused(&adapter.build_tx(&request()).await.unwrap_err(), BUSY);
    assert!(sink.drain().iter().all(|event| !matches!(event, WalletEvent::TxDetected(_))));
    node.state.lock().has_searched_all = true;
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert!(sink.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.txid == signed.txid && record.outcome == TxOutcome::Failed)));
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn provisional_claim_costs_are_not_final_until_validated() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    node.state.lock().engine_result = String::from("tecDST_TAG_NEEDED");
    adapter.broadcast(&signed).await.unwrap();
    let mut tx = transaction(signed.txid.value(), "tecDST_TAG_NEEDED", 10_000_000, 9_999_990);
    tx["validated"] = false.into();
    node.state.lock().tx = Some(tx.clone());
    node.state.lock().ledger = 90_000_001;
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert_refused(&adapter.build_tx(&request()).await.unwrap_err(), BUSY);
    tx["validated"] = true.into();
    node.state.lock().tx = Some(tx);
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert!(sink.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.outcome == TxOutcome::Failed && record.amount.is_zero() && record.fee.unwrap().base_units() == 10)));
    let stored = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.with_bytes(|bytes| bytes == br#"{"version":1,"pending":null}"#));
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn a_validated_success_is_recorded_and_releases_the_sequence() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    node.state.lock().ledger = 90_000_001;
    node.state.lock().tx =
        Some(transaction(signed.txid.value(), "tesSUCCESS", 10_000_000, 7_999_990));
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert!(sink.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.outcome == TxOutcome::Success && record.amount.base_units() == 2_000_000 && record.confirmations == 1)));
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn an_expired_send_reports_that_it_was_charged_no_fee() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    node.state.lock().ledger = 90_000_021;
    node.state.lock().has_searched_all = true;
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let expired = sink.records().into_iter().find(|record| record.txid == signed.txid).unwrap();
    assert_eq!(expired.outcome, TxOutcome::Failed);
    // The engine stored the broadcast row with the preview's fee, and the store keeps a
    // stored fee whenever a later record carries none.
    assert_eq!(
        expired.fee,
        Some(Amount::ZERO),
        "expected the expired payment to report that no fee was charged, but it reports {:?}, \
         so its history row keeps the estimated fee of {} drops",
        expired.fee,
        unsigned.preview.fee.base_units()
    );
}

#[tokio::test]
async fn unreadable_stored_state_blocks_every_build_broadcast_and_pass_and_is_left_as_it_was() {
    for is_scan in [false, true] {
        let malformed: &[u8] =
            if is_scan { br#"{"version":1,"next_ledger":"invalid"}"# } else { br#"{"version":1}"# };
        for bytes in [b"not json".as_slice(), br#"{"version":99,"pending":null}"#, malformed] {
            let node = Node::new();
            let adapter = node.adapter().await.unwrap();
            let unsigned = adapter.build_tx(&request()).await.unwrap();
            let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
            adapter.broadcast(&signed).await.unwrap();
            let saved = node
                .cursors
                .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
                .await
                .unwrap();
            if is_scan {
                node.cursors
                    .advance(
                        CursorScope::Address(1),
                        ScanStream::XrpTransactions,
                        CursorBytes::copied(bytes),
                    )
                    .await
                    .unwrap();
            } else {
                node.cursors
                    .reserve(
                        CursorScope::Address(1),
                        ReservationSlot::XrpSends,
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
                        .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
                        .await
                        .unwrap(),
                    saved
                );
                node.cursors
                    .load_scan(CursorScope::Address(1), ScanStream::XrpTransactions)
                    .await
                    .unwrap()
            } else {
                node.cursors
                    .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
                    .await
                    .unwrap()
            };
            assert!(stored.unwrap().with_bytes(|stored| stored == bytes));
            assert_eq!(node.cursors.reserve_count(), writes);
            assert_eq!(node.state.lock().submissions.len(), 1);
        }
    }
}
