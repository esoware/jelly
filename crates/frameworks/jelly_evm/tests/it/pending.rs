use std::future::pending;
use std::sync::Arc;

use alloy::hex;
use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter as _, ChainId, CursorBytes, CursorScope, CursorStore as _, EventSink,
    ReservationSlot, ScanStream, TxDetection, TxDirection, TxOutcome, WalletEvent, confirmations,
    test_util::{ControlledCursorStore, FakeClock, FakeEventSink, MemoryCursorStore, records},
};
use jelly_evm::{EvmAdapter, Method};

use crate::common::{
    Answer, DEEP_ETHEREUM, ETH, ETHEREUM, FakeNode, Fallible, GWEI, Harness, PEER, SIGNER, Setup,
    TIP, payment, quantity, receipt, receipt_for, sheet, watched,
};

async fn seed_settlement_store(
    harness: &Harness,
    cursors: &ControlledCursorStore,
) -> Fallible<CursorBytes> {
    let baseline = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await?
        .ok_or("baseline")?;
    let reservation = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await?
        .ok_or("reservation")?;
    cursors
        .committed()
        .advance(CursorScope::Address(7), ScanStream::EvmLogScan, baseline.clone())
        .await?;
    cursors.reserve(CursorScope::Address(7), ReservationSlot::EvmNonces, reservation).await?;
    Ok(baseline)
}

fn settlement_adapter(
    harness: &Harness,
    cursors: &Arc<ControlledCursorStore>,
) -> Fallible<EvmAdapter> {
    Ok(EvmAdapter::builder(
        ETHEREUM,
        Arc::<FakeNode>::clone(&harness.node),
        Arc::<ControlledCursorStore>::clone(cursors),
        Arc::<FakeClock>::clone(&harness.clock),
    )
    .config(Setup::default().one_window().config)
    .build()?)
}

async fn settlement_loss(should_restart: bool) -> Fallible<()> {
    let harness = Harness::signer(Setup::default().one_window(), 12).await?;
    harness.node.quiet(&sheet(5 * ETH));
    harness.sync().await?;
    let signed = harness.send(&payment(PEER, 12)).await?;
    let cursors = Arc::new(ControlledCursorStore::default());
    let baseline = seed_settlement_store(&harness, &cursors).await?;
    let build = || settlement_adapter(&harness, &cursors);
    let mut adapter = build()?;
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await?;
    harness.node.quiet(&sheet(4 * ETH - 21_000 * 20 * GWEI));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.always(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(signed.txid.value(), TIP - 100)),
    );
    let events = FakeEventSink::new();
    adapter.sync(harness.deadline(), &events).await?;
    assert!(
        events
            .drain()
            .iter()
            .any(|event| matches!(event, WalletEvent::TxDetected(row) if row.txid == signed.txid)),
        "the initial pass emits the real receipt"
    );
    cursors.discard();
    harness.node.always(Method::GetTransactionReceipt, Answer::Unreachable);
    assert_eq!(
        cursors.committed().load_scan(CursorScope::Address(7), ScanStream::EvmLogScan).await?,
        Some(baseline),
        "queue acceptance must not commit the baseline"
    );
    if should_restart {
        drop(adapter);
        adapter = build()?;
        adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await?;
    }
    adapter.sync(harness.deadline(), &events).await?;
    let replay = events.drain();
    assert!(!replay.iter().any(|event| matches!(event, WalletEvent::TxDetected(row) if row.detection == TxDetection::BalanceDelta)), "settlement must still explain the committed native baseline: {replay:?}");
    assert!(replay.iter().any(|event| matches!(event, WalletEvent::TxDetected(row) if row.txid == signed.txid && row.outcome == TxOutcome::Success)), "the lost real receipt must replay: {replay:?}");
    cursors.commit().await?;
    adapter.sync(harness.deadline(), &events).await?;
    assert!(
        !events.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(_))),
        "committed receipts are not replayed"
    );
    Ok(())
}

#[tokio::test]
async fn a_receipt_whose_checkpoint_was_lost_is_reported_again_in_the_same_session() {
    settlement_loss(false).await.unwrap();
}

#[tokio::test]
async fn a_receipt_whose_checkpoint_was_lost_is_reported_again_after_a_restart() {
    settlement_loss(true).await.unwrap();
}

