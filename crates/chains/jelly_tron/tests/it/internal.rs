//! TRX that contracts move: paid to an address by a contract, sent by it through
//! a contract call, and netted into one record per transaction whichever stream
//! lists the transaction.

use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, TxDirection, TxOutcome, TxRecord, WalletEvent,
    test_util::{FakeEventSink, records},
};
use jelly_tron::{
    Route, TronConfig,
    test_util::{TRX_OWNER, TRX_OWNER_HEX, TRX_RECIPIENT_HEX},
};

use crate::common::{
    Answer, FakeNode, HISTORY_SWEEP, Harness, TRX, account, balances_only, canonical, fixture,
    history_page, pass, transfer_row, watching_owner,
};

/// The contract that pays in these tests.
const PAYER_HEX: &str = "416804040d601e099c588b5e7392e92c56a54609c9";
/// The account that calls [`PAYER_HEX`] to make a payout.
const OPERATOR_HEX: &str = "4125883a7c842f157d3a4cbea1976c92591d674d16";
/// Someone else the contract pays.
const OTHER_HEX: &str = TRX_RECIPIENT_HEX;

/// The payout `internal_page1.json` and `internal_parent.json` describe.
const PAYOUT: &str = "7a3f0c9e1b5d2a8c4e6f0b1d3c5e7a9b2d4f6a8c0e1b3d5f7a9c1e3b5d7f9a1c";
/// The transaction of the step `internal_page1.json` lists as rejected.
const REJECTED: &str = "3e8d1f6a0c2b4d5e7f9a1b3c5d7e9f0a2b4c6d8e0f1a3b5c7d9e1f2a4b6c8d0e";
const SWAP: &str = "5c2e9a7f1d3b8e6a4c0f2d9b7e5a3c1f8d6b4e2a0c9f7d5b3e1a8c6f4d2b0e9a";

/// One TRX movement a contract made while a transaction ran.
#[derive(Debug, Clone, Copy)]
struct Step {
    from_hex: &'static str,
    to_hex: &'static str,
    sun: u64,
    is_rejected: bool,
}

impl Step {
    const fn paid(from_hex: &'static str, to_hex: &'static str, sun: u64) -> Self {
        Self { from_hex, to_hex, sun, is_rejected: false }
    }

    const fn rejected(self) -> Self {
        Self { is_rejected: true, ..self }
    }

    /// As a native row lists it, inside its transaction.
    fn inner(&self) -> String {
        self.json("")
    }

    /// As the internal stream lists it, naming its transaction.
    fn listed(&self, call: &ContractCall) -> String {
        self.json(&format!(
            r#""tx_id":"{}","block_timestamp":{},"#,
            call.txid, call.timestamp_millis
        ))
    }

    fn json(&self, listing: &str) -> String {
        format!(
            r#"{{{listing}"data":{{"note":"call","rejected":{},"call_value":{{"_":{}}}}},
                "to_address":"{}","from_address":"{}"}}"#,
            self.is_rejected, self.sun, self.to_hex, self.from_hex
        )
    }
}

/// A call to [`PAYER_HEX`] as the indexer lists it, with the steps the call took.
#[derive(Debug, Clone)]
struct ContractCall {
    txid: String,
    sender_hex: &'static str,
    block: u64,
    timestamp_millis: i64,
    value_sun: u64,
    contract_ret: &'static str,
    fee_sun: u64,
    steps: Vec<Step>,
}

impl ContractCall {
    /// A call that carries nothing, mined two blocks under the tip the shared
    /// fixtures report.
    fn new(txid: &str, sender_hex: &'static str) -> Self {
        Self {
            txid: txid.to_owned(),
            sender_hex,
            block: 85_304_911,
            timestamp_millis: 1_786_585_800_000,
            value_sun: 0,
            contract_ret: "SUCCESS",
            fee_sun: 1_234_000,
            steps: Vec::new(),
        }
    }

