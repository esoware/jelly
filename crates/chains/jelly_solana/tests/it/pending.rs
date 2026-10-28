use std::sync::Arc;

use jelly_core::{
    Amount, ChainAdapter as _, Clock as _, CursorBytes, CursorScope, CursorStore as _, Deadline,
    ReservationSlot, SignedTx, TxDetection, TxDirection, TxOutcome,
    test_util::{ControlledCursorStore, FailingClock, FakeClock, FakeEventSink, MemoryCursorStore},
};
use jelly_solana::{Method, SolanaAdapter, SolanaConfig};
use serde_json::json;

use crate::common::{
    self, Answer, FakeNode, Fallible, Harness, PEER, native, owner_key, request, statuses,
    stock_usdc, usdc,
};

/// Signs, sends, and leaves the outcome unknown, which is the state every case
/// here starts from.
async fn in_flight(harness: &Harness) -> Fallible<SignedTx> {
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()])?;
    harness.node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));
    harness.adapter.broadcast(&signed).await?;
    harness.drain();
    harness.node.forget_calls();
    Ok(signed)
}

#[tokio::test]
async fn refreshing_an_unchanged_watch_set_reads_no_stored_coverage_again() {
    let node = FakeNode::new();
    node.quiet().unwrap();
    let cursors = Arc::new(ControlledCursorStore::default());
    let adapter = SolanaAdapter::builder(
        node,
        Arc::<ControlledCursorStore>::clone(&cursors),
        Arc::new(FakeClock::default()),
    )
    .build();
    let watched = [common::watched(7, common::OWNER)];
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
async fn a_send_that_landed_is_reported_and_stops_being_followed() {
    let harness = Harness::priced().await.unwrap();
    let signed = in_flight(&harness).await.unwrap();
    harness.node.always(
        Method::GetSignatureStatuses,
        Answer::ok(&statuses(&[Some((common::SLOT - 5, "confirmed", false))])),
    );

    harness.sync().await.unwrap();
    let records = harness.records();
    let record = records.first().expect("a record");
    assert_eq!(record.txid, signed.txid);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.detection, TxDetection::Full);
    assert_eq!(record.amount, Amount::from_base_units(1_000));
    assert_eq!(record.counterparty.as_deref(), Some(PEER));
    assert_eq!(record.height, Some(common::SLOT - 5));
    assert_eq!(record.confirmations, 6);
    assert_eq!(record.fee, None, "the fee the submission recorded is not overwritten");
    assert_eq!(harness.adapter.in_flight(), 0, "a voted transaction needs no more watching");
}

#[tokio::test]
async fn a_send_that_landed_and_failed_is_reported_as_failed() {
    let harness = Harness::priced().await.unwrap();
    in_flight(&harness).await.unwrap();
    harness.node.always(
        Method::GetSignatureStatuses,
        Answer::ok(&statuses(&[Some((common::SLOT, "finalized", true))])),
    );

    harness.sync().await.unwrap();
    let records = harness.records();
    let record = records.first().expect("a record");
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.fee, None, "the fee was charged, so the estimate stands");
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_token_send_that_landed_and_failed_settles_its_own_row_and_reads_as_the_walk_does() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, false).unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    harness.node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));
    harness.adapter.broadcast(&signed).await.unwrap();
    harness.drain();
    harness.node.always(
        Method::GetSignatureStatuses,
        Answer::ok(&statuses(&[Some((common::SLOT, "finalized", true))])),
    );

    harness.sync().await.unwrap();
    let records = harness.records();
    let failure = records
        .iter()
        .find(|record| record.asset == native())
        .expect("the failure, as the signature walk reads it");
    assert_eq!(failure.txid, signed.txid);
    assert_eq!(failure.outcome, TxOutcome::Failed);
    assert_eq!(failure.amount, Amount::ZERO);
    let own_row = records
        .iter()
        .find(|record| record.asset == usdc())
        .expect("the row the submission wrote is settled");
    assert_eq!(own_row.outcome, TxOutcome::Failed);
    assert_eq!(own_row.amount, Amount::ZERO, "a failure never contributes to a total");
    assert_eq!(own_row.fee, Some(Amount::ZERO), "the fee was paid in SOL and is counted once");
    assert_eq!(own_row.rent, Some(Amount::ZERO), "a failed send opened no account");
    assert_eq!(own_row.height, Some(common::SLOT), "it no longer reads as pending");
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_send_seen_only_as_processed_stays_pending_until_a_vote() {
    let harness = Harness::priced().await.unwrap();
    in_flight(&harness).await.unwrap();
    harness.node.always(
        Method::GetSignatureStatuses,
        Answer::ok(&statuses(&[Some((common::SLOT, "processed", false))])),
    );

    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "a processed block can still be skipped");
    assert_eq!(harness.adapter.in_flight(), 1, "so it is still watched");
}