async fn outstanding_batch_loss(should_restart: bool, is_replaced: bool) -> Fallible<()> {
    let harness = Harness::signer(Setup::default().one_window(), 12).await?;
    harness.node.quiet(&sheet(10 * ETH));
    harness.sync().await?;
    let first = harness.send(&payment(PEER, 12)).await?;
    let second = harness.send(&payment(PEER, 13)).await?;
    let third = harness.send(&payment(PEER, 14)).await?;
    let cursors = Arc::new(ControlledCursorStore::default());
    let baseline = seed_settlement_store(&harness, &cursors).await?;
    let build = || settlement_adapter(&harness, &cursors);
    let mut adapter = build()?;
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await?;
    harness
        .node
        .on(Method::GetTransactionReceipt, Answer::ok(&receipt_for(first.txid.value(), TIP - 100)));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    let events = FakeEventSink::new();
    adapter.sync(harness.deadline(), &events).await?;
    assert!(
        events
            .drain()
            .iter()
            .any(|event| matches!(event, WalletEvent::TxDetected(row) if row.txid == first.txid)),
        "the earlier window includes its settlement"
    );
    assert_eq!(
        cursors.committed().load_scan(CursorScope::Address(7), ScanStream::EvmLogScan).await?,
        Some(baseline),
        "neither window committed"
    );

    if is_replaced {
        // Another wallet holding the same key took both nonces, deep enough to stand.
        harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(15)));
        harness.node.always(Method::GetTransactionByHash, Answer::ok("null"));
    } else {
        for signed in [&second, &third] {
            harness.node.on(
                Method::GetTransactionReceipt,
                Answer::ok(&receipt_for(signed.txid.value(), TIP - 100)),
            );
        }
        harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(15)));
    }
    adapter.sync(harness.deadline(), &events).await?;
    let later = events.drain();
    for signed in [&first, &second, &third] {
        assert!(
            later.iter().any(
                |event| matches!(event, WalletEvent::TxDetected(row) if row.txid == signed.txid)
            ),
            "later window includes every settlement: {later:?}"
        );
    }
    cursors.commit_prefix(1).await?;
    let committed = cursors
        .committed()
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await?
        .ok_or("checkpoint")?;
    let committed: serde_json::Value =
        committed.with_bytes(|bytes| serde_json::from_slice(bytes))?;
    assert_eq!(
        committed.get("settledThrough").and_then(serde_json::Value::as_u64),
        Some(1),
        "only the earlier window committed"
    );
    cursors.discard();
    harness.node.always(Method::GetTransactionReceipt, Answer::Unreachable);
    if should_restart {
        drop(adapter);
        adapter = build()?;
        adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await?;
    }
    adapter.sync(harness.deadline(), &events).await?;
    let replay = events.drain();
    for signed in [&second, &third] {
        let outcome = if is_replaced { TxOutcome::Replaced } else { TxOutcome::Success };
        assert!(replay.iter().any(|event| matches!(event, WalletEvent::TxDetected(row) if row.txid == signed.txid && row.outcome == outcome)), "earlier checkpoint must not acknowledge the later settlement batch: {replay:?}");
    }
    cursors.commit().await?;
    adapter.sync(harness.deadline(), &events).await?;
    assert!(
        !events.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(_))),
        "committed settlements stay quiet"
    );
    let saved = cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await?
        .ok_or("settlements")?;
    let saved: serde_json::Value = saved.with_bytes(|bytes| serde_json::from_slice(bytes))?;
    assert_eq!(
        saved.get("settled"),
        Some(&serde_json::json!([])),
        "loaded coverage cleans the outbox"
    );
    Ok(())
}

#[tokio::test]
async fn receipts_after_the_last_committed_checkpoint_are_reported_again_in_the_same_session() {
    outstanding_batch_loss(false, false).await.unwrap();
}

#[tokio::test]
async fn receipts_after_the_last_committed_checkpoint_are_reported_again_after_a_restart() {
    outstanding_batch_loss(true, false).await.unwrap();
}

#[tokio::test]
async fn replacements_after_the_last_committed_checkpoint_are_reported_again_in_the_same_session() {
    outstanding_batch_loss(false, true).await.unwrap();
}

