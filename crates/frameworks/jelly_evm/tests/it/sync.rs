use std::collections::HashMap;
use std::slice::from_ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, CursorBytes, CursorScope,
    CursorStore as _, ScanStream, TokenId, TxDetection, TxDirection, TxOutcome, WalletEvent,
    confirmations,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, RefusingCursorStore, TxRequestBuilder, records,
    },
};
use jelly_evm::{Answered, EvmAdapter, EvmConfig, EvmError, EvmNode, EvmParams, Method, RpcCall};
use serde_json::Value;
use tokio::time::sleep;

use crate::common::{
    Answer, BASE, BSC, ETH, ETHEREUM, FakeNode, Fallible, GWEI, Harness, L1_FEE_WEI, OPTIMISM,
    OWNER, PEER, SECOND, SHEET_ENTRIES, SIGNER, Setup, TIP, TIP_HEX, aggregate, evm_fee, fixture,
    quantity, receipt, receipt_for, sheet, sheet_index, sheets_on, watched, word,
};

fn logs(key: &str) -> Fallible<String> {
    fixture("get_logs.json", key)
}

#[tokio::test]
async fn one_pass_reads_the_tip_the_history_and_the_balances_and_stores_a_cursor() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    // The sender-side filter goes first and answers with the recorded window. The
    // recipient-side one answers with nothing.
    harness.node.on(Method::GetLogs, Answer::ok(&logs("result").unwrap()));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    let mut balances = sheet(5 * ETH);
    balances[sheet_index(ETHEREUM, TokenId::UsdtErc20).unwrap()] = Some(2_500_000);
    harness.node.always(Method::Call, Answer::ok(&aggregate(&balances)));

    let progress = harness.sync().await.unwrap();
    assert_eq!(progress.chain(), ChainId::Ethereum);
    assert_eq!(progress.tip_height(), Some(TIP));
    assert_eq!(progress.synced_height(), Some(TIP));

    let drained = harness.drain();
    let balances: Vec<_> = drained
        .iter()
        .filter_map(|event| match event {
            WalletEvent::BalanceChanged(update) => Some(update),
            WalletEvent::TxDetected(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .collect();
    assert_eq!(balances.len(), SHEET_ENTRIES, "one coin and every token");
    assert_eq!(balances.first().map(|update| update.balance.base_units()), Some(5 * ETH));

    let records = records(&drained);
    assert_eq!(
        records.len(),
        2,
        "the duplicate, the foreign contract, the removed log and the positionless one all go"
    );
    let incoming = records.first().expect("the incoming transfer");
    assert_eq!(incoming.direction, TxDirection::Incoming);
    assert_eq!(incoming.asset, AssetId::Token(TokenId::UsdtErc20));
    assert_eq!(incoming.amount.base_units(), 2_500_000);
    assert_eq!(incoming.counterparty.as_deref(), Some(PEER));
    assert_eq!(incoming.height, Some(18_000_000));
    assert_eq!(incoming.timestamp, Some(1_692_000_000));
    assert_eq!(incoming.confirmations, confirmations(Some(18_000_000), TIP));
    assert_eq!(incoming.detection, TxDetection::Full);
    assert_eq!(incoming.address_id, 7);
    assert_eq!(incoming.fee, None, "a log carries no fee");

    let outgoing = records.get(1).expect("the outgoing transfer");
    assert_eq!(outgoing.direction, TxDirection::Outgoing);
    assert_eq!(outgoing.amount.base_units(), 1_000_000);

    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a stored cursor");
    let cursor: Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    assert_eq!(cursor.get("version").and_then(Value::as_u64), Some(1));
    assert_eq!(cursor.get("forwardTo").and_then(Value::as_u64), Some(TIP));
    assert_eq!(
        cursor.get("nativeBalance").and_then(Value::as_u64),
        Some(5_000_000_000_000_000_000),
        "the balance is remembered, so a later change can be told from a cold start"
    );
}

#[tokio::test]
async fn a_window_covers_every_watched_address_in_two_requests() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness
        .watch(&[
            watched(7, ChainId::Ethereum, OWNER),
            watched(8, ChainId::Ethereum, SECOND),
            watched(9, ChainId::Ethereum, PEER),
        ])
        .await
        .unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(ETHEREUM, 3, 0))));

    harness.sync().await.unwrap();
    assert_eq!(
        harness.node.calls_to(Method::GetLogs).len(),
        2,
        "three addresses, one window, two filters"
    );
    let filters = harness.node.calls_to(Method::GetLogs);
    let sending = filters.first().expect("the sender-side filter");
    assert_eq!(
        sending.params.matches("0x000000000000000000000000").count(),
        3,
        "every watched address rides in one topic array: {}",
        sending.params
    );
    assert_eq!(
        harness.node.calls_to(Method::Call).len(),
        1,
        "one aggregate covers twenty-seven balances"
    );
}

