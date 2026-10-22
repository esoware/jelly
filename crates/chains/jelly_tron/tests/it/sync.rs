use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter as _, ChainError, ChainId, ChainSyncProgress, Clock as _, CursorBytes,
    CursorScope, CursorStore as _, Deadline, ScanStream, TokenId, TxDetection, TxDirection,
    TxOutcome, WalletEvent,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore, WatchedAddressBuilder, records},
};
use jelly_tron::{
    Answered, Request, Route, TronAdapter, TronConfig, TronError, TronNode,
    test_util::{
        OWNER, OWNER_KEY, RECIPIENT, TRX_OWNER, TRX_OWNER_HEX, TRX_RECIPIENT, TRX_RECIPIENT_HEX,
        USDT,
    },
};
use tokio::time::sleep;

use crate::common::{
    Answer, FakeNode, HISTORY_SWEEP, Harness, TOKEN_HISTORY, TRX, USDT_ASSET, account, balances,
    balances_only, base, fixture, funded, history_page, key_material, pass, quiet_history, ready,
    request, transfer_row, watched,
};

#[tokio::test]
async fn a_pass_reports_every_balance_and_the_tip() {
    let fake = quiet_history(&balances_only(OWNER));
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();

    let progress = pass(&harness, &sink).await.unwrap();
    assert_eq!(progress.chain(), ChainId::Tron);
    assert_eq!(progress.tip_height(), Some(85_304_913));
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
    assert_eq!(
        progress.synced_height(),
        Some(85_304_913),
        "an address whose whole history has been read is synced"
    );

    let seen = balances(&sink.drain());
    assert!(seen.contains(&(1, TRX, Amount::from_base_units(7_000_000))));
    assert!(seen.contains(&(1, USDT_ASSET, Amount::from_base_units(30_577_741_502))));
    assert_eq!(seen.len(), 2, "one native balance and one per supported token");
}

#[tokio::test]
async fn a_recorded_mainnet_account_row_reports_its_recorded_balance() {
    let fake = FakeNode::new();
    let fake = quiet_history(
        &base(&fake)
            .on_matching(Route::Account, RECIPIENT, Answer::body(fixture!("account_active")))
            .on_matching(
                Route::ConstantContract,
                "balanceOf(address)",
                Answer::body(fixture!("constant_balance_of")),
            ),
    );
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(1, RECIPIENT)]).await.unwrap();
    let sink = FakeEventSink::new();

    pass(&harness, &sink).await.unwrap();
    let seen = balances(&sink.drain());
    assert!(
        seen.contains(&(1, TRX, Amount::from_base_units(2_061_293_364_479_502))),
        "the recorded balance, in sun: {seen:?}"
    );
}

#[tokio::test]
async fn native_history_classifies_direction_amount_and_fee() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let records = records(&sink.drain());
    assert_eq!(records.len(), 2, "both rows on the recorded page are transfers from this account");
    let first = records.first().expect("a record");
    assert_eq!(
        first.txid.value(),
        "a46244abb4be3f4247c538712715c41476b5b0b0f515f657c6acd76b67674bc8"
    );
    assert_eq!(first.direction, TxDirection::Outgoing);
    assert_eq!(first.outcome, TxOutcome::Success);
    assert_eq!(first.asset, TRX);
    assert_eq!(first.amount, Amount::from_base_units(7));
    assert_eq!(first.address_id, 4);
    assert_eq!(first.height, Some(85_304_913));
    assert_eq!(first.confirmations, 1, "the tip block is one confirmation deep");
    assert_eq!(first.fee, Some(Amount::ZERO), "the sender pays, and this one paid nothing");
    assert_eq!(first.counterparty.as_deref(), Some(TRX_RECIPIENT));
    assert_eq!(first.detection, TxDetection::Full);
}

#[tokio::test]
async fn token_history_classifies_direction_amount_and_block() {
    let fake = balances_only(TOKEN_HISTORY)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("token_page1")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(9, TOKEN_HISTORY)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let records = records(&sink.drain());
    assert_eq!(records.len(), 3);
    let first = records.first().expect("a record");
    assert_eq!(first.asset, USDT_ASSET);
    assert_eq!(first.direction, TxDirection::Outgoing);
    assert_eq!(first.amount, Amount::from_base_units(125_000_000));
    assert_eq!(first.height, Some(85_304_913));
    assert!(first.confirmations > 0, "a mined transfer is at least one deep");
    assert_eq!(first.fee, None, "the TRX burned is on the native record for the same id");
    assert!(records.iter().all(|record| record.asset == USDT_ASSET));
}