#[tokio::test]
async fn replacements_after_the_last_committed_checkpoint_are_reported_again_after_a_restart() {
    outstanding_batch_loss(true, true).await.unwrap();
}

#[tokio::test]
async fn a_settled_receipt_explains_the_balance_after_a_restart_and_is_cleared_once_the_nonce_shows_it()
 {
    let harness = Harness::signer(Setup::default().one_window(), 12).await.unwrap();
    harness.node.quiet(&sheet(5 * ETH));
    harness.sync().await.unwrap();
    let signed = harness.send(&payment(PEER, 12)).await.unwrap();
    harness.node.always(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(signed.txid.value(), TIP - 100)),
    );
    harness.sync().await.unwrap();
    harness.sync().await.unwrap();
    let adapter = EvmAdapter::builder(
        ETHEREUM,
        Arc::<FakeNode>::clone(&harness.node),
        Arc::<MemoryCursorStore>::clone(&harness.cursors),
        Arc::<FakeClock>::clone(&harness.clock),
    )
    .config(Setup::default().one_window().config)
    .build()
    .unwrap();
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.quiet(&sheet(4 * ETH - 21_000 * 20 * GWEI));
    harness.node.always(Method::GetTransactionReceipt, Answer::Unreachable);
    let events = FakeEventSink::new();
    adapter.sync(harness.deadline(), &events).await.unwrap();
    assert!(!events.drain().iter().any(|event| matches!(event, WalletEvent::TxDetected(row) if row.detection == TxDetection::BalanceDelta)));
    adapter.sync(harness.deadline(), &events).await.unwrap();
    let saved = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.with_bytes(|bytes| {
        serde_json::from_slice::<serde_json::Value>(bytes).unwrap()["settled"]
            .as_array()
            .unwrap()
            .is_empty()
    }));
}

/// Never makes room for a transaction record, the way a full channel does.
#[derive(Debug)]
struct StalledSink;

#[async_trait]
impl EventSink for StalledSink {
    async fn emit(&self, event: WalletEvent) {
        if matches!(event, WalletEvent::TxDetected(_)) {
            pending::<()>().await;
        }
    }

    fn try_emit(&self, _event: WalletEvent) {}
}

#[tokio::test(start_paused = true)]
async fn a_pass_cut_off_while_it_reports_a_receipt_leaves_the_record_to_the_next_pass() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(4 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));

    let cut_off = harness.adapter.sync(harness.deadline(), &StalledSink).await;
    assert!(cut_off.is_err(), "the pass ran out of budget while it reported");

    harness.sync().await.unwrap();
    let records = harness.records();
    assert!(
        records.iter().any(|record| record.outcome == TxOutcome::Success),
        "a receipt record no sink took is reported by the next pass: {records:?}"
    );
}

#[tokio::test]
async fn a_receipt_is_reported_once_and_then_the_transaction_is_forgotten() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(4 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));

    harness.sync().await.unwrap();
    let drained = harness.drain();
    let record = records(&drained).into_iter().next().expect("the receipt record");
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.amount.base_units(), ETH);
    assert_eq!(record.fee.map(Amount::base_units), Some(65_000 * 20_000_000_000));
    assert_eq!(record.height, Some(18_000_001));
    assert_eq!(record.counterparty.as_deref(), Some(PEER));

    let depth = drained
        .iter()
        .find_map(|event| match event {
            WalletEvent::TxConfirmed(confirmation) => Some(confirmation),
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxDetected(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .expect("a confirmation");
    assert_eq!(depth.confirmations, confirmations(Some(18_000_001), TIP));
    assert_eq!(
        harness.adapter.in_flight(),
        0,
        "a transaction past the confirmation target stops being watched"
    );

    harness.node.forget_calls();
    harness.sync().await.unwrap();
    assert!(
        harness.node.calls_to(Method::GetTransactionReceipt).is_empty(),
        "nothing is watched any more"
    );
}

#[tokio::test]
async fn a_reverted_receipt_records_no_movement_and_a_real_fee() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(4 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("reverted").unwrap()));

    harness.sync().await.unwrap();
    let records = harness.records();
    let record = records.first().expect("the receipt record");
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.amount.base_units(), 0, "a reverted transaction moved nothing");
    assert_eq!(
        record.fee.map(Amount::base_units),
        Some(21_000 * 20_000_000_000),
        "and still paid for the gas it burned"
    );
}