#[tokio::test]
async fn a_transfer_between_two_watched_addresses_is_recorded_against_both() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness
        .watch(&[watched(7, ChainId::Ethereum, OWNER), watched(8, ChainId::Ethereum, PEER)])
        .await
        .unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.on(Method::GetLogs, Answer::ok(&logs("result").unwrap()));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(ETHEREUM, 2, 0))));

    harness.sync().await.unwrap();
    let records = harness.records();
    let owner_rows = records.iter().filter(|record| record.address_id == 7).count();
    let peer_rows = records.iter().filter(|record| record.address_id == 8).count();
    assert_eq!(owner_rows, 2, "both transfers name the owner");
    assert_eq!(peer_rows, 2, "and both name the peer");
    assert!(
        records
            .iter()
            .any(|record| record.address_id == 8 && record.direction == TxDirection::Outgoing),
        "the peer sent one of them"
    );
}

#[tokio::test]
async fn a_value_wider_than_the_wallets_base_units_is_dropped_not_truncated() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok(&logs("oversized_value").unwrap()));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(0))));

    harness.sync().await.unwrap();
    assert!(harness.records().is_empty());
}

#[tokio::test]
async fn an_unexplained_native_change_becomes_a_balance_delta_record() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    // The tip has not moved, so the forward walk asks for nothing and only the
    // balance changed.
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(7 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    let delta = records.first().expect("a balance delta record");
    assert_eq!(delta.detection, TxDetection::BalanceDelta);
    assert_eq!(delta.outcome, TxOutcome::Unknown);
    assert_eq!(delta.direction, TxDirection::Incoming);
    assert_eq!(delta.amount.base_units(), 2 * ETH);
    assert_eq!(delta.address_id, 7);
    assert!(delta.txid.value().starts_with("delta:"), "{}", delta.txid);
}

#[tokio::test]
async fn a_first_pass_reports_no_delta_because_there_is_nothing_to_compare_with() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheet(5 * ETH));
    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "a cold cursor is not a change");
}

#[tokio::test]
async fn a_balance_change_a_receipt_explains_yields_no_delta_record() {
    const FEE_WEI: u128 = 65_000 * 20 * GWEI;
    let harness = Harness::sent().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    // The one coin send lands at 65,000 gas and 20 gwei, which accounts for the
    // balance change the same pass sees.
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(4 * ETH - FEE_WEI))));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert!(
        records.iter().all(|record| record.detection == TxDetection::Full),
        "the receipt explains the change: {records:?}"
    );
}

