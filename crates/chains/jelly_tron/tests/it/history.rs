//! When a pass asks the indexer for an address's history. The balances every
//! pass reads anyway decide it, so a quiet address costs the indexer nothing
//! between sweeps.

use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    ChainAdapter as _, TxRecord,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore, records},
};
use jelly_tron::{
    Route, TronAdapter, TronConfig,
    test_util::{
        OWNER, OWNER_KEY, RECIPIENT, TRX_OWNER, TRX_OWNER_HEX, TRX_RECIPIENT, TRX_RECIPIENT_HEX,
        USDT,
    },
};

use crate::common::{
    Answer, FakeNode, HISTORY_SWEEP, Harness, TRX, USDT_ASSET, account, balances, balances_only,
    fixture, history_page, key_material, pass, quiet_history, ready, request, sender, transfer_row,
    watched, watching_owner,
};

/// What `balanceOf` answers for a holding of `units`.
fn holding(units: u128) -> Answer {
    Answer::body(&format!(r#"{{"constant_result":["{units:064x}"],"result":{{"result":true}}}}"#))
}

/// The history pages each stream read since the last call, as native, TRC-20
/// and internal.
fn reads(harness: &Harness) -> [usize; 3] {
    let calls = harness.fake.calls();
    harness.fake.forget_calls();
    [Route::NativeHistory, Route::TokenHistory, Route::InternalHistory]
        .map(|route| calls.iter().filter(|call| call.route == route).count())
}

fn txids(reported: &[TxRecord]) -> Vec<&str> {
    reported.iter().map(|record| record.txid.value()).collect()
}

/// A keyless `TronGrid` budget does not cover three history pages per address
/// every twelve seconds, so a pass whose balances did not move leaves the indexer
/// alone.
#[tokio::test]
async fn a_pass_that_sees_no_news_reads_no_history() {
    let fake = quiet_history(&balances_only(TRX_OWNER));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 1], "the first pass reads every stream");

    for _ in 0..3u8 {
        harness.clock.advance(Duration::from_secs(12));
        pass(&harness, &sink).await.unwrap();
        assert_eq!(reads(&harness), [0, 0, 0], "nothing moved, so nothing was asked");
    }
    assert_eq!(balances(&sink.drain()).len(), 12, "every pass still reports every balance");
}

#[tokio::test]
async fn a_trx_balance_change_is_read_in_the_pass_that_sees_it() {
    let fake = quiet_history(&balances_only(TRX_OWNER));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    harness.fake.forget_calls();

    // One TRX arrives, and the indexer already lists it.
    let paid = "c1".repeat(32);
    let row = transfer_row(&paid, 1_786_585_800_000, TRX_RECIPIENT_HEX, TRX_OWNER_HEX, 1_000_000);
    fake.always(Route::NativeHistory, history_page(&[row.as_str()], None))
        .always(Route::Account, account(TRX_OWNER, 8_000_000));
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(
        reads(&harness),
        [1, 1, 0],
        "the native row explains the move, so no contract's transfer is looked for"
    );
    assert_eq!(txids(&records(&sink.drain())), vec![paid.as_str()]);

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "reported news is not read again");
}

#[tokio::test]
async fn a_token_balance_change_is_read_in_the_pass_that_sees_it() {
    let fake = quiet_history(&balances_only(TRX_OWNER));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    harness.fake.forget_calls();

    // Five USDT base units arrive, and the TRX balance does not move.
    let paid = "d1".repeat(32);
    let row = format!(
        r#"{{"transaction_id":"{paid}","token_info":{{"address":"{USDT}"}},
            "block_timestamp":1786585800000,"block_number":85304900,
            "from":"{TRX_RECIPIENT}","to":"{TRX_OWNER}","type":"Transfer","value":"5"}}"#
    );
    fake.on_matching(Route::ConstantContract, USDT, holding(30_577_741_507))
        .always(Route::TokenHistory, history_page(&[row.as_str()], None));
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 0], "no TRX moved, so no contract's transfer is looked for");
    let reported = records(&sink.drain());
    assert_eq!(txids(&reported), vec![paid.as_str()]);
    assert_eq!(reported.first().map(|record| record.asset), Some(USDT_ASSET));

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "reported news is not read again");
}