#[tokio::test]
async fn a_send_with_no_status_is_resent_while_its_window_is_open() {
    let harness = Harness::priced().await.unwrap();
    let signed = in_flight(&harness).await.unwrap();
    harness.node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));
    harness.node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));

    harness.sync().await.unwrap();
    let sends = harness.node.calls_to(Method::SendTransaction);
    assert_eq!(sends.len(), 1, "one resend");
    let call = sends.first().expect("a resend");
    assert!(call.contains(r#""skipPreflight":true"#), "{}", call.params);
    assert!(harness.records().is_empty(), "nothing has happened to report");
    assert_eq!(harness.adapter.in_flight(), 1);
}

#[tokio::test]
async fn a_send_whose_window_has_closed_is_reported_dead_with_no_fee() {
    let harness = Harness::priced().await.unwrap();
    in_flight(&harness).await.unwrap();
    harness.node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));
    harness.node.always(Method::GetBlockHeight, Answer::ok(&json!(common::EXPIRY_HEIGHT + 1)));

    harness.sync().await.unwrap();
    let records = harness.records();
    let record = records.first().expect("a record");
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.fee, Some(Amount::ZERO), "nothing was charged");
    assert_eq!(
        record.timestamp,
        Some(harness.clock.now_secs().unwrap()),
        "it is timed at the moment the wallet gave up on it"
    );
    assert_eq!(record.height, None, "it never reached a block");
    assert_eq!(record.amount, Amount::from_base_units(1_000), "what was attempted");
    assert_eq!(harness.adapter.in_flight(), 0);
    assert_eq!(harness.node.count(Method::SendTransaction), 0, "a dead transaction is not resent");
}

#[tokio::test]
async fn a_dead_send_is_left_untimed_when_the_clock_cannot_be_read() {
    let node = FakeNode::new();
    node.priced().unwrap();
    let adapter = SolanaAdapter::builder(
        Arc::<FakeNode>::clone(&node),
        Arc::new(MemoryCursorStore::new()),
        Arc::new(FailingClock),
    )
    .build();
    adapter.set_watch_set(&[common::watched(7, common::OWNER)]).await.unwrap();

    let unsigned = adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));
    adapter.broadcast(&signed).await.unwrap();
    node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));
    node.always(Method::GetBlockHeight, Answer::ok(&json!(common::EXPIRY_HEIGHT + 1)));

    let events = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &events).await.unwrap();
    let records = events.records();
    let record = records.first().expect("a record for the dead send");
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.fee, Some(Amount::ZERO));
    assert_eq!(record.timestamp, None, "an unreadable clock is not the epoch");
    assert_eq!(adapter.in_flight(), 0);
}

#[tokio::test]
async fn the_window_is_judged_on_the_block_height_and_not_the_slot() {
    let harness = Harness::priced().await.unwrap();
    in_flight(&harness).await.unwrap();
    harness.node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));
    // The slot is far past the expiry height, but the block height is not.
    harness.node.always(Method::GetSlot, Answer::ok(&json!(common::EXPIRY_HEIGHT * 2)));
    harness.node.always(Method::GetBlockHeight, Answer::ok(&json!(common::EXPIRY_HEIGHT)));

    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "still in flight at exactly the last valid height");
    assert_eq!(harness.adapter.in_flight(), 1);
}

#[tokio::test]
async fn a_send_is_followed_up_again_after_a_restart() {
    let cursors = Arc::new(MemoryCursorStore::new());
    let signature = {
        let harness = Harness::over(SolanaConfig::default(), Arc::clone(&cursors));
        harness.node.priced().unwrap();
        harness.watch_owner().await.unwrap();
        let signed = in_flight(&harness).await.unwrap();
        signed.txid.value().to_owned()
    };

    let restarted = Harness::over(SolanaConfig::default(), cursors);
    restarted.node.priced().unwrap();
    restarted.watch_owner().await.unwrap();
    assert_eq!(restarted.adapter.in_flight(), 1, "the send was read back in");
    restarted.node.always(
        Method::GetSignatureStatuses,
        Answer::ok(&statuses(&[Some((common::SLOT, "finalized", false))])),
    );

    restarted.sync().await.unwrap();
    let records = restarted.records();
    let record = records.first().expect("a record");
    assert_eq!(record.txid.value(), signature);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(restarted.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_build_that_was_never_sent_is_not_written_out() {
    let cursors = Arc::new(MemoryCursorStore::new());
    {
        let harness = Harness::over(SolanaConfig::default(), Arc::clone(&cursors));
        harness.node.priced().unwrap();
        harness.watch_owner().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
        assert_eq!(harness.adapter.in_flight(), 1, "it is watched in memory");
        harness.sync().await.unwrap();
    }

    let restarted = Harness::over(SolanaConfig::default(), cursors);
    restarted.node.priced().unwrap();
    restarted.watch_owner().await.unwrap();
    assert_eq!(restarted.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_discarded_build_stops_being_followed() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    let tracker = harness.adapter.pending_tracker().expect("a pending tracker");
    tracker.discard(&unsigned).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);

    // A second discard of the same build is harmless.
    tracker.discard(&unsigned).await.unwrap();
}

#[tokio::test]
async fn an_abandoned_build_ages_out_of_the_pending_set() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    harness.node.always(Method::GetBlockHeight, Answer::ok(&json!(common::EXPIRY_HEIGHT + 1)));

    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);
    assert!(harness.records().is_empty(), "an unsent build is not a failed transaction");
}