/// Base takes the L1 data fee its receipt reports beside the gas, so a send that
/// landed explains the whole drop and leaves no stray delta.
#[tokio::test]
async fn an_op_stack_send_explains_its_l1_data_fee_and_yields_no_delta_record() {
    const GAS_FEE_WEI: u128 = 21_000 * 20 * GWEI;
    let harness = Harness::new(Setup::on(BASE).one_window()).unwrap();
    harness.watch(&[watched(7, ChainId::Base, SIGNER)]).await.unwrap();
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(12)));
    harness.node.always(Method::GetBalance, Answer::ok(&quantity(5 * ETH)));
    harness.node.on(Method::Call, Answer::ok(&word(L1_FEE_WEI)));
    let payment = TxRequestBuilder::new(7, PEER, BASE.native(), Amount::from_base_units(ETH))
        .fee(evm_fee(Some(21_000), Some(30 * GWEI), Some(GWEI)))
        .nonce(12)
        .build();
    let signed = harness.send(&payment).await.unwrap();
    harness.node.forget_calls();
    harness.drain();

    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(BASE, 1, 5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    let landed = receipt_for(signed.txid.value(), TIP - 50).replacen(
        '{',
        &format!(r#"{{"l1Fee":"{L1_FEE_WEI:#x}","#),
        1,
    );
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&landed));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    let left = 4 * ETH - GAS_FEE_WEI - L1_FEE_WEI;
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(BASE, 1, left))));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert!(!records.is_empty(), "the landed send is recorded");
    assert!(
        records.iter().all(|record| record.detection == TxDetection::Full),
        "the receipt's gas and L1 data fee explain the change: {records:?}"
    );
}

#[tokio::test]
async fn an_incoming_payment_in_the_pass_a_send_lands_is_still_recorded() {
    const FEE_WEI: u128 = 65_000 * 20 * GWEI;
    let harness = Harness::sent().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    // The one coin send lands at 65,000 gas and 20 gwei, and a peer pays the
    // address two coins in the same interval.
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH - FEE_WEI))));
    harness.sync().await.unwrap();

    let records = harness.records();
    let delta = records.iter().find(|record| record.detection == TxDetection::BalanceDelta);
    assert!(
        delta.is_some_and(|record| record.direction == TxDirection::Incoming
            && record.amount.base_units() == 2 * ETH),
        "the receipt explains its own value and fee and nothing more: {records:?}"
    );
}

#[tokio::test]
async fn a_send_reported_in_an_earlier_pass_does_not_hide_a_later_native_change() {
    let harness = Harness::sent().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    let inside_the_window = receipt_for(&format!("0x{}", "22".repeat(32)), TIP - 1);
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&inside_the_window));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(4 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    // Nothing new lands, and a peer pays the address two coins.
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    let delta = records.iter().find(|record| record.detection == TxDetection::BalanceDelta);
    assert!(
        delta.is_some_and(|record| record.direction == TxDirection::Incoming
            && record.amount.base_units() == 2 * ETH),
        "a receipt reported a pass ago explains nothing this pass saw: {records:?}"
    );
}

/// A block can land between the receipt read and the balance read.
///
/// The balance then shows a send no receipt has answered for yet. The nonce says
/// the balance holds it, so the judgement waits for the receipt instead of
/// recording the drop as an unknown payment out beside the send itself.
#[tokio::test]
async fn a_send_the_balance_shows_before_its_receipt_is_recorded_once() {
    const FEE_WEI: u128 = 65_000 * 20 * GWEI;
    let harness = Harness::sent().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(4 * ETH - FEE_WEI))));
    harness.sync().await.unwrap();
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt("success").unwrap()));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert_eq!(records.len(), 1, "the send and nothing else: {records:?}");
    assert!(records.iter().all(|record| record.detection == TxDetection::Full), "{records:?}");
}

/// A receipt can come from a node ahead of the one that read the balance.
///
/// The nonce says that balance does not hold the send yet, so the send is counted
/// against the balance that first does, and a payment beside it keeps its own
/// amount.
#[tokio::test]
async fn a_receipt_read_ahead_of_the_balance_is_counted_once_the_balance_shows_the_send() {
    const FEE_WEI: u128 = 21_000 * 20 * GWEI;
    let harness = Harness::sent().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();
    harness.drain();

    let inside_the_window = receipt_for(&format!("0x{}", "22".repeat(32)), TIP - 1);
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&inside_the_window));
    harness.sync().await.unwrap();

    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH - FEE_WEI))));
    harness.sync().await.unwrap();

    let records = harness.records();
    let deltas: Vec<_> =
        records.iter().filter(|record| record.detection == TxDetection::BalanceDelta).collect();
    assert_eq!(deltas.len(), 1, "{records:?}");
    assert!(
        deltas.iter().all(|record| record.direction == TxDirection::Incoming
            && record.amount.base_units() == 2 * ETH),
        "the payment in, and none of the send: {records:?}"
    );
}

