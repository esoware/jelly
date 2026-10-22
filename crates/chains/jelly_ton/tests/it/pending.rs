use std::slice::from_ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, Clock as _, CursorBytes, CursorScope, CursorStore as _,
    Deadline, ReservationSlot, ScanStream, TxOutcome,
    test_util::{ControlledCursorStore, FakeEventSink},
};
use serde_json::{Value, json};

use super::common::{Node, assert_refused, key, request, watched};

const BUSY: &str = "Wait for the pending TON payment to finish before sending again.";

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_does_not_restore_a_settled_send() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let saved = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
        .await
        .unwrap()
        .unwrap();
    node.clock.advance(Duration::from_secs(301));
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    node.cursors.reserve(CursorScope::Address(1), ReservationSlot::TonSends, saved).await.unwrap();
    let watched = [watched().unwrap()];
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
    let watched = [watched().unwrap()];
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
            let watched = watched().unwrap();
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
async fn ambiguous_broadcast_survives_restart_and_retries_identical_bytes() {
    let node = Node::new();
    node.state.lock().has_submission_error = true;
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let error = adapter.broadcast(&signed).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    assert_refused(&adapter.build_tx(&request()).await.unwrap_err(), BUSY);
    drop(adapter);
    let restored = node.adapter().await.unwrap();
    assert_refused(&restored.build_tx(&request()).await.unwrap_err(), BUSY);
    let events = FakeEventSink::new();
    restored.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let state = node.state.lock();
    assert_eq!(state.submissions.len(), 2);
    assert_eq!(state.submissions.first(), state.submissions.get(1));
}

#[tokio::test]
async fn a_settled_send_keeps_its_signed_txid_and_releases_the_reservation() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let row: Value = serde_json::from_str(include_str!("../fixtures/settlement.json")).unwrap();
    {
        let mut state = node.state.lock();
        state.position = row.get("transaction_id").unwrap().clone();
        state.seqno = 2;
        state.pages.push_back(json!([row]));
    }
    let events = FakeEventSink::new();
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let records = events.records();
    let record = records.first().unwrap();
    assert_eq!(record.txid, signed.txid);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.amount.base_units(), 10);
    let saved = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.with_bytes(|bytes| bytes == br#"{"version":1,"pending":null}"#));
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn a_send_expires_by_the_nodes_time_and_not_the_local_clock() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    node.clock.advance(Duration::from_secs(301));
    node.state.lock().is_stale = true;
    let events = FakeEventSink::new();
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(events.records().is_empty());
    node.state.lock().is_stale = false;
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(
        events
            .records()
            .iter()
            .any(|record| record.txid == signed.txid && record.outcome == TxOutcome::Failed)
    );
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn an_expired_send_reports_that_it_was_charged_no_fee() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    node.clock.advance(Duration::from_secs(301));
    let events = FakeEventSink::new();
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let expired = events.records().into_iter().find(|record| record.txid == signed.txid).unwrap();
    assert_eq!(expired.outcome, TxOutcome::Failed);
    // The engine stored the broadcast row with the preview's fee, and the store keeps a
    // stored fee whenever a later record carries none.
    assert_eq!(
        expired.fee,
        Some(Amount::ZERO),
        "expected the expired send to report that no fee was charged, but it reports {:?}, \
         so its history row keeps the estimated fee of {} nanogram",
        expired.fee,
        tx.preview.fee.base_units()
    );
}

#[tokio::test]
async fn a_stored_send_whose_amount_was_changed_is_refused_on_restart() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let saved = node
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
        .await
        .unwrap()
        .unwrap();
    let mut document: Value = saved.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    document["pending"]["payment"]["transfer"]["amount_nanogram"] = json!(42);
    node.cursors
        .reserve(
            CursorScope::Address(1),
            ReservationSlot::TonSends,
            CursorBytes::new(serde_json::to_vec(&document).unwrap()),
        )
        .await
        .unwrap();
    let error = node.adapter().await.unwrap_err();
    assert!(matches!(error, ChainError::Internal { .. }), "{error:?}");
}

#[tokio::test]
async fn discard_releases_an_unsent_reservation_and_signing_refuses_an_expired_payment() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    let tx = adapter.build_tx(&request()).await.unwrap();
    node.clock.advance(Duration::from_secs(300));
    assert_refused(
        &adapter.sign_tx(&tx, &[key().unwrap()]).unwrap_err(),
        "This TON payment has expired. Build it again.",
    );
    adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn unreadable_stored_state_blocks_every_build_broadcast_and_pass_and_is_left_as_it_was() {
    for is_scan in [false, true] {
        for bytes in
            [b"not json".as_slice(), br#"{"version":99,"pending":null}"#, br#"{"version":1}"#]
        {
            let node = Node::new();
            let adapter = node.adapter().await.unwrap();
            let unsigned = adapter.build_tx(&request()).await.unwrap();
            let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
            adapter.broadcast(&signed).await.unwrap();
            let saved = node
                .cursors
                .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
                .await
                .unwrap();
            if is_scan {
                node.cursors
                    .advance(
                        CursorScope::Address(1),
                        ScanStream::TonTransactions,
                        CursorBytes::copied(bytes),
                    )
                    .await
                    .unwrap();
            } else {
                node.cursors
                    .reserve(
                        CursorScope::Address(1),
                        ReservationSlot::TonSends,
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
                assert!(adapter.set_watch_set(&[watched().unwrap()]).await.is_err());
                adapter.build_tx(&request()).await.unwrap_err();
                adapter.broadcast(&signed).await.unwrap_err();
                adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
                node.adapter().await.unwrap_err();
            }
            let stored = if is_scan {
                assert_eq!(
                    node.cursors
                        .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
                        .await
                        .unwrap(),
                    saved
                );
                node.cursors
                    .load_scan(CursorScope::Address(1), ScanStream::TonTransactions)
                    .await
                    .unwrap()
            } else {
                node.cursors
                    .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
                    .await
                    .unwrap()
            };
            assert!(stored.unwrap().with_bytes(|stored| stored == bytes));
            assert_eq!(node.cursors.reserve_count(), writes);
            assert_eq!(node.state.lock().submissions.len(), 1);
        }
    }
}