#[tokio::test]
async fn an_inclusion_a_reorg_removed_is_reported_pending_again_and_then_re_recorded() {
    let harness = Harness::sent_with(Setup::on(&DEEP_ETHEREUM).one_window()).await.unwrap();
    harness.node.quiet(&sheet(4 * ETH));

    harness.node.on(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    harness.sync().await.unwrap();
    assert_eq!(harness.records().len(), 1, "reported once");

    // The block it was in is reorganised away.
    harness.node.on(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.sync().await.unwrap();
    let drained = harness.drain();
    let depth = drained
        .iter()
        .find_map(|event| match event {
            WalletEvent::TxConfirmed(confirmation) => Some(confirmation),
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxDetected(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .expect("a confirmation back to zero");
    assert_eq!(depth.confirmations, 0, "the interface is told it is pending again");
    assert_eq!(harness.adapter.in_flight(), 1, "and it is still watched");

    harness
        .node
        .on(Method::GetTransactionReceipt, Answer::ok(&receipt("success_reorganised").unwrap()));
    harness.sync().await.unwrap();
    let records = harness.records();
    assert_eq!(records.len(), 1, "the new inclusion is recorded");
    assert_eq!(records.first().and_then(|record| record.height), Some(18_000_002));
}

/// A node that knows the send, so a pass neither sends it again nor reads its
/// nonce as taken until the script says so.
fn known(harness: &Harness) {
    harness.node.always(Method::GetTransactionByHash, Answer::ok(r#"{"blockNumber":null}"#));
}

#[tokio::test]
async fn a_send_no_receipt_answers_for_is_watched_however_long_that_takes() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    known(&harness);

    for tip in [TIP, TIP + 600, TIP + 10_000_000] {
        harness.node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(tip))));
        harness.sync().await.unwrap();
        assert_eq!(harness.adapter.in_flight(), 1, "no time limit ends a send, at {tip}");
    }
    let records = harness.records();
    assert!(
        records.iter().all(|record| record.outcome != TxOutcome::Replaced),
        "nothing took its nonce: {records:?}"
    );
    assert!(harness.node.calls_to(Method::SendRawTransaction).is_empty(), "every node knew it");
}

#[tokio::test]
async fn a_send_no_node_knows_goes_out_again_as_the_bytes_it_was_signed_as() {
    let harness = Harness::signer(Setup::default().one_window(), 12).await.unwrap();
    let signed = harness.send(&payment(PEER, 12)).await.unwrap();
    harness.node.forget_calls();
    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.always(Method::GetTransactionByHash, Answer::ok("null"));
    harness.node.always(Method::SendRawTransaction, Answer::refused("already known"));

    harness.sync().await.unwrap();
    let sent = harness.node.calls_to(Method::SendRawTransaction);
    let [again] = sent.as_slice() else { panic!("one rebroadcast: {sent:?}") };
    assert_eq!(again.params, format!("[\"0x{}\"]", hex::encode(signed.raw.as_bytes())));
    assert_eq!(harness.adapter.in_flight(), 1, "it is still watched");

    let asked = harness.node.calls_to(Method::GetTransactionByHash);
    assert!(
        asked.iter().all(|call| call.params.contains(signed.txid.value())),
        "the node was asked about the send itself: {asked:?}"
    );
}

#[tokio::test]
async fn the_bytes_a_pass_sends_again_survive_a_restart() {
    let harness = Harness::signer(Setup::default().one_window(), 12).await.unwrap();
    let signed = harness.send(&payment(PEER, 12)).await.unwrap();
    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .expect("a stored set");

    let restarted = Harness::new(Setup::default().one_window()).unwrap();
    restarted
        .cursors
        .reserve(CursorScope::Address(7), ReservationSlot::EvmNonces, stored)
        .await
        .unwrap();
    restarted.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    restarted.node.quiet(&sheet(5 * ETH));
    restarted.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    restarted.node.always(Method::GetTransactionCount, Answer::ok(&quantity(12)));
    restarted.node.always(Method::GetTransactionByHash, Answer::ok("null"));
    restarted
        .node
        .always(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));

    restarted.sync().await.unwrap();
    let sent = restarted.node.calls_to(Method::SendRawTransaction);
    assert_eq!(
        sent.first().map(|call| call.params.clone()),
        Some(format!("[\"0x{}\"]", hex::encode(signed.raw.as_bytes())))
    );
}

/// Another wallet holding the same key sent its own transaction at the nonce this
/// wallet's send took.
#[tokio::test]
async fn a_send_whose_nonce_another_transaction_took_is_replaced_once_that_is_deep() {
    let harness = Harness::sent().await.unwrap();
    let txid = harness.stored_txids().await.unwrap().remove(0);
    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    known(&harness);

    // The other transaction is mined, but not yet as deep as the confirmation
    // target, so the nonce at depth still says twelve.
    harness.node.on(Method::GetTransactionCount, Answer::ok(&quantity(12)));
    harness.node.on(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1, "a nonce taken shallow may still be reorganised");
    assert!(harness.records().is_empty());

    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.forget_calls();
    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0, "the nonce is taken for good");
    let records = harness.records();
    let replaced =
        records.iter().find(|record| record.txid.value() == txid).expect("a replaced record");
    assert_eq!(replaced.outcome, TxOutcome::Replaced);
    assert_eq!(replaced.amount, Amount::ZERO, "it never ran, so it moved nothing");
    assert_eq!(replaced.fee, Some(Amount::ZERO), "and paid nothing");
    assert_eq!(replaced.direction, TxDirection::Outgoing);
    assert_eq!(replaced.counterparty.as_deref(), Some(PEER));

    let deep = harness.node.calls_to(Method::GetTransactionCount);
    assert!(
        deep.iter().any(|call| call.params.contains(&format!("{:#x}", TIP + 1 - 12))),
        "the nonce is read at the confirmation target's depth: {deep:?}"
    );
}

/// The receipt poll and the recheck can reach different nodes. Only the node that
/// says the nonce was taken can say the send did not take it.
#[tokio::test]
async fn a_send_the_rechecking_node_has_a_receipt_for_is_never_replaced() {
    let harness = Harness::sent().await.unwrap();
    let txid = harness.stored_txids().await.unwrap().remove(0);
    harness.node.quiet(&sheet(5 * ETH));
    known(&harness);
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.on(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(Method::GetTransactionReceipt, Answer::ok(&receipt_for(&txid, TIP - 100)));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));

    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1, "the poll's node was behind the recheck's");
    assert!(
        harness.records().iter().all(|record| record.outcome != TxOutcome::Replaced),
        "a send its node holds a receipt for took its own nonce"
    );
}