#[tokio::test]
async fn a_forward_budget_that_ran_out_short_of_the_frontier_does_not_move_it() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page2")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let config = TronConfig { forward_pages: 1, backfill_pages: 1, ..TronConfig::default() };
    let harness = Harness::with(&fake, config);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(records(&sink.drain()).len(), 1);

    // Two newer transactions arrive and the balance shows them, and one page per
    // pass cannot reach the frontier. The frontier must not move over unread
    // history, so the next pass resumes from where the budget stopped, which this
    // fake answers with the same rows.
    fake.always(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .always(Route::Account, account(TRX_OWNER, 6_999_990));
    let second = FakeEventSink::new();
    pass(&harness, &second).await.unwrap();
    assert_eq!(records(&second.drain()).len(), 2);

    let third = FakeEventSink::new();
    pass(&harness, &third).await.unwrap();
    assert_eq!(
        records(&third.drain()).len(),
        2,
        "a duplicate record is idempotent; a frontier moved over unread history is not"
    );
}

#[tokio::test]
async fn an_airdropped_token_never_reaches_the_interface() {
    let page = format!(
        r#"{{"data":[{{"transaction_id":"aa","token_info":{{"address":"{OWNER}","decimals":6,
            "symbol":"SCAM"}},"from":"{TOKEN_HISTORY}","to":"{OWNER}","type":"Transfer",
            "value":"1"}}],"meta":{{}}}}"#
    );
    let fake = balances_only(TOKEN_HISTORY)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(&page));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(9, TOKEN_HISTORY)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert!(records(&sink.drain()).is_empty());
}

#[tokio::test]
async fn the_forward_frontier_stops_at_the_last_transaction_this_wallet_reported() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();

    let first = FakeEventSink::new();
    pass(&harness, &first).await.unwrap();
    assert_eq!(records(&first.drain()).len(), 2);

    // The sweep reads the newest page again and reports nothing, because the first
    // row on it is the one the frontier names.
    fake.always(Route::NativeHistory, Answer::body(fixture!("native_page1")));
    harness.clock.advance(HISTORY_SWEEP);
    harness.fake.forget_calls();
    let second = FakeEventSink::new();
    pass(&harness, &second).await.unwrap();
    assert!(records(&second.drain()).is_empty(), "nothing new is nothing reported");
    assert_eq!(
        harness.fake.count(Route::NativeHistory),
        1,
        "a caught-up stream costs one page, not a whole history"
    );
}

#[tokio::test]
async fn the_backfill_walks_older_pages_and_then_closes() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .on(Route::NativeHistory, Answer::body(fixture!("native_page2")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let config = TronConfig { forward_pages: 1, backfill_pages: 1, ..TronConfig::default() };
    let harness = Harness::with(&fake, config);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();

    let sink = FakeEventSink::new();
    let progress = pass(&harness, &sink).await.unwrap();
    assert_eq!(records(&sink.drain()).len(), 3, "two rows from page one and one from page two");
    assert!(progress.synced_height().is_none(), "a history still filling in is not synced");
    assert!(
        harness
            .cursors
            .load_scan(CursorScope::Address(4), ScanStream::TronTransfers)
            .await
            .unwrap()
            .is_some(),
        "the walk was written out"
    );

    // The backfill reaches the empty page and closes, and the token stream is
    // already complete, so the address is synced.
    let sink = FakeEventSink::new();
    let progress = pass(&harness, &sink).await.unwrap();
    assert_eq!(progress.synced_height(), Some(85_304_913));
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
}

#[tokio::test]
async fn an_empty_forward_page_ends_history_even_with_a_fingerprint() {
    let fake = quiet_history(&balances_only(OWNER));
    for route in [Route::NativeHistory, Route::TokenHistory, Route::InternalHistory] {
        fake.always(route, history_page(&[], Some("unused-next-page")));
    }
    let harness = ready(&fake).await.unwrap();
    let progress = pass(&harness, &FakeEventSink::new()).await.unwrap();
    assert_eq!(progress.synced_height(), Some(85_304_913));
    for route in [Route::NativeHistory, Route::TokenHistory, Route::InternalHistory] {
        assert_eq!(fake.count(route), 1, "an empty page must not start backfill");
    }
    let saved = harness
        .cursors
        .load_scan(CursorScope::Address(1), ScanStream::TronTransfers)
        .await
        .unwrap();
    assert!(saved.is_some());
}

#[tokio::test]
async fn a_quiet_pass_writes_no_cursor_at_all() {
    let fake = quiet_history(&balances_only(OWNER));
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let after_first = harness
        .cursors
        .load_scan(CursorScope::Address(1), ScanStream::TronTransfers)
        .await
        .unwrap();
    assert!(after_first.is_some(), "the first pass records that both streams are complete");
    let advances = harness.cursors.advance_count();

    pass(&harness, &sink).await.unwrap();
    assert_eq!(harness.cursors.advance_count(), advances, "a quiet pass writes nothing");
    assert_eq!(
        harness
            .cursors
            .load_scan(CursorScope::Address(1), ScanStream::TronTransfers)
            .await
            .unwrap(),
        after_first,
        "a pass that found nothing new leaves the stored state alone"
    );
}

#[tokio::test]
async fn an_unreadable_stored_state_walks_the_address_again() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = Harness::new(&fake);
    harness
        .cursors
        .advance(
            CursorScope::Address(4),
            ScanStream::TronTransfers,
            CursorBytes::copied(b"not json"),
        )
        .await
        .unwrap();
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(records(&sink.drain()).len(), 2, "a cache miss costs a rescan, not correctness");
}