    fn row(&self) -> String {
        let steps: Vec<String> = self.steps.iter().map(Step::inner).collect();
        format!(
            r#"{{"txID":"{}","blockNumber":{},"block_timestamp":{},
                "ret":[{{"contractRet":"{}","fee":{}}}],
                "raw_data":{{"contract":[{{"type":"TriggerSmartContract","parameter":{{"value":{{
                  "data":"3ccfd60b","owner_address":"{}","contract_address":"{PAYER_HEX}",
                  "call_value":{}}}}}}}]}},
                "internal_transactions":[{}]}}"#,
            self.txid,
            self.block,
            self.timestamp_millis,
            self.contract_ret,
            self.fee_sun,
            self.sender_hex,
            self.value_sun,
            steps.join(",")
        )
    }

    /// What `gettransactionbyid` answers, which names the sender and no block,
    /// fee, or internal transfer.
    fn held(&self) -> String {
        format!(
            r#"{{"ret":[{{"contractRet":"{}"}}],"txID":"{}",
                "raw_data":{{"contract":[{{"type":"TriggerSmartContract","parameter":{{"value":{{
                  "data":"3ccfd60b","owner_address":"{}","contract_address":"{PAYER_HEX}",
                  "call_value":{}}}}}}}]}}}}"#,
            self.contract_ret, self.txid, self.sender_hex, self.value_sun
        )
    }

    /// Scripts the two reads that find this transaction from one of its steps:
    /// its sender, then the sender's history at its block time.
    fn findable(&self, fake: &Arc<FakeNode>) -> Arc<FakeNode> {
        fake.on_matching(Route::TransactionById, &self.txid, Answer::body(&self.held()))
            .on_matching(
                Route::NativeHistory,
                &format!("min_timestamp={}", self.timestamp_millis.saturating_sub(1)),
                history_page(&[self.row().as_str()], None),
            )
    }
}

fn trx_records(events: &[WalletEvent], txid: &str) -> Vec<TxRecord> {
    records(events)
        .into_iter()
        .filter(|record| record.asset == TRX && record.txid.value() == txid)
        .collect()
}

fn lookups(fake: &FakeNode, txid: &str) -> usize {
    fake.calls()
        .iter()
        .filter(|call| {
            call.route == Route::TransactionById
                && call.body.as_deref().is_some_and(|body| body.contains(txid))
        })
        .count()
}

#[tokio::test]
async fn a_contract_payout_is_recorded_as_incoming_trx_from_the_contract() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .always(Route::InternalHistory, Answer::body(fixture!("internal_page1")))
        .on_matching(
            Route::TransactionById,
            PAYOUT,
            Answer::body(&ContractCall::new(PAYOUT, OPERATOR_HEX).held()),
        )
        .on_matching(
            Route::NativeHistory,
            "only_from=true",
            Answer::body(fixture!("internal_parent")),
        );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let reported = records(&sink.drain());
    assert_eq!(reported.len(), 1, "the rejected step moved nothing: {reported:?}");
    let record = reported.first().unwrap();
    assert_eq!(record.txid.value(), PAYOUT, "the transaction the transfer ran in");
    assert_eq!(record.asset, TRX);
    assert_eq!(record.direction, TxDirection::Incoming);
    assert_eq!(record.amount, Amount::from_base_units(1_000_000));
    assert_eq!(record.counterparty, Some(canonical(PAYER_HEX).unwrap()), "the contract that paid");
    assert_eq!(record.fee, None, "the operator that called the contract paid the fee");
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.height, Some(85_304_911));
    assert_eq!(record.timestamp, Some(1_786_585_800));
    assert_eq!(record.confirmations, 3, "two blocks under the tip");

    let calls = harness.fake.calls();
    let internal = format!("/v1/accounts/{TRX_OWNER}/internal-transactions");
    assert!(calls.iter().any(|call| call.route == Route::InternalHistory && call.path == internal));
    let parent = calls
        .iter()
        .find(|call| call.query.contains("only_from=true"))
        .expect("the payout was read from its sender's history");
    assert_eq!(
        parent.path,
        format!("/v1/accounts/{}/transactions", canonical(OPERATOR_HEX).unwrap())
    );
    assert_eq!(lookups(&harness.fake, REJECTED), 0, "a step that reverted is never looked up");
}