#[tokio::test]
async fn a_recheck_no_node_answers_leaves_the_send_as_it_was() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.always(Method::GetTransactionByHash, Answer::Unreachable);

    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);
    assert!(harness.node.calls_to(Method::SendRawTransaction).is_empty());
}

/// The other wallet's send spent a coin at the nonce this wallet's send held.
///
/// While that nonce may still go either way, the balance waits. Once it is taken
/// for good, the drop is the other wallet's payment and is recorded as one.
#[tokio::test]
async fn a_balance_another_wallet_spent_at_a_held_nonce_is_judged_once_the_send_is_replaced() {
    let harness = Harness::sent().await.unwrap();
    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    known(&harness);
    harness.sync().await.unwrap();
    harness.drain();

    harness.node.quiet(&sheet(4 * ETH));
    harness.node.on(Method::GetTransactionCount, Answer::ok(&quantity(12)));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.sync().await.unwrap();
    let records = harness.records();
    assert!(
        records.iter().all(|record| record.detection != TxDetection::BalanceDelta),
        "the send at that nonce may still be the one that spent it: {records:?}"
    );

    harness.sync().await.unwrap();
    let records = harness.records();
    let delta = records.iter().find(|record| record.detection == TxDetection::BalanceDelta);
    assert!(
        delta
            .is_some_and(|record| record.direction == TxDirection::Outgoing
                && record.amount.base_units() == ETH),
        "the other wallet's payment is recorded once nothing of this wallet explains it: \
         {records:?}"
    );
}