#[tokio::test]
async fn a_history_route_no_endpoint_serves_never_costs_a_balance() {
    // A plain full node answers 404 for the /v1 routes, which is a statement about
    // that endpoint rather than about the wallet.
    let fake = balances_only(OWNER)
        .on(Route::NativeHistory, Answer::Status(404))
        .on(Route::TokenHistory, Answer::Status(404))
        .always(Route::InternalHistory, Answer::Status(404));
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();
    let progress = pass(&harness, &sink).await.unwrap();

    let events = sink.drain();
    assert_eq!(balances(&events).len(), 2, "every balance still reported");
    assert!(records(&events).is_empty());
    assert!(
        progress.synced_height().is_none(),
        "a wallet whose history could not be read is not synced to the tip"
    );
    assert_eq!(harness.cursors.len(), 0, "no cursor moves over history nobody read");
}

#[tokio::test]
async fn one_address_that_will_not_answer_does_not_abort_the_pass() {
    let fake = quiet_history(&balances_only(TRX_OWNER)).on_matching(
        Route::Account,
        OWNER,
        Answer::Transport,
    );
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(1, OWNER), watched(4, TRX_OWNER)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let events = sink.drain();
    assert!(
        balances(&events).iter().any(|(id, _, _)| *id == 4),
        "the address that answered still reported"
    );
    assert!(
        !balances(&events).iter().any(|(id, _, _)| *id == 1),
        "the address that did not answer reports nothing rather than a confident zero"
    );
    assert!(events.iter().any(|event| matches!(*event, WalletEvent::SyncProgress(_))));
}

#[tokio::test]
async fn a_token_that_will_not_answer_does_not_cost_the_coin() {
    let usdt = TokenId::UsdtTrc20.contract();
    let fake = quiet_history(&balances_only(OWNER)).on_matching(
        Route::ConstantContract,
        usdt,
        Answer::Transport,
    );
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let seen = balances(&sink.drain());
    assert!(seen.iter().any(|(_, asset, _)| *asset == TRX));
    assert_eq!(seen.len(), 1, "the token that would not answer is absent, not zero");
}

#[tokio::test]
async fn an_expired_deadline_stops_the_pass_before_any_address_work() {
    let fake = quiet_history(&balances_only(OWNER));
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();
    let progress = harness.adapter.sync(Deadline::EXPIRED, &sink).await.unwrap();
    assert_eq!(progress.tip_height(), Some(85_304_913), "the tip is read before anything else");
    assert!(sink.is_empty(), "no address work starts after the deadline");
}

/// Each native history page takes a second, on the runtime timer and on the
/// adapter's clock alike, the way a rate-limited indexer answers.
#[derive(Debug)]
struct SlowHistory {
    inner: Arc<FakeNode>,
    clock: Arc<FakeClock>,
}

#[async_trait]
impl TronNode for SlowHistory {
    async fn call(&self, request: &Request) -> Result<Answered, TronError> {
        if request.route() == Route::NativeHistory {
            sleep(Duration::from_secs(1)).await;
            self.clock.advance(Duration::from_secs(1));
        }
        self.inner.call(request).await
    }

    async fn broadcast(
        &self,
        preferred: &Request,
        fallback: &Request,
    ) -> Result<Answered, TronError> {
        self.inner.broadcast(preferred, fallback).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_pass_whose_deadline_falls_during_a_page_returns_the_walk_it_made() {
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, Answer::body(fixture!("native_page1")))
        .on(Route::NativeHistory, Answer::body(fixture!("native_page2")))
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let clock = Arc::new(FakeClock::default());
    let cursors = Arc::new(MemoryCursorStore::new());
    let slow = Arc::new(SlowHistory { inner: fake, clock: Arc::clone(&clock) });
    let adapter = TronAdapter::new(
        slow,
        Arc::<MemoryCursorStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&clock),
    );
    adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();