/// The receipt comes from a node at least the confirmation target ahead of the one
/// that answers the balance, which on BNB Smart Chain is under seven seconds.
#[tokio::test]
async fn a_send_retired_before_the_balance_shows_it_is_not_recorded_again_as_a_delta() {
    const FEE_WEI: u128 = 21_000 * 20 * GWEI;
    let harness = Harness::sent().await.unwrap();
    let txid = harness.stored_txids().await.unwrap().remove(0);
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.always(Method::GetTransactionReceipt, Answer::ok("null"));
    let sheet = |native: u128| aggregate(&sheet(native));
    harness.node.always(Method::Call, Answer::ok(&sheet(5 * ETH)));
    harness.sync().await.unwrap();
    harness.drain();

    // The receipt is deep enough to stop watching the send, while the balance and
    // nonce still predate it.
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt_for(&txid, TIP - 20)));
    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0, "the send is retired");

    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(13)));
    harness.node.always(Method::Call, Answer::ok(&sheet(4 * ETH - FEE_WEI)));
    harness.sync().await.unwrap();

    let records = harness.records();
    let deltas: Vec<_> =
        records.iter().filter(|record| record.detection == TxDetection::BalanceDelta).collect();
    assert!(
        deltas.is_empty(),
        "the balance drop is the wallet's own send, which its receipt already recorded, so it \
         must not be recorded a second time as an unknown payment out: {deltas:?}"
    );
}

/// `allowFailure` keeps one reverting contract from losing the batch, and a
/// contract that answers 2^256-1 should cost no more than one that reverts.
#[tokio::test]
async fn a_token_balance_wider_than_128_bits_is_left_unknown_and_the_pass_goes_on() {
    const SENTINEL: u128 = 0x00ab_cdef_0123_4567_89ab_cdef;
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    let mut balances = sheet(5 * ETH);
    balances[1] = Some(SENTINEL);
    harness.node.quiet(&balances);
    let sentinel_word = format!("{SENTINEL:064x}");
    let answer = aggregate(&balances);
    assert_eq!(answer.matches(&sentinel_word).count(), 1, "the sentinel word is unique");
    let oversized = answer.replace(&sentinel_word, &"f".repeat(64));
    harness.node.always(Method::Call, Answer::ok(&oversized));

    let pass = harness.sync().await;
    let native_reported = harness.drain().into_iter().any(|event| {
        matches!(event, WalletEvent::BalanceChanged(update)
            if update.asset == AssetId::Native(ChainId::Ethereum)
                && update.balance.base_units() == 5 * ETH)
    });
    assert!(
        pass.is_ok() && native_reported,
        "one token answering a balance no supply can reach is an unknown balance for that token, \
         but the pass answered {pass:?} and the native balance reported is {native_reported}"
    );
}