#[tokio::test]
async fn a_self_send_receipt_is_recorded_as_a_self_transfer() {
    let harness = Harness::signer(Setup::default().one_window(), 12).await.unwrap();
    harness.send(&payment(SIGNER, 12)).await.unwrap();
    harness.drain();

    harness.node.quiet(&sheet(5 * ETH));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    harness.sync().await.unwrap();

    let records = harness.records();
    let record = records.first().expect("the receipt record");
    assert_eq!(
        record.direction,
        TxDirection::SelfTransfer,
        "a coin sent to its own sender never left the address"
    );
}

#[tokio::test]
async fn the_watched_set_survives_a_restart_of_the_adapter() {
    let harness = Harness::sent().await.unwrap();
    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .expect("a stored set");
    assert!(!stored.is_empty());

    let restarted = Harness::new(Setup::on(&DEEP_ETHEREUM).one_window()).unwrap();
    restarted
        .cursors
        .reserve(CursorScope::Address(7), ReservationSlot::EvmNonces, stored.clone())
        .await
        .unwrap();
    restarted.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    assert_eq!(restarted.adapter.in_flight(), 1, "the send survived the restart");

    restarted.node.quiet(&sheet(4 * ETH));
    restarted.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    restarted.sync().await.unwrap();
    assert_eq!(restarted.records().len(), 1, "and its receipt was still asked for");
}

#[tokio::test]
async fn watching_the_same_address_again_does_not_duplicate_its_sends() {
    let harness = Harness::sent().await.unwrap();
    for _ in 0..3u8 {
        harness.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    }
    assert_eq!(harness.adapter.in_flight(), 1);
}

#[tokio::test]
async fn an_address_that_left_the_watch_set_is_forgotten() {
    let harness = Harness::sent().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);
    harness.watch(&[watched(9, ChainId::Ethereum, PEER)]).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn an_unreadable_stored_send_set_blocks_every_operation_and_is_never_overwritten() {
    let original = Harness::signer(Setup::default().one_window(), 12).await.unwrap();
    let signed = original.send(&payment(PEER, 12)).await.unwrap();
    let saved = original
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .unwrap();
    let mut invalid_fee: serde_json::Value =
        saved.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    invalid_fee["entries"][0]["maxFeePerGas"] = serde_json::json!("0");
    invalid_fee["entries"][0]["maxPriorityFeePerGas"] = serde_json::json!("1");
    let invalid_fee = serde_json::to_vec(&invalid_fee).unwrap();
    for bytes in [
        b"not json".as_slice(),
        br#"{"version":99,"entries":[]}"#,
        br#"{"version":1,"entries":[{}]}"#,
        invalid_fee.as_slice(),
    ] {
        let restarted = Harness::new(Setup::default().one_window()).unwrap();
        restarted
            .cursors
            .reserve(CursorScope::Address(7), ReservationSlot::EvmNonces, saved.clone())
            .await
            .unwrap();
        restarted.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
        restarted
            .cursors
            .reserve(
                CursorScope::Address(9),
                ReservationSlot::EvmNonces,
                CursorBytes::copied(bytes),
            )
            .await
            .unwrap();
        let addresses =
            [watched(7, ChainId::Ethereum, SIGNER), watched(9, ChainId::Ethereum, PEER)];
        let (first, second) =
            tokio::join!(restarted.watch(&addresses), restarted.watch(&addresses));
        assert!(first.is_err());
        assert!(second.is_err());
        assert!(restarted.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.is_err());
        restarted.adapter.build_tx(&payment(PEER, 13)).await.unwrap_err();
        restarted.adapter.broadcast(&signed).await.unwrap_err();
        restarted.sync().await.unwrap_err();
        assert!(restarted.node.calls_to(Method::SendRawTransaction).is_empty());
        assert_eq!(restarted.cursors.reserve_count(), 2);
        assert!(
            restarted
                .cursors
                .load_reservation(CursorScope::Address(9), ReservationSlot::EvmNonces)
                .await
                .unwrap()
                .unwrap()
                .with_bytes(|stored| stored == bytes)
        );
        assert_eq!(
            restarted
                .cursors
                .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
                .await
                .unwrap(),
            Some(saved.clone())
        );
    }
}