    // The first page is still being read when the deadline passes.
    let sink = FakeEventSink::new();
    let deadline = clock.deadline_in(Duration::from_millis(500));
    let progress = adapter
        .sync(deadline, &sink)
        .await
        .expect("a pass that ran out of time stops with what it has");
    assert_eq!(progress.tip_height(), Some(85_304_913));
    assert!(
        cursors
            .load_scan(CursorScope::Address(4), ScanStream::TronTransfers)
            .await
            .unwrap()
            .is_some(),
        "the page that was read is kept for the next pass"
    );
}

#[tokio::test]
async fn an_empty_watch_set_is_a_complete_pass() {
    let harness = Harness::new(&funded(1));
    harness.adapter.set_watch_set(&[]).await.unwrap();
    let sink = FakeEventSink::new();
    let progress = pass(&harness, &sink).await.unwrap();
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
    assert_eq!(progress.synced_height(), Some(85_304_913));
    assert!(sink.is_empty());
}

#[tokio::test]
async fn a_watch_set_row_from_another_chain_is_refused() {
    let harness = Harness::new(&funded(1));
    let foreign = WatchedAddressBuilder::new(
        1,
        ChainId::Bitcoin,
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
    )
    .build();
    let refused = harness.adapter.set_watch_set(&[foreign]).await;
    assert!(matches!(refused, Err(ChainError::Unsupported(_))), "{refused:?}");
}

#[tokio::test]
async fn dropping_an_address_from_the_watch_set_stops_following_its_sends() {
    let fake = quiet_history(&funded(100_000_000))
        .on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    harness.adapter.set_watch_set(&[]).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0, "a dropped address leaves nothing to follow");
    let progress = pass(&harness, &FakeEventSink::new()).await.unwrap();
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
}

const TOKEN_TXID: &str = "abe1ec9aee76590c0d69681c7ac9f4f97c95d9516271244dd3d979b44c80a09d";

#[tokio::test]
async fn rows_between_a_full_forward_budget_and_the_frontier_are_reported_on_a_later_pass() {
    const OLDEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SKIPPED: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const NEWER: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const NEWEST: &str = "4444444444444444444444444444444444444444444444444444444444444444";
    let sent = |txid: &str, timestamp_millis: i64| {
        transfer_row(txid, timestamp_millis, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5)
    };
    let oldest = sent(OLDEST, 1_786_585_000_000);
    let skipped = sent(SKIPPED, 1_786_585_100_000);
    let newer = sent(NEWER, 1_786_585_200_000);
    let newest = sent(NEWEST, 1_786_585_300_000);
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[oldest.as_str()], None))
        .on(Route::NativeHistory, history_page(&[newest.as_str(), newer.as_str()], Some("FpOlder")))
        .on_matching(
            Route::NativeHistory,
            "fingerprint=FpOlder",
            history_page(&[skipped.as_str(), oldest.as_str()], None),
        )
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let config = TronConfig { forward_pages: 1, ..TronConfig::default() };
    let harness = Harness::with(&fake, config);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    let first = FakeEventSink::new();
    pass(&harness, &first).await.unwrap();
    assert_eq!(records(&first.drain()).len(), 1, "the whole history is one row at first");

    // Three rows arrive, more than one forward page holds, and the balance shows
    // the fifteen sun they sent.
    fake.always(Route::Account, account(TRX_OWNER, 6_999_985));
    let mut reported: Vec<String> = Vec::new();
    for _ in 0..4u8 {
        let sink = FakeEventSink::new();
        pass(&harness, &sink).await.unwrap();
        reported.extend(
            records(&sink.drain()).into_iter().map(|record| record.txid.value().to_owned()),
        );
    }
    assert!(reported.iter().any(|txid| txid == NEWEST), "{reported:?}");
    assert!(
        reported.iter().any(|txid| txid == SKIPPED),
        "four passes never read the row between the forward page and the frontier: {reported:?}"
    );
}