/// The cursor store refused the write at the end of a pass that had already
/// reported a native change, so the next pass judged the same change again.
#[tokio::test]
async fn a_delta_judged_again_after_a_failed_pass_keeps_its_txid() {
    let store = Arc::new(RefusingCursorStore::default());
    let node = FakeNode::new();
    let clock = Arc::new(FakeClock::default());
    let adapter = EvmAdapter::builder(
        ETHEREUM,
        Arc::<FakeNode>::clone(&node),
        Arc::<RefusingCursorStore>::clone(&store),
        Arc::<FakeClock>::clone(&clock),
    )
    .config(Setup::default().one_window().config)
    .build()
    .unwrap();
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, OWNER)]).await.unwrap();
    let events = FakeEventSink::new();
    let sheet = |native: u128| aggregate(&sheet(native));
    node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    node.always(Method::GetLogs, Answer::ok("[]"));
    node.always(Method::Call, Answer::ok(&sheet(5 * ETH)));
    adapter.sync(clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();
    events.drain();

    // A peer pays two coins, and the write that would remember the judgement fails.
    node.always(Method::Call, Answer::ok(&sheet(7 * ETH)));
    store.scan_refusal().store(true, Ordering::SeqCst);
    adapter
        .sync(clock.deadline_in(Duration::from_secs(60)), &events)
        .await
        .expect_err("the cursor write failed");

    store.scan_refusal().store(false, Ordering::SeqCst);
    node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(TIP + 1))));
    adapter.sync(clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();

    let mut rows: HashMap<String, u128> = HashMap::new();
    for event in events.drain() {
        if let WalletEvent::TxDetected(record) = event
            && record.detection == TxDetection::BalanceDelta
        {
            rows.insert(record.txid.value().to_owned(), record.amount.base_units());
        }
    }
    let total: u128 = rows.values().sum();
    assert_eq!(
        total,
        2 * ETH,
        "the store keeps one row per txid, so one payment of two coins must leave one row of two \
         coins, but these rows were reported: {rows:?}"
    );
}

#[tokio::test]
async fn the_synced_height_is_absent_while_one_address_has_never_been_scanned() {
    static NARROW_WINDOW: EvmParams = EvmParams { log_window_blocks: 32, ..*ETHEREUM };
    let mut setup = Setup::on(&NARROW_WINDOW).one_window();
    setup.config.min_log_window_blocks = 32;
    let harness = Harness::new(setup).unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    // One sheet for the pass over one address, then a standing one for the two.
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(0))));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(ETHEREUM, 2, 0))));

    harness.watch(&[watched(7, ChainId::Ethereum, OWNER)]).await.unwrap();
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    let first = harness.sync().await.unwrap();
    assert_eq!(first.synced_height(), Some(TIP));

    // A second address joins unscanned and forms its own group, whose window is
    // refused at the narrowest width. The fresh group sorts first, and the
    // caught-up one has nothing left to ask for.
    harness
        .watch(&[watched(7, ChainId::Ethereum, OWNER), watched(8, ChainId::Ethereum, SECOND)])
        .await
        .unwrap();
    harness.node.on(Method::GetLogs, Answer::refused("block range is too large"));
    let joined = harness.sync().await.unwrap();
    assert_eq!(
        joined.synced_height(),
        None,
        "a wallet is not synced to a height one of its addresses has not reached"
    );
    assert_eq!(joined.tip_height(), Some(TIP), "the tip is still known");
}

#[tokio::test]
async fn a_pass_that_moves_nothing_writes_no_cursor() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheet(ETH));

    harness.sync().await.unwrap();
    let after_first = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a cursor row");
    let advances = harness.cursors.advance_count();

    // The tip and balances have not moved, so there is nothing to write.
    harness.sync().await.unwrap();
    assert_eq!(harness.cursors.advance_count(), advances, "a quiet pass wrote nothing");
    assert_eq!(
        harness.cursors.load_scan(CursorScope::Address(7), ScanStream::EvmLogScan).await.unwrap(),
        Some(after_first)
    );
}

#[tokio::test]
async fn a_pass_over_no_addresses_reports_complete_without_a_round_trip() {
    let harness = Harness::ethereum().unwrap();
    let progress = harness.sync().await.unwrap();
    assert!(progress.is_complete());
    assert_eq!(progress.tip_height(), None);
    assert!(harness.node.calls().is_empty());
}