#[tokio::test]
async fn an_internal_transfers_parent_is_found_beyond_the_first_sender_history_page() {
    let (parent, listed) = payout(PAYOUT, 85_304_911, 1_786_585_800_000);
    let other = ContractCall::new(SWAP, OPERATOR_HEX).row();
    let first_page = vec![other.as_str(); 200];
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .always(Route::InternalHistory, history_page(&[listed.as_str()], None))
        .on_matching(Route::TransactionById, PAYOUT, Answer::body(&parent.held()))
        .on_matching(
            Route::NativeHistory,
            "only_from=true",
            history_page(&first_page, Some("FpParent")),
        )
        .on_matching(
            Route::NativeHistory,
            "fingerprint=FpParent",
            history_page(&[parent.row().as_str()], None),
        );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let reported = trx_records(&sink.drain(), PAYOUT);
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0].amount, Amount::from_base_units(1_000_000));
    let calls = fake.calls();
    let pages: Vec<_> = calls.iter().filter(|call| call.query.contains("only_from=true")).collect();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].path, pages[1].path);
    assert_eq!(pages[1].query, format!("{}&fingerprint=FpParent", pages[0].query));

    fake.forget_calls();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(fake.count(Route::InternalHistory), 0);
}

/// The sender history pages read while searching for a step's parent transaction.
fn parent_pages(fake: &FakeNode) -> usize {
    fake.calls().iter().filter(|call| call.query.contains("only_from=true")).count()
}

/// A wallet whose one internal transfer is from `parent`, and whose other streams are
/// quiet. The test scripts the sender history the search reads.
fn paid_by(parent: &ContractCall, listed: &str) -> Arc<FakeNode> {
    balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .always(Route::InternalHistory, history_page(&[listed], None))
        .on_matching(Route::TransactionById, &parent.txid, Answer::body(&parent.held()))
}

#[tokio::test]
async fn an_empty_sender_history_page_ends_the_search_for_a_parent() {
    let (parent, listed) = payout(PAYOUT, 85_304_911, 1_786_585_800_000);
    let fake = paid_by(&parent, &listed).on_matching(
        Route::NativeHistory,
        "only_from=true",
        history_page(&[], Some("FpEmpty")),
    );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    assert!(trx_records(&sink.drain(), PAYOUT).is_empty(), "nothing placed the payout");
    assert_eq!(parent_pages(&fake), 1, "a page with no rows is the end of the history");
}

#[tokio::test]
async fn the_search_for_a_parent_stops_at_its_page_budget() {
    let (parent, listed) = payout(PAYOUT, 85_304_911, 1_786_585_800_000);
    let other = ContractCall::new(SWAP, OPERATOR_HEX).row();
    let mut fake = paid_by(&parent, &listed);
    for page in 1..=20u8 {
        let fingerprint = format!("Fp{page:02}");
        fake = fake.on_matching(
            Route::NativeHistory,
            "only_from=true",
            history_page(&[other.as_str()], Some(&fingerprint)),
        );
    }
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    assert!(trx_records(&sink.drain(), PAYOUT).is_empty(), "nothing placed the payout");
    assert_eq!(
        parent_pages(&fake),
        5,
        "an indexer that never runs out of fingerprints is not followed past the budget"
    );
}

#[tokio::test]
async fn trx_sent_through_a_contract_call_is_recorded_as_outgoing_with_its_fee() {
    let mut sent = ContractCall::new(SWAP, TRX_OWNER_HEX);
    sent.value_sun = 2_500_000;
    sent.fee_sun = 345_000;
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[sent.row().as_str()], None))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let reported = records(&sink.drain());
    assert_eq!(reported.len(), 1, "{reported:?}");
    let record = reported.first().unwrap();
    assert_eq!(record.asset, TRX);
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.amount, Amount::from_base_units(2_500_000));
    assert_eq!(
        record.counterparty,
        Some(canonical(PAYER_HEX).unwrap()),
        "the contract it was sent to"
    );
    assert_eq!(record.fee, Some(Amount::from_base_units(345_000)), "the sender paid the fee");
    assert_eq!(record.height, Some(85_304_911));
}

