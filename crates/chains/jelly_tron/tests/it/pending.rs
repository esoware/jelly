use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, Clock as _, CursorBytes, CursorScope, CursorStore as _,
    Deadline, ReservationSlot, SignedTx, TxDirection, TxOutcome,
    test_util::{
        ControlledCursorStore, FakeClock, FakeEventSink, MemoryCursorStore, RefusingCursorStore,
        records,
    },
};
use jelly_tron::{
    Route, TronAdapter,
    test_util::{OWNER, OWNER_KEY, RECIPIENT},
};

use crate::common::{
    Answer, FakeNode, Fallible, Harness, TRX, key_material, ready, request, sender, watched,
};

/// A head block past the expiration every build in these tests is stamped with.
fn later_head_block() -> Answer {
    Answer::body(
        r#"{"blockID":"000000000515a651af7c721ebc0c55d84f973b989508d8f40ba10affa980fd3e",
            "block_header":{"raw_data":{"number":85305000,"timestamp":1786599999000}}}"#,
    )
}

async fn sent(harness: &Harness) -> Fallible<SignedTx> {
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)])?;
    harness.adapter.broadcast(&signed).await?;
    Ok(signed)
}

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_reads_no_stored_coverage_again() {
    let cursors = Arc::new(ControlledCursorStore::default());
    let adapter = TronAdapter::new(
        sender(),
        Arc::<ControlledCursorStore>::clone(&cursors),
        Arc::new(FakeClock::default()),
    );
    let watched = [watched(1, OWNER)];
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
async fn a_landed_send_is_reported_with_the_fee_the_network_charged() {
    let fake = sender().on(
        Route::TransactionInfoById,
        Answer::body(
            r#"{"id":"aa","blockNumber":85304890,"blockTimeStamp":1786585800000,"fee":265000}"#,
        ),
    );
    let harness = ready(&fake).await.unwrap();
    let signed = sent(&harness).await.unwrap();

    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    let reported = records(&sink.drain());
    let record = reported.first().expect("the send was reported");
    assert_eq!(record.txid, signed.txid);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.amount, Amount::from_base_units(1_000_000));
    assert_eq!(
        record.fee,
        Some(Amount::from_base_units(265_000)),
        "the charged fee, not the estimate"
    );
    assert_eq!(record.height, Some(85_304_890));
    assert_eq!(record.timestamp, Some(1_786_585_800), "milliseconds become seconds");
    assert_eq!(
        harness.adapter.in_flight(),
        0,
        "twenty-four blocks deep is past the nineteen TRON solidifies at, so it is settled"
    );
}

#[tokio::test]
async fn a_landed_send_that_is_not_deep_enough_is_still_followed() {
    let fake = sender().on(
        Route::TransactionInfoById,
        Answer::body(r#"{"id":"aa","blockNumber":85304913,"fee":0}"#),
    );
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    assert_eq!(records(&sink.drain()).len(), 1, "it is reported at every depth");
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "one block deep is not solidified, so the wallet keeps watching"
    );
}

#[tokio::test]
async fn a_landed_failure_moves_nothing_and_still_records_what_it_burned() {
    let fake = sender().on(
        Route::TransactionInfoById,
        Answer::body(
            r#"{"id":"aa","blockNumber":85304800,"fee":345000,"receipt":{"result":"OUT_OF_ENERGY"}}"#,
        ),
    );
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    let reported = records(&sink.drain());
    let record = reported.first().expect("the send was reported");
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.amount, Amount::ZERO, "a failure never contributes to a total");
    assert_eq!(record.fee, Some(Amount::from_base_units(345_000)));
}