/// A send of the address's own is read on every pass that reports it, landed or
/// settled, even before any balance shows it.
#[tokio::test]
async fn an_own_send_is_read_as_it_lands_and_settles() {
    let fake = sender();
    let harness = ready(&fake).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    harness.fake.forget_calls();

    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();

    // It lands one block deep, and the balance does not show it yet.
    fake.on(
        Route::TransactionInfoById,
        Answer::body(r#"{"id":"aa","blockNumber":85304913,"fee":0}"#),
    );
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 0], "a send landing is news");

    // It settles, and the wallet stops following it.
    fake.always(
        Route::TransactionInfoById,
        Answer::body(r#"{"id":"aa","blockNumber":85304890,"fee":0}"#),
    );
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 0], "a send settling is news");
    assert_eq!(harness.adapter.in_flight(), 0);

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "a send no longer followed is no news");
}

/// A walk still filling in, or stopped short of its frontier, is read every pass
/// until it is done, whatever the balances say.
#[tokio::test]
async fn an_unfinished_walk_stays_due_until_it_is_done() {
    let txid = |n: i64| format!("{n:02x}").repeat(32);
    let row = |n: i64| {
        transfer_row(&txid(n), 1_786_585_000_000 + n * 100_000, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5)
    };
    let [r1, r2, r3, r4, r5, r6] = [1, 2, 3, 4, 5, 6].map(row);
    let fake = balances_only(TRX_OWNER)
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .on(Route::NativeHistory, history_page(&[r3.as_str(), r2.as_str()], Some("FpOlder")))
        .on_matching(
            Route::NativeHistory,
            "fingerprint=FpOlder",
            history_page(&[r1.as_str()], Some("FpOldest")),
        )
        .on_matching(
            Route::NativeHistory,
            "fingerprint=FpOldest",
            Answer::body(fixture!("page_empty")),
        );
    let config = TronConfig { forward_pages: 1, backfill_pages: 1, ..TronConfig::default() };
    let harness = watching_owner(&fake, config).await.unwrap();
    let sink = FakeEventSink::new();

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [2, 1, 1], "the newest page and one older page");
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [2, 0, 0], "the backfill goes on with nothing new");
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "and stops once it reaches the end");
    assert_eq!(records(&sink.drain()).len(), 3);

    // Three sends leave, more than one forward page holds.
    fake.always(Route::NativeHistory, history_page(&[r6.as_str(), r5.as_str()], Some("FpNewer")))
        .on_matching(
            Route::NativeHistory,
            "fingerprint=FpNewer",
            history_page(&[r4.as_str(), r3.as_str()], None),
        )
        .always(Route::Account, account(TRX_OWNER, 6_999_985));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 1]);

    // Past the news, the walk left short of its frontier is still read.
    harness.clock.advance(Duration::from_secs(61));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 0, 0]);
    assert_eq!(txids(&records(&sink.drain())), vec![txid(6), txid(5), txid(4)]);

    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "caught up and nothing new");
}

/// An address the adapter has not read since it was built is read on its first
/// pass, whatever its balances say, and so is one it stopped watching and then
/// watched again.
#[tokio::test]
async fn a_cold_address_is_read_on_its_first_pass() {
    let fake = quiet_history(&balances_only(TRX_OWNER)).on_matching(
        Route::Account,
        OWNER,
        account(OWNER, 1),
    );
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    let history_of = |address: &str| {
        let calls = harness.fake.calls();
        calls.iter().filter(|call| call.route.is_indexer() && call.path.contains(address)).count()
    };
    let both = [watched(4, TRX_OWNER), watched(1, OWNER)];

    harness.fake.forget_calls();
    harness.adapter.set_watch_set(&both).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(history_of(OWNER), 3, "a new address is read on every stream at once");
    assert_eq!(history_of(TRX_OWNER), 0, "the address already read had no news");

    // Its walk is complete by now, so only being cold makes it due.
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    harness.fake.forget_calls();
    harness.adapter.set_watch_set(&both).await.unwrap();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(history_of(OWNER), 3, "an address watched again is read at once");
    assert_eq!(history_of(TRX_OWNER), 0);
}