#[tokio::test]
async fn the_next_pass_re_reads_the_frontier_the_last_one_left() {
    let setup = Setup::default().one_window();
    let overlap = setup.params.rescan_overlap_blocks();
    assert!(overlap > 0, "a network with no overlap could not be reorganised");
    let harness = Harness::new(setup).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(0))));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));

    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.sync().await.unwrap();
    let first_pass = harness.node.calls_to(Method::GetLogs).len();
    assert!(first_pass > 0, "the first pass read a window");

    let moved = TIP + 10;
    harness.node.always(Method::BlockNumber, Answer::ok(&format!("\"{moved:#x}\"")));
    harness.sync().await.unwrap();

    let windows = harness.node.calls_to(Method::GetLogs);
    let next = windows.get(first_pass).expect("the second pass asked for a window");
    let from = TIP + 1 - overlap;
    assert!(next.params.contains(&format!("{from:#x}")), "{}", next.params);
    assert!(next.params.contains(&format!("{moved:#x}")), "{}", next.params);
    assert_eq!(
        windows.len(),
        first_pass.saturating_mul(2),
        "the overlap widens the window that was going out, it does not add one"
    );
}

#[tokio::test]
async fn a_window_the_endpoint_refuses_is_halved_and_retried_inside_the_same_pass() {
    static WIDE_WINDOW: EvmParams = EvmParams { log_window_blocks: 1_000, ..*ETHEREUM };
    let mut setup = Setup::on(&WIDE_WINDOW).one_window();
    setup.config.min_log_window_blocks = 250;
    let harness = Harness::new(setup).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(0))));
    harness
        .node
        .on(Method::GetLogs, Answer::refused("eth_getLogs block range is too large, max 500"));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));

    harness.sync().await.unwrap();
    let windows = harness.node.calls_to(Method::GetLogs);
    assert_eq!(windows.len(), 3, "one refused, then two filters at the narrower width");
    let refused = windows.first().expect("the refused filter");
    let retried = windows.get(1).expect("the retried filter");
    // A 1000-block window up to the tip starts at 0x112a4fd, and a 500-block one
    // at 0x112a6f1.
    assert!(refused.params.contains("0x112a4fd"), "the first width: {}", refused.params);
    assert!(retried.params.contains("0x112a6f1"), "the window halved: {}", retried.params);
}

#[tokio::test]
async fn a_window_no_endpoint_will_serve_leaves_the_cursor_alone_and_the_balances_stand() {
    static NARROW_WINDOW: EvmParams = EvmParams { log_window_blocks: 32, ..*ETHEREUM };
    let mut setup = Setup::on(&NARROW_WINDOW).one_window();
    setup.config.min_log_window_blocks = 32;
    let harness = Harness::new(setup).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.node.always(Method::GetLogs, Answer::refused("block range is too large"));

    let progress = harness.sync().await.unwrap();
    assert_eq!(progress.tip_height(), Some(TIP));
    let balances = harness
        .drain()
        .into_iter()
        .filter(|event| matches!(event, WalletEvent::BalanceChanged(_)))
        .count();
    assert_eq!(balances, SHEET_ENTRIES, "the balance sheet is read whatever the log walk did");

    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a cursor row");
    let cursor: Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    assert_eq!(
        cursor.get("forwardTo").and_then(Value::as_u64),
        None,
        "nothing was scanned, so the frontier did not move"
    );
}

#[tokio::test]
async fn a_pass_whose_budget_has_run_out_walks_no_windows() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheet(ETH));

    // Only the log walk is skipped. The balances are still read, because an
    // expired budget is no reason to report nothing.
    let progress = harness.sync_expired().await.unwrap();
    assert_eq!(progress.tip_height(), Some(TIP));
    assert!(harness.node.calls_to(Method::GetLogs).is_empty(), "no window was asked for");
    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a cursor row");
    let cursor: Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    assert_eq!(cursor.get("forwardTo").and_then(Value::as_u64), None, "nothing was scanned");
}

#[tokio::test(start_paused = true)]
async fn a_pass_that_cannot_finish_inside_its_budget_is_reported_rather_than_waited_on() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheet(ETH));
    harness.node.slow_by(Duration::from_secs(300));

    let error = harness.sync().await.expect_err("the budget is sixty seconds");
    let ChainError::EndpointsDown { operation, .. } = error else {
        panic!("unexpected {error:?}");
    };
    assert_eq!(operation, "a sync pass inside its deadline");
}