#[tokio::test]
async fn a_send_with_no_receipt_that_a_node_still_holds_is_resent_not_rebuilt() {
    let fake = sender()
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionById, Answer::body(r#"{"txID":"aa"}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = sent(&harness).await.unwrap();
    harness.fake.forget_calls();

    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    assert!(records(&sink.drain()).is_empty(), "nothing is reported about a transaction in flight");
    assert_eq!(harness.adapter.in_flight(), 1);
    assert_eq!(harness.fake.count(Route::BroadcastHex), 1, "the identical bytes went out again");
    let resent = harness.fake.last_body(Route::BroadcastHex).expect("a body");
    assert!(
        resent.contains(&hex::encode(signed.raw.as_bytes())),
        "a resend is the same bytes, because a rebuild would be a second transaction"
    );
}

#[tokio::test]
async fn a_send_whose_window_has_closed_and_that_no_node_holds_is_reported_dead() {
    let fake = sender()
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionById, Answer::body("{}"))
        // The build reads the head block the shared fixtures script, and the pass
        // after it reads this later one.
        .on(Route::NowBlock, later_head_block());
    let harness = ready(&fake).await.unwrap();
    let signed = sent(&harness).await.unwrap();
    // An unusable wall clock reads as the epoch here. The record takes its time
    // from the head block that decided the expiry, so a wrong clock can't sink a
    // payment to the bottom of the history.
    harness.clock.set_unix_millis(0);

    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    let reported = records(&sink.drain());
    let record = reported.first().expect("a dead send is reported");
    assert_eq!(record.txid, signed.txid);
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.amount, Amount::ZERO);
    assert_eq!(record.fee, Some(Amount::ZERO), "nothing was charged, because nothing ran");
    assert_eq!(record.height, None);
    assert_eq!(
        record.timestamp,
        Some(1_786_599_999),
        "the head block that decided the expiry, not the signer's clock"
    );
    assert_eq!(harness.adapter.in_flight(), 0, "past its window it can never land");
}

#[tokio::test]
async fn a_send_inside_its_window_that_no_node_holds_yet_is_still_in_flight() {
    let fake = sender()
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionById, Answer::body("{}"));
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    assert!(records(&sink.drain()).is_empty());
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "propagation takes a moment, and the window is still open"
    );
}

#[tokio::test]
async fn a_follow_up_that_will_not_answer_leaves_the_send_alone() {
    let fake = sender().on(Route::TransactionInfoById, Answer::Transport);
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
    assert!(records(&sink.drain()).is_empty());
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "a pass that could not ask has not learned anything"
    );
}

#[tokio::test]
async fn a_send_survives_a_restart_because_it_was_written_before_the_bytes_left() {
    let fake = sender().on(
        Route::TransactionInfoById,
        Answer::body(r#"{"id":"aa","blockNumber":85304900,"fee":265000}"#),
    );
    let harness = ready(&fake).await.unwrap();
    let signed = sent(&harness).await.unwrap();
    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(1), ReservationSlot::TronSends)
        .await
        .unwrap()
        .expect("the send was written");
    assert!(
        stored.with_bytes(|bytes| String::from_utf8_lossy(bytes).contains(signed.txid.value())),
        "the record names the transaction a restart has to ask about"
    );

    // A second adapter over the same store stands in for a restart.
    let restarted = TronAdapter::new(
        Arc::<FakeNode>::clone(&fake),
        Arc::<MemoryCursorStore>::clone(&harness.cursors),
        Arc::new(FakeClock::default()),
    );
    restarted.set_watch_set(&[watched(1, OWNER)]).await.unwrap();
    assert_eq!(restarted.in_flight(), 1, "the send is picked up again");

    let sink = FakeEventSink::new();
    restarted.sync(harness.deadline(), &sink).await.unwrap();
    let reported = records(&sink.drain());
    assert_eq!(reported.first().map(|record| record.txid.clone()), Some(signed.txid.clone()));
}

#[tokio::test]
async fn a_follow_up_runs_before_the_history_walk() {
    let fake = sender().on(
        Route::TransactionInfoById,
        Answer::body(r#"{"id":"aa","blockNumber":85304900,"fee":0}"#),
    );
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    harness.fake.forget_calls();
    let sink = FakeEventSink::new();
    harness.adapter.sync(harness.deadline(), &sink).await.unwrap();

    let calls = harness.fake.calls();
    let receipt = calls
        .iter()
        .position(|call| call.route == Route::TransactionInfoById)
        .expect("the follow-up asked for the receipt");
    let history = calls
        .iter()
        .position(|call| call.route == Route::NativeHistory)
        .expect("the walk read the history");
    assert!(
        receipt < history,
        "a transaction that has just landed is reported from its own receipt rather than \
         waiting for an indexer"
    );
}

#[tokio::test]
async fn a_pass_whose_deadline_has_passed_leaves_the_send_alone() {
    let fake = sender().on(Route::TransactionInfoById, Answer::body("{}"));
    let harness = ready(&fake).await.unwrap();
    let _ = sent(&harness).await.unwrap();
    let sink = FakeEventSink::new();
    harness.adapter.sync(Deadline::EXPIRED, &sink).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);
    assert!(records(&sink.drain()).is_empty());
}