#[tokio::test]
async fn every_trx_movement_of_one_transaction_nets_into_one_record() {
    // The address sends a contract ten TRX, and the call pays it back three and
    // two, pays someone else four, and has a seven TRX step that reverted.
    let back = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 3_000_000);
    let more = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 2_000_000);
    let mut swap = ContractCall::new(SWAP, TRX_OWNER_HEX);
    swap.value_sun = 10_000_000;
    swap.steps = vec![
        back,
        more,
        Step::paid(PAYER_HEX, OTHER_HEX, 4_000_000),
        Step::paid(PAYER_HEX, TRX_OWNER_HEX, 7_000_000).rejected(),
    ];
    // An operator's payout whose call paid the address twice.
    let first = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 1_000_000);
    let second = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 500_000);
    let mut payout = ContractCall::new(PAYOUT, OPERATOR_HEX);
    payout.block = 85_304_878;
    payout.timestamp_millis = 1_786_585_700_000;
    payout.steps = vec![first, second];

    let internal =
        [back.listed(&swap), more.listed(&swap), first.listed(&payout), second.listed(&payout)];
    let listed: Vec<&str> = internal.iter().map(String::as_str).collect();
    let fake = payout.findable(
        &balances_only(TRX_OWNER)
            .on(Route::NativeHistory, history_page(&[swap.row().as_str()], None))
            .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
            .always(Route::InternalHistory, history_page(&listed, None)),
    );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let events = sink.drain();

    let swapped = trx_records(&events, SWAP);
    assert_eq!(swapped.len(), 1, "one record for the whole transaction: {swapped:?}");
    let swapped = swapped.first().unwrap();
    assert_eq!(swapped.direction, TxDirection::Outgoing);
    assert_eq!(swapped.amount, Amount::from_base_units(5_000_000), "ten out, five back");
    assert_eq!(swapped.fee, Some(Amount::from_base_units(1_234_000)), "the fee counted once");
    assert_eq!(swapped.counterparty, Some(canonical(PAYER_HEX).unwrap()));

    let paid = trx_records(&events, PAYOUT);
    assert_eq!(paid.len(), 1, "one record for the whole transaction: {paid:?}");
    let paid = paid.first().unwrap();
    assert_eq!(paid.direction, TxDirection::Incoming);
    assert_eq!(paid.amount, Amount::from_base_units(1_500_000));
    assert_eq!(paid.fee, None);

    assert_eq!(
        lookups(&harness.fake, SWAP),
        0,
        "the native stream already reported the swap whole, internal transfers and all"
    );
    assert_eq!(lookups(&harness.fake, PAYOUT), 1, "two steps of one payout read it once");
}

#[tokio::test]
async fn a_rejected_step_and_the_steps_of_a_failed_transaction_move_nothing() {
    // An operator's payout that reverted, listed with a step the indexer didn't
    // mark rejected.
    let refund = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 1_000_000);
    let mut reverted = ContractCall::new(PAYOUT, OPERATOR_HEX);
    reverted.contract_ret = "REVERT";
    reverted.steps = vec![refund];
    // A payout that succeeded, with a step that reverted on its own.
    let refused = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 2_000_000).rejected();
    let mut partial = ContractCall::new(REJECTED, OPERATOR_HEX);
    partial.timestamp_millis = 1_786_585_500_000;
    // The address's own call that carried five TRX, ran out of energy, and lists
    // a step paying three back.
    let mut failed = ContractCall::new(SWAP, TRX_OWNER_HEX);
    failed.value_sun = 5_000_000;
    failed.contract_ret = "OUT_OF_ENERGY";
    failed.fee_sun = 345_000;
    failed.steps = vec![Step::paid(PAYER_HEX, TRX_OWNER_HEX, 3_000_000)];

    let internal = [refund.listed(&reverted), refused.listed(&partial)];
    let listed: Vec<&str> = internal.iter().map(String::as_str).collect();
    let fake = reverted.findable(
        &balances_only(TRX_OWNER)
            .on(Route::NativeHistory, history_page(&[failed.row().as_str()], None))
            .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
            .always(Route::InternalHistory, history_page(&listed, None)),
    );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let reported = records(&sink.drain());
    assert_eq!(reported.len(), 1, "only the failed send is history: {reported:?}");
    let record = reported.first().unwrap();
    assert_eq!(record.txid.value(), SWAP);
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.amount, Amount::ZERO, "a failed call moved nothing, out or back");
    assert_eq!(record.fee, Some(Amount::from_base_units(345_000)), "and still cost its fee");
    assert_eq!(lookups(&harness.fake, REJECTED), 0, "a rejected step is never looked up");
}