/// Each log query takes a second, on the runtime timer and on the adapter's clock
/// alike, the way a rate-limited public node answers.
#[derive(Debug)]
struct SlowLogs {
    inner: Arc<FakeNode>,
    clock: Arc<FakeClock>,
}

impl SlowLogs {
    async fn wait_if_logs(&self, calls: &[RpcCall]) {
        if calls.iter().any(|call| call.method() == Method::GetLogs) {
            sleep(Duration::from_secs(1)).await;
            self.clock.advance(Duration::from_secs(1));
        }
    }
}

#[async_trait]
impl EvmNode for SlowLogs {
    async fn call(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.wait_if_logs(from_ref(call)).await;
        self.inner.call(call).await
    }

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, EvmError> {
        self.wait_if_logs(calls).await;
        self.inner.batch(calls).await
    }

    async fn broadcast(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.inner.broadcast(call).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_pass_whose_deadline_falls_during_a_log_query_returns_the_walk_it_made() {
    static LONG_HISTORY: EvmParams = EvmParams { backfill_floor: TIP - 1_000_000, ..*ETHEREUM };
    let config = EvmConfig { max_backfill_windows: 1_000, ..EvmConfig::default() };
    let node = FakeNode::new();
    node.quiet(&sheet(ETH));
    let clock = Arc::new(FakeClock::default());
    let cursors = Arc::new(MemoryCursorStore::new());
    let slow = Arc::new(SlowLogs { inner: node, clock: Arc::clone(&clock) });
    let adapter = EvmAdapter::builder(
        &LONG_HISTORY,
        slow,
        Arc::<MemoryCursorStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&clock),
    )
    .config(config)
    .build()
    .unwrap();
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, OWNER)]).await.unwrap();
    let events = FakeEventSink::new();

    // A window is two queries, so the thirty-first is part way through when the
    // deadline passes.
    let deadline = clock.deadline_in(Duration::from_millis(60_500));
    let progress = adapter.sync(deadline, &events).await;
    assert!(progress.is_ok(), "a pass that ran out of time stops with what it has: {progress:?}");
    assert!(
        cursors.load_scan(CursorScope::Address(7), ScanStream::EvmLogScan).await.unwrap().is_some(),
        "the windows the pass scanned are kept for the next one"
    );
}

#[tokio::test]
async fn every_carried_token_is_in_the_log_filters_and_reports_its_balance() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    let cursor = serde_json::json!({
        "version": 1, "forwardTo": null, "backfillFrom": null,
        "nativeBalance": ETH, "nativeNonce": 0, "settledThrough": 0,
    });
    harness
        .cursors
        .advance(
            CursorScope::Address(7),
            ScanStream::EvmLogScan,
            CursorBytes::copied(cursor.to_string().as_bytes()),
        )
        .await
        .unwrap();
    harness.watch_owner().await.unwrap();
    let added = [
        (TokenId::LinkErc20, 11),
        (TokenId::WbtcErc20, 12),
        (TokenId::WethErc20, 13),
        (TokenId::UniErc20, 14),
        (TokenId::ShibErc20, 15),
        (TokenId::EnaErc20, 16),
        (TokenId::AaveErc20, 17),
        (TokenId::OndoErc20, 18),
        (TokenId::PepeErc20, 19),
        (TokenId::ArbErc20, 20),
    ];
    let mut balances = sheet(ETH);
    for (token, units) in added {
        balances[sheet_index(ETHEREUM, token).unwrap()] = Some(units);
    }
    harness.node.quiet(&balances);
    harness.sync().await.unwrap();
    let filters = harness.node.calls_to(Method::GetLogs);
    assert_eq!(filters.len(), 2);
    for (token, _) in added {
        assert!(
            filters
                .iter()
                .all(|call| call.params.to_lowercase().contains(&token.contract().to_lowercase()))
        );
    }
    let events = harness.drain();
    for (token, units) in added {
        assert!(events.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(update)
            if update.asset == AssetId::Token(token) && update.balance.base_units() == units)));
    }
    assert!(!events.iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.detection == TxDetection::BalanceDelta)));
}