/// What decides a read lives in memory, so a restart reads every address once,
/// and finds what arrived while the wallet was closed.
#[tokio::test]
async fn a_restart_reads_every_address_once() {
    let oldest =
        transfer_row(&"e0".repeat(32), 1_786_585_000_000, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5);
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[oldest.as_str()], None))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    harness.fake.forget_calls();

    // A send leaves while the wallet is closed.
    let sent = "e1".repeat(32);
    let newer = transfer_row(&sent, 1_786_585_100_000, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5);
    fake.always(Route::NativeHistory, history_page(&[newer.as_str(), oldest.as_str()], None))
        .always(Route::Account, account(TRX_OWNER, 6_999_995));
    let restarted = TronAdapter::new(
        Arc::<FakeNode>::clone(&fake),
        Arc::<MemoryCursorStore>::clone(&harness.cursors),
        Arc::<FakeClock>::clone(&harness.clock),
    );
    restarted.set_watch_set(&[watched(4, TRX_OWNER)]).await.unwrap();

    let sink = FakeEventSink::new();
    restarted.sync(harness.deadline(), &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 1], "a restart reads every stream once");
    assert_eq!(txids(&records(&sink.drain())), vec![sent.as_str()]);

    restarted.sync(harness.deadline(), &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "and then only on news");
}

#[tokio::test]
async fn the_sweep_reads_a_quiet_address_once_its_interval_has_passed() {
    let fake = quiet_history(&balances_only(TRX_OWNER));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    harness.fake.forget_calls();

    harness.clock.advance(HISTORY_SWEEP.saturating_sub(Duration::from_secs(1)));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "the sweep is not due yet");

    harness.clock.advance(Duration::from_secs(1));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 1], "the sweep reads every stream");
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "and then waits another interval");
}

/// The node shows a send before the indexer lists it. The address is read again
/// on the next pass, and once the indexer has reported the send it goes quiet.
#[tokio::test]
async fn a_change_the_indexer_reports_a_pass_late_is_read_again_and_then_left_alone() {
    let oldest =
        transfer_row(&"f0".repeat(32), 1_786_585_000_000, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5);
    let fake = balances_only(TRX_OWNER)
        .on(Route::NativeHistory, history_page(&[oldest.as_str()], None))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    harness.fake.forget_calls();

    fake.always(Route::Account, account(TRX_OWNER, 6_999_995));
    let sink = FakeEventSink::new();
    pass(&harness, &sink).await.unwrap();
    assert_eq!(
        reads(&harness),
        [1, 1, 1],
        "nothing explains the move yet, so a contract's transfer is looked for too"
    );
    assert!(records(&sink.drain()).is_empty(), "the indexer has nothing yet");

    let sent = "f1".repeat(32);
    let newer = transfer_row(&sent, 1_786_585_100_000, TRX_OWNER_HEX, TRX_RECIPIENT_HEX, 5);
    fake.always(Route::NativeHistory, history_page(&[newer.as_str(), oldest.as_str()], None));
    harness.clock.advance(Duration::from_secs(12));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [1, 1, 0], "unreported news is read again");
    assert_eq!(txids(&records(&sink.drain())), vec![sent.as_str()]);

    harness.clock.advance(Duration::from_secs(12));
    pass(&harness, &sink).await.unwrap();
    assert_eq!(reads(&harness), [0, 0, 0], "reported news is not read again");
}

/// TRX staked or unstaked moves the balance with no transfer to list, so its
/// news is let go once the window passes rather than read forever.
#[tokio::test]
async fn a_move_no_transfer_explains_is_read_only_for_a_short_window() {
    let fake = quiet_history(&balances_only(TRX_OWNER));
    let harness = watching_owner(&fake, TronConfig::default()).await.unwrap();
    pass(&harness, &FakeEventSink::new()).await.unwrap();
    harness.fake.forget_calls();

    fake.always(Route::Account, account(TRX_OWNER, 6_000_000));
    let mut seen = Vec::new();
    for _ in 0..10u8 {
        pass(&harness, &FakeEventSink::new()).await.unwrap();
        seen.push(reads(&harness));
        harness.clock.advance(Duration::from_secs(12));
    }
    let mut expected = vec![[1, 1, 1]; 5];
    expected.extend([[0, 0, 0]; 5]);
    assert_eq!(seen, expected, "every pass for a minute and then no more");
}