/// A payout of 1 TRX to the address, mined at `timestamp_millis`.
fn payout(txid: &str, block: u64, timestamp_millis: i64) -> (ContractCall, String) {
    let mut call = ContractCall::new(txid, OPERATOR_HEX);
    call.block = block;
    call.timestamp_millis = timestamp_millis;
    let step = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 1_000_000);
    call.steps = vec![step];
    let listed = step.listed(&call);
    (call, listed)
}

#[tokio::test]
async fn the_internal_stream_backfills_and_resumes_between_its_budget_and_its_frontier() {
    let (p0, row0) = payout(&"a0".repeat(32), 85_304_000, 1_786_583_000_000);
    let (p1, row1) = payout(&"a1".repeat(32), 85_304_100, 1_786_583_300_000);
    let (p2, row2) = payout(&"a2".repeat(32), 85_304_200, 1_786_583_600_000);
    let (p3, row3) = payout(&"a3".repeat(32), 85_304_300, 1_786_583_900_000);
    let (p4, row4) = payout(&"a4".repeat(32), 85_304_400, 1_786_584_200_000);
    let (p5, row5) = payout(&"a5".repeat(32), 85_304_500, 1_786_584_500_000);
    let mut fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .always(
            Route::InternalHistory,
            history_page(&[row2.as_str(), row1.as_str()], Some("FpBack")),
        )
        .on_matching(
            Route::InternalHistory,
            "fingerprint=FpBack",
            history_page(&[row0.as_str()], None),
        );
    for call in [&p0, &p1, &p2, &p3, &p4, &p5] {
        fake = call.findable(&fake);
    }
    let config = TronConfig { forward_pages: 1, backfill_pages: 1, ..TronConfig::default() };
    let harness = watching_owner(&fake, config).await.unwrap();

    let first = FakeEventSink::new();
    let progress = pass(&harness, &first).await.unwrap();
    let reported: Vec<String> =
        records(&first.drain()).into_iter().map(|record| record.txid.value().to_owned()).collect();
    assert_eq!(reported, vec![p2.txid.clone(), p1.txid.clone(), p0.txid.clone()]);
    assert_eq!(progress.synced_height(), Some(85_304_913), "every stream is walked back");

    // Three more payouts arrive and raise the balance, more than one forward page
    // holds.
    fake.always(Route::Account, account(TRX_OWNER, 10_000_000))
        .always(
            Route::InternalHistory,
            history_page(&[row5.as_str(), row4.as_str()], Some("FpOlder")),
        )
        .on_matching(
            Route::InternalHistory,
            "fingerprint=FpOlder",
            history_page(&[row3.as_str(), row2.as_str(), row1.as_str()], None),
        );
    let second = FakeEventSink::new();
    pass(&harness, &second).await.unwrap();
    let reported: Vec<String> =
        records(&second.drain()).into_iter().map(|record| record.txid.value().to_owned()).collect();
    assert_eq!(reported, vec![p5.txid.clone(), p4.txid.clone()]);

    // Once the news window has passed, the forward walk that was cut short still
    // makes the stream due, and it reads the one row left before the frontier.
    harness.clock.advance(Duration::from_secs(61));
    harness.fake.forget_calls();
    let third = FakeEventSink::new();
    pass(&harness, &third).await.unwrap();
    let reported: Vec<String> =
        records(&third.drain()).into_iter().map(|record| record.txid.value().to_owned()).collect();
    assert_eq!(reported, vec![p3.txid.clone()], "the frontier stops the walk at p2");
    assert!(
        harness.fake.calls().iter().any(|call| call.query.contains("fingerprint=FpOlder")),
        "the walk resumed from where the budget stopped it"
    );

    harness.fake.forget_calls();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    assert_eq!(harness.fake.count(Route::InternalHistory), 0, "caught up and nothing new");
}