#[tokio::test]
async fn each_carried_token_reports_its_balance_and_its_logged_transfers() {
    for (network, token) in [
        (ETHEREUM, TokenId::LinkErc20),
        (ETHEREUM, TokenId::WbtcErc20),
        (ETHEREUM, TokenId::PaxgErc20),
        (ETHEREUM, TokenId::XautErc20),
        (BSC, TokenId::LinkBep20),
        (BSC, TokenId::XautBep20),
        (BSC, TokenId::EnaBep20),
        (BASE, TokenId::WethBase),
        (OPTIMISM, TokenId::OpOptimism),
    ] {
        let harness = Harness::new(Setup::on(network).one_window()).unwrap();
        let cursor = serde_json::json!({
            "version": 1, "forwardTo": null, "backfillFrom": null,
            "nativeBalance": 0, "nativeNonce": 0, "settledThrough": 0,
        });
        harness
            .cursors
            .advance(
                CursorScope::Address(7),
                ScanStream::EvmLogScan,
                CursorBytes::copied(cursor.to_string().as_bytes()),
            )
            .await
            .unwrap();
        harness.watch_owner().await.unwrap();
        let amount = Amount::parse("0.125", token.decimals()).unwrap();
        let mut balances = sheets_on(network, 1, 0);
        balances[sheet_index(network, token).unwrap()] = Some(amount.base_units());
        harness.node.quiet(&balances);
        let recorded: Vec<Value> = serde_json::from_str(&logs("result").unwrap()).unwrap();
        let mut incoming = recorded.first().unwrap().clone();
        incoming["address"] = serde_json::json!(token.contract());
        incoming["data"] = serde_json::json!(format!("0x{:064x}", amount.base_units()));
        harness.node.on(Method::GetLogs, Answer::ok(&serde_json::json!([incoming]).to_string()));
        harness.sync().await.unwrap();
        let asset = AssetId::Token(token);
        let events = harness.drain();
        assert!(events.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(update)
            if update.asset == asset && update.balance == amount)));
        assert!(events.iter().any(|event| matches!(event, WalletEvent::TxDetected(record)
            if record.asset == asset && record.amount == amount
                && record.direction == TxDirection::Incoming
                && record.detection == TxDetection::Full)));
    }
}

#[tokio::test]
async fn an_unreadable_scan_cursor_blocks_every_operation_and_is_never_overwritten() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness
        .cursors
        .advance(CursorScope::Address(7), ScanStream::EvmLogScan, CursorBytes::copied(b"not json"))
        .await
        .unwrap();
    assert!(harness.watch_owner().await.is_err());
    assert!(harness.watch_owner().await.is_err());
    harness.sync().await.unwrap_err();
    let request = TxRequestBuilder::new(
        7,
        PEER,
        AssetId::Native(ChainId::Ethereum),
        Amount::from_base_units(1),
    )
    .build();
    harness.adapter.build_tx(&request).await.unwrap_err();
    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.with_bytes(|bytes| bytes == b"not json"));
    assert_eq!(harness.cursors.advance_count(), 1);
}

#[tokio::test]
async fn a_backfill_walks_down_over_successive_passes() {
    static SHORT_HISTORY: EvmParams =
        EvmParams { log_window_blocks: 100, backfill_floor: TIP - 300, ..*ETHEREUM };
    let mut setup = Setup::on(&SHORT_HISTORY);
    setup.config.max_forward_windows = 1;
    setup.config.max_backfill_windows = 1;
    let harness = Harness::new(setup).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheet(0));

    let first = harness.sync().await.unwrap();
    assert!(first.progress_permille() < 1_000, "the history is not covered yet");
    let second = harness.sync().await.unwrap();
    assert!(second.progress_permille() > first.progress_permille(), "the backfill moved");
    for _ in 0..4u8 {
        harness.sync().await.unwrap();
    }
    let done = harness.sync().await.unwrap();
    assert_eq!(done.progress_permille(), 1_000, "the walk reached the floor");
}