#[tokio::test]
async fn a_trc20_transfer_from_the_indexer_is_recorded_in_its_block() {
    // The row shape api.trongrid.io answered on 2026-10-05, which has no block number.
    let page = format!(
        r#"{{"data":[{{"transaction_id":"{TOKEN_TXID}","token_info":{{"symbol":"USDT",
            "address":"{USDT}","decimals":6,"name":"Tether USD"}},"block_timestamp":1786585806000,
            "from":"{TRX_OWNER}","to":"{TOKEN_HISTORY}","type":"Transfer","value":"125000000"}}],
            "success":true,"meta":{{"at":1786586066075,"page_size":1}}}}"#
    );
    let receipt = format!(
        r#"{{"id":"{TOKEN_TXID}","blockNumber":85304913,"blockTimeStamp":1786585806000,
            "receipt":{{"result":"SUCCESS"}}}}"#
    );
    let fake = balances_only(TOKEN_HISTORY)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(&page))
        .on(Route::TransactionInfoById, Answer::body(&receipt))
        .on(Route::TransactionById, Answer::body(&format!(r#"{{"txID":"{TOKEN_TXID}"}}"#)));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(9, TOKEN_HISTORY)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let reported = records(&sink.drain());
    let record = reported.first().expect("the transfer is reported");
    assert_eq!(record.asset, USDT_ASSET);
    assert_eq!(record.direction, TxDirection::Incoming);
    assert!(
        record.height.is_some(),
        "a mined transfer stored without a height reads as pending and is never revisited"
    );
}

/// `TronGrid` lists a TRC-20 transfer, unconfirmed ones included, with no block
/// number, so its block comes from a full node's receipt. A full node a block
/// behind the indexer answers `{}` for it at first.
#[tokio::test]
async fn an_incoming_trc20_transfer_listed_before_its_receipt_is_placed_once_a_node_has_it() {
    let row = format!(
        r#"{{"transaction_id":"{TOKEN_TXID}","token_info":{{"symbol":"USDT",
            "address":"{USDT}","decimals":6,"name":"Tether USD"}},"block_timestamp":1786585806000,
            "from":"{TRX_OWNER}","to":"{TOKEN_HISTORY}","type":"Transfer","value":"125000000"}}"#
    );
    let receipt = format!(
        r#"{{"id":"{TOKEN_TXID}","blockNumber":85304913,"blockTimeStamp":1786585806000,
            "receipt":{{"result":"SUCCESS"}}}}"#
    );
    let fake = balances_only(TOKEN_HISTORY)
        .on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .always(Route::TokenHistory, history_page(&[row.as_str()], None))
        .on(Route::TransactionInfoById, Answer::body("{}"))
        .on(Route::TransactionInfoById, Answer::body(&receipt));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(9, TOKEN_HISTORY)]).await.unwrap();

    let mut heights = Vec::new();
    for _ in 0..4u8 {
        let sink = FakeEventSink::new();
        pass(&harness, &sink).await.unwrap();
        heights.extend(
            records(&sink.drain())
                .into_iter()
                .filter(|record| record.txid.value() == TOKEN_TXID)
                .map(|record| record.height),
        );
        harness.clock.advance(HISTORY_SWEEP);
    }
    assert!(
        heights.iter().any(Option::is_some),
        "the transfer is reported in block 85304913 once a node has its receipt, because the \
         store keeps an unplaced report as pending for good: {heights:?}"
    );
}

#[tokio::test]
async fn the_trx_a_trc20_send_burned_is_on_its_history_record() {
    let call = format!(
        r#"{{"txID":"{TOKEN_TXID}","blockNumber":85304913,"block_timestamp":1786585806000,
            "ret":[{{"contractRet":"SUCCESS","fee":13026600}}],
            "raw_data":{{"contract":[{{"type":"TriggerSmartContract","parameter":{{"value":{{
              "data":"a9059cbb000000000000000000000000df0c86c531205eb7a4e27d9589ba7188dfb32f080000000000000000000000000000000000000000000000000000000000004c4b40",
              "owner_address":"{TRX_OWNER_HEX}",
              "contract_address":"41a614f803b6fd780986a42c78ec9c7f77e6ded13c"}},
              "type_url":"type.googleapis.com/protocol.TriggerSmartContract"}}}}]}}}}"#
    );
    let transfer = format!(
        r#"{{"transaction_id":"{TOKEN_TXID}","token_info":{{"address":"{USDT}"}},
            "block_timestamp":1786585806000,"from":"{TRX_OWNER}","to":"{TRX_RECIPIENT}",
            "type":"Transfer","value":"5000000"}}"#
    );
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[call.as_str()], None))
        .on(Route::TokenHistory, history_page(&[transfer.as_str()], None));
    let harness = Harness::new(&fake);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();

    let reported = records(&sink.drain());
    assert!(reported.iter().any(|record| record.txid.value() == TOKEN_TXID), "{reported:?}");
    assert!(
        reported.iter().any(|record| record.txid.value() == TOKEN_TXID
            && record.fee == Some(Amount::from_base_units(13_026_600))),
        "the 13.0266 TRX this send burned is on no record: {reported:?}"
    );
}