/// An adapter watching [`OWNER`] over `fake`, whose send writes fail once its
/// store is told to refuse them.
async fn unwritable(
    fake: &Arc<FakeNode>,
) -> Fallible<(TronAdapter, Arc<RefusingCursorStore>, Arc<FakeClock>)> {
    let store = Arc::new(RefusingCursorStore::default());
    let clock = Arc::new(FakeClock::default());
    let adapter = TronAdapter::new(
        Arc::<FakeNode>::clone(fake),
        Arc::<RefusingCursorStore>::clone(&store),
        Arc::<FakeClock>::clone(&clock),
    );
    adapter.set_watch_set(&[watched(1, OWNER)]).await?;
    Ok((adapter, store, clock))
}

#[tokio::test]
async fn a_send_whose_record_could_not_be_written_never_goes_out() {
    let fake = sender()
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionById, Answer::body("{}"));
    let (adapter, store, clock) = unwritable(&fake).await.unwrap();
    let unsigned = adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();

    store.reservation_refusal().store(true, Ordering::SeqCst);
    let error = adapter.broadcast(&signed).await.expect_err("an unrecorded send never leaves");
    assert!(!matches!(error, ChainError::Rejected(_)), "{error:?}");
    assert_eq!(fake.count(Route::BroadcastHex), 0, "the failed write stopped the send");

    let sink = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &sink).await.unwrap();
    assert_eq!(
        fake.count(Route::BroadcastHex),
        0,
        "the broadcast reported failure, so no later pass may put these bytes on the network"
    );
}

#[tokio::test]
async fn a_send_whose_record_could_not_be_written_leaves_no_record_when_its_window_closes() {
    let fake = sender()
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionById, Answer::body("{}"))
        .on(Route::NowBlock, later_head_block());
    let (adapter, store, clock) = unwritable(&fake).await.unwrap();
    let unsigned = adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    store.reservation_refusal().store(true, Ordering::SeqCst);
    adapter.broadcast(&signed).await.expect_err("an unrecorded send never leaves");
    store.reservation_refusal().store(false, Ordering::SeqCst);
    fake.forget_calls();

    // The pass reads the later head block.
    let sink = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &sink).await.unwrap();
    assert!(
        records(&sink.drain()).is_empty(),
        "bytes that never reached the network are not a failed transaction"
    );
    assert_eq!(
        fake.count(Route::TransactionInfoById) + fake.count(Route::TransactionById),
        0,
        "nothing is asked about a transaction no endpoint was ever given"
    );
    assert_eq!(adapter.in_flight(), 0, "its window has closed, so nothing is left to follow");
}

#[tokio::test]
async fn unreadable_stored_sends_block_every_build_broadcast_and_pass() {
    let fake = sender();
    let original = ready(&fake).await.unwrap();
    let signed = sent(&original).await.unwrap();
    for bytes in [
        b"not json".as_slice(),
        br#"{"version":99,"pending":[]}"#,
        br#"{"version":1,"pending":[{}]}"#,
    ] {
        let cursors = Arc::new(MemoryCursorStore::new());
        cursors
            .reserve(
                CursorScope::Address(1),
                ReservationSlot::TronSends,
                CursorBytes::copied(bytes),
            )
            .await
            .unwrap();
        let restarted = TronAdapter::new(
            Arc::<FakeNode>::clone(&fake),
            Arc::<MemoryCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&original.clock),
        );
        fake.forget_calls();
        assert!(restarted.set_watch_set(&[watched(1, OWNER)]).await.is_err());
        assert!(restarted.set_watch_set(&[watched(1, OWNER)]).await.is_err());
        restarted.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap_err();
        restarted.broadcast(&signed).await.unwrap_err();
        restarted.sync(original.deadline(), &FakeEventSink::new()).await.unwrap_err();
        assert_eq!(cursors.reserve_count(), 1);
        assert!(
            cursors
                .load_reservation(CursorScope::Address(1), ReservationSlot::TronSends)
                .await
                .unwrap()
                .unwrap()
                .with_bytes(|stored| stored == bytes)
        );
    }
}