#[tokio::test]
async fn a_transaction_both_streams_list_reads_the_same_whichever_lists_it_first() {
    // The address sends a contract ten TRX and the call refunds four.
    let refund = Step::paid(PAYER_HEX, TRX_OWNER_HEX, 4_000_000);
    let mut swap = ContractCall::new(SWAP, TRX_OWNER_HEX);
    swap.value_sun = 10_000_000;
    swap.fee_sun = 345_000;
    swap.steps = vec![refund];
    let native = || history_page(&[swap.row().as_str()], None);
    let internal = || history_page(&[refund.listed(&swap).as_str()], None);
    let quiet = || {
        balances_only(TRX_OWNER)
            .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
            .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
    };

    // The internal stream lists it before the native stream does, which reads it
    // on its sweep.
    let fake = swap.findable(&quiet().always(Route::InternalHistory, internal()));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let from_internal = trx_records(&sink.drain(), SWAP);
    fake.always(Route::NativeHistory, native());
    harness.clock.advance(HISTORY_SWEEP.saturating_add(Duration::from_secs(1)));
    pass(&harness, &sink).await.unwrap();
    let from_native = trx_records(&sink.drain(), SWAP);
    assert_eq!(from_internal.len(), 1, "{from_internal:?}");
    assert_eq!(from_internal, from_native, "the later list rewrites the row with the same values");
    let record = from_native.first().unwrap();
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.amount, Amount::from_base_units(6_000_000), "ten out, four back");
    assert_eq!(record.fee, Some(Amount::from_base_units(345_000)));
    assert_eq!(record.counterparty, Some(canonical(PAYER_HEX).unwrap()));

    // The native stream lists it a pass before the internal stream does, which
    // reads it on its sweep.
    let fake = swap.findable(&quiet().always(Route::NativeHistory, native()));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    let native_first = trx_records(&sink.drain(), SWAP);
    fake.always(Route::InternalHistory, internal());
    harness.clock.advance(HISTORY_SWEEP.saturating_add(Duration::from_secs(1)));
    pass(&harness, &sink).await.unwrap();
    let internal_later = trx_records(&sink.drain(), SWAP);
    assert_eq!(internal_later.len(), 1, "{internal_later:?}");
    assert_eq!(native_first, internal_later);
    assert_eq!(native_first, from_native);

    // Both list it in the same pass.
    let fake =
        quiet().always(Route::NativeHistory, native()).always(Route::InternalHistory, internal());
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(trx_records(&sink.drain(), SWAP), from_native);
    assert_eq!(lookups(&harness.fake, SWAP), 0, "the native row was already whole");
}

#[tokio::test]
async fn the_internal_stream_is_read_on_news_and_on_a_slow_sweep_and_not_on_a_quiet_pass() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let reads = |harness: &Harness| {
        let count = harness.fake.count(Route::InternalHistory);
        harness.fake.forget_calls();
        count
    };
    let sink = FakeEventSink::new();

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 1, "the first pass reads every stream");
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 0, "an unchanged balance is no news");

    // One TRX arrives that neither other stream reports.
    fake.always(Route::Account, account(TRX_OWNER, 8_000_000));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 1, "an unexplained change is news");
    harness.clock.advance(Duration::from_secs(12));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 1, "the indexer may list it a few blocks late");
    harness.clock.advance(Duration::from_secs(60));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 0, "the news has gone stale");

    harness.clock.advance(HISTORY_SWEEP);
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 1, "the sweep comes round");
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), 0);
}

#[tokio::test]
async fn a_balance_change_the_native_stream_explains_is_no_news() {
    let oldest = transfer_row(&"b0".repeat(32), 1_786_585_000_000, TRX_OWNER_HEX, OTHER_HEX, 5);
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[oldest.as_str()], None))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    harness.fake.forget_calls();

    // The address sends five sun, and the native stream reports exactly that.
    let newer = transfer_row(&"b1".repeat(32), 1_786_585_100_000, TRX_OWNER_HEX, OTHER_HEX, 5);
    fake.always(Route::NativeHistory, history_page(&[newer.as_str(), oldest.as_str()], None))
        .always(Route::Account, account(TRX_OWNER, 6_999_995));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(harness.fake.count(Route::InternalHistory), 0, "the change is explained");

    // It sends five more, and one more sun leaves that nothing reports.
    let newest = transfer_row(&"b2".repeat(32), 1_786_585_200_000, TRX_OWNER_HEX, OTHER_HEX, 5);
    fake.always(
        Route::NativeHistory,
        history_page(&[newest.as_str(), newer.as_str(), oldest.as_str()], None),
    )
    .always(Route::Account, account(TRX_OWNER, 6_999_989));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(harness.fake.count(Route::InternalHistory), 1, "a sun went unexplained");
}