mod endpoints {
    use std::slice;
    use std::time::Duration;

    use async_trait::async_trait;
    use jelly_solana::{Answered, BroadcastReply, RpcCall, SolanaError, SolanaNode};

    use super::*;

    /// Where the endpoint that answers the status read stands.
    const BEHIND_SLOT: u64 = common::SLOT - 8;
    const BEHIND_HEIGHT: u64 = common::EXPIRY_HEIGHT - 2;

    /// Two endpoints of one pool.
    ///
    /// Every document that asks about signature statuses reaches the one a few
    /// blocks behind, and everything else reaches the one that has passed the
    /// send's last valid block height.
    #[derive(Debug)]
    struct TwoEndpoints {
        ahead: Arc<FakeNode>,
        behind: Arc<FakeNode>,
    }

    impl TwoEndpoints {
        fn serving(&self, calls: &[RpcCall]) -> &FakeNode {
            if calls.iter().any(|call| call.method() == Method::GetSignatureStatuses) {
                &self.behind
            } else {
                &self.ahead
            }
        }
    }

    #[async_trait]
    impl SolanaNode for TwoEndpoints {
        async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
            self.serving(slice::from_ref(call)).call(call).await
        }

        async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
            self.serving(calls).batch(calls).await
        }

        async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
            self.ahead.broadcast(call).await
        }
    }

    /// A node behind the tip has no status for a transaction in a block it hasn't
    /// reached, so its silence says nothing while its own height is still inside the
    /// window.
    #[tokio::test]
    async fn an_endpoints_silence_is_judged_against_its_own_block_height() {
        let ahead = FakeNode::new();
        ahead.priced().unwrap();
        let behind = FakeNode::new();
        behind.priced().unwrap();
        behind.always(Method::GetSlot, Answer::ok(&json!(BEHIND_SLOT)));
        behind.always(Method::GetBlockHeight, Answer::ok(&json!(BEHIND_HEIGHT)));
        behind.always(
            Method::GetSignatureStatuses,
            Answer::ok(&json!({ "context": { "slot": BEHIND_SLOT }, "value": [null] })),
        );
        let clock = Arc::new(FakeClock::default());
        let adapter = SolanaAdapter::builder(
            Arc::new(TwoEndpoints { ahead: Arc::clone(&ahead), behind }),
            Arc::new(MemoryCursorStore::new()),
            Arc::<FakeClock>::clone(&clock),
        )
        .build();
        adapter.set_watch_set(&[common::watched(7, common::OWNER)]).await.unwrap();

        let unsigned = adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        let signed = adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
        ahead.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));
        adapter.broadcast(&signed).await.unwrap();
        ahead.always(Method::GetBlockHeight, Answer::ok(&json!(common::EXPIRY_HEIGHT + 1)));

        let events = FakeEventSink::new();
        adapter.sync(clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();
        let failed: Vec<_> = events
            .records()
            .into_iter()
            .filter(|record| record.outcome == TxOutcome::Failed)
            .collect();
        assert!(
            failed.is_empty() && adapter.in_flight() == 1,
            "expected the send to stay in flight, because the endpoint that had no status for it \
             is at height {BEHIND_HEIGHT}, inside the window ending at {}; it was reported failed \
             as {failed:?} and {} sends are still followed",
            common::EXPIRY_HEIGHT,
            adapter.in_flight()
        );
    }
}

#[tokio::test]
async fn an_address_that_leaves_the_watch_set_takes_its_sends_with_it() {
    let harness = Harness::priced().await.unwrap();
    in_flight(&harness).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);
    harness.watch(&[common::watched(8, common::RECORDED)]).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn unreadable_stored_sends_block_every_build_broadcast_and_pass() {
    let original = Harness::priced().await.unwrap();
    let signed = in_flight(&original).await.unwrap();
    for bytes in [
        b"not json".as_slice(),
        br#"{"version":99,"pending":[]}"#,
        br#"{"version":1,"pending":[{}]}"#,
    ] {
        let cursors = Arc::new(MemoryCursorStore::new());
        cursors
            .reserve(
                CursorScope::Address(7),
                ReservationSlot::SolanaSends,
                CursorBytes::copied(bytes),
            )
            .await
            .unwrap();
        let restarted = Harness::over(SolanaConfig::default(), Arc::clone(&cursors));
        restarted.node.priced().unwrap();
        assert!(restarted.watch_owner().await.is_err());
        assert!(restarted.watch_owner().await.is_err());
        restarted.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap_err();
        restarted.adapter.broadcast(&signed).await.unwrap_err();
        restarted.sync().await.unwrap_err();
        assert!(restarted.node.calls_to(Method::SendTransaction).is_empty());
        assert_eq!(cursors.reserve_count(), 1);
        assert!(
            cursors
                .load_reservation(CursorScope::Address(7), ReservationSlot::SolanaSends)
                .await
                .unwrap()
                .unwrap()
                .with_bytes(|stored| stored == bytes)
        );
    }
}
