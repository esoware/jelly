use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{Clock, Deadline, random_bytes};
use tokio::sync::watch;
use tokio::time::sleep;

use crate::error::PriceError;
use crate::feed::PriceFeed;
use crate::fetch::Fetch;
use crate::quote::{QuoteRequest, Quotes};
use crate::schedule::Schedule;
use crate::switch::{wait_until_disabled, wait_until_enabled};
use crate::ticker::Ticker;

pub(crate) const TARGET: &str = "jelly::prices";

/// What became of the one request a pass may send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PassOutcome {
    /// No ticker is configured, so nothing was asked.
    #[default]
    NotSent,
    /// Held back because the provider had asked to be left alone.
    Resting,
    /// The request failed outright, or the pass never reached it.
    Failed,
    /// The body decoded.
    Answered,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PassReport {
    pub(crate) outcome: PassOutcome,
    /// Tickers that came back with a usable price.
    pub(crate) quoted: usize,
    /// Tickers an answered request declined to price.
    pub(crate) rejected: usize,
    /// Configured tickers this pass did not quote.
    pub(crate) missing: usize,
    /// Currencies that came back with a usable rate.
    pub(crate) rated: usize,
    /// Currencies an answered request declined to rate.
    pub(crate) unrated: usize,
    /// Price and rate records whose value moved.
    pub(crate) moved: usize,
    /// Price and rate records re-confirmed at the same value.
    pub(crate) refreshed: usize,
    /// Price and rate records dropped for being too old to show.
    pub(crate) discarded: usize,
    /// How long the provider asked to be left alone.
    pub(crate) retry_after: Option<Duration>,
}

impl PassReport {
    /// True when the pass failed outright or got an answer that priced nothing, either of
    /// which backs the feed off. A provider that answers about the wrong pair is as much
    /// an outage as one that doesn't answer.
    ///
    /// A pass held back by a rest, or with no ticker configured, isn't a failure.
    pub(crate) const fn is_failure(&self) -> bool {
        self.quoted == 0 && (matches!(self.outcome, PassOutcome::Failed) || self.rejected > 0)
    }
}

/// Fills a [`PriceFeed`] and owns all polling state without a lock, because there's
/// exactly one poller and it belongs to one task.
#[derive(Debug)]
pub struct PricePoller {
    feed: PriceFeed,
    /// `None` when no ticker is configured.
    request: Option<QuoteRequest>,
    fetch: Arc<dyn Fetch>,
    clock: Arc<dyn Clock>,
    schedule: Schedule,
    /// The provider isn't asked again before this.
    resting_until: Deadline,
}

impl PricePoller {
    pub(crate) fn new(
        feed: PriceFeed,
        tickers: &BTreeSet<Ticker>,
        fetch: Arc<dyn Fetch>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let request = QuoteRequest::for_tickers(tickers);
        let seed = random_bytes::<8>()
            .map_or_else(|_| clock.now_millis().unwrap_or(0), |bytes| u64::from_le_bytes(*bytes));
        let schedule = Schedule::new(seed);
        Self { feed, request, fetch, clock, schedule, resting_until: Deadline::EXPIRED }
    }

    /// One pass over every configured ticker and every rate.
    pub(crate) async fn poll_once(&mut self) -> Result<PassReport, PriceError> {
        let now_unix_secs = self.clock.now_secs()?;
        let (outcome, quotes, retry_after) = match &self.request {
            None => (PassOutcome::NotSent, Quotes::default(), None),
            Some(_) if !self.resting_until.is_expired(self.clock.as_ref()) => {
                (PassOutcome::Resting, Quotes::default(), None)
            }
            Some(request) => {
                let answer = self.fetch.fetch_quotes(request).await;
                match answer.and_then(|body| request.decode(&body)) {
                    Ok(quotes) => {
                        log_rejections(&quotes);
                        (PassOutcome::Answered, quotes, None)
                    }
                    Err(error) => {
                        // Honors a `Retry-After`, so the provider isn't asked again
                        // until it said it would be ready, however often the feed polls.
                        let retry_after = error.retry_after(now_unix_secs);
                        if let Some(wait) = retry_after {
                            self.resting_until = self.clock.deadline_in(wait);
                        }
                        tracing::warn!(target: TARGET, %error, "a price request failed");
                        (PassOutcome::Failed, Quotes::default(), retry_after)
                    }
                }
            }
        };

        let published = self.feed.publish(&quotes, now_unix_secs);
        let asked = self.request.as_ref().map_or(0, |request| request.tickers().len());
        let report = PassReport {
            outcome,
            quoted: quotes.accepted.len(),
            rejected: quotes.rejected.len(),
            missing: asked.saturating_sub(quotes.accepted.len()),
            rated: quotes.rates.len(),
            unrated: quotes.unrated.len(),
            moved: published.moved,
            refreshed: published.refreshed,
            discarded: published.discarded,
            retry_after,
        };
        log_pass(&report);
        Ok(report)
    }

    /// Polls until `enabled`'s sender is dropped.
    ///
    /// A `false` value suspends the feed without ending the task, so turning
    /// prices off costs nothing and turning them back on needs no restart. It also
    /// withdraws every price and rate, so nothing is shown while prices are off.
    pub async fn run(mut self, mut enabled: watch::Receiver<bool>) {
        loop {
            if !*enabled.borrow_and_update() {
                self.feed.clear();
                if !wait_until_enabled(&mut enabled).await {
                    return;
                }
            }
            let report = match self.poll_once().await {
                Ok(report) => report,
                Err(error) => {
                    tracing::warn!(target: TARGET, %error, "the price pass didn't run");
                    // A pass that never reached its request still counts as failed, so
                    // the schedule backs off.
                    PassReport { outcome: PassOutcome::Failed, ..PassReport::default() }
                }
            };
            let delay = self.schedule.next_delay(report);
            if !self.expire_while(wait_out(delay, &mut enabled)).await {
                return;
            }
        }
    }

    /// Awaits `waiting`, dropping each record the moment it grows too old to show,
    /// so a feed that is resting serves nothing older than
    /// [`DISCARD_AFTER`](crate::book::DISCARD_AFTER) either.
    async fn expire_while(&self, waiting: impl Future<Output = bool>) -> bool {
        tokio::pin!(waiting);
        loop {
            let until_expiry = self
                .clock
                .now_secs()
                .ok()
                .and_then(|now_unix_secs| self.feed.until_expiry(now_unix_secs));
            tokio::select! {
                is_running = &mut waiting => return is_running,
                () = sleep(until_expiry.unwrap_or_default()), if until_expiry.is_some() => {
                    if let Ok(now_unix_secs) = self.clock.now_secs() {
                        let published = self.feed.publish(&Quotes::default(), now_unix_secs);
                        tracing::debug!(
                            target: TARGET,
                            discarded = published.discarded,
                            "prices too old to show were dropped"
                        );
                    }
                }
            }
        }
    }
}

fn log_rejections(quotes: &Quotes) {
    for rejected in &quotes.rejected {
        tracing::debug!(
            target: TARGET,
            ticker = rejected.ticker.coin().as_str(),
            reason = rejected.reason.as_str(),
            "a quote wasn't usable"
        );
    }
    for unrated in &quotes.unrated {
        tracing::debug!(
            target: TARGET,
            currency = unrated.currency.as_str(),
            reason = unrated.reason.as_str(),
            "a rate wasn't usable"
        );
    }
}

fn log_pass(report: &PassReport) {
    if report.is_failure() {
        tracing::warn!(
            target: TARGET,
            outcome = ?report.outcome,
            rejected = report.rejected,
            missing = report.missing,
            "no price was updated"
        );
        return;
    }
    tracing::debug!(
        target: TARGET,
        outcome = ?report.outcome,
        quoted = report.quoted,
        rated = report.rated,
        unrated = report.unrated,
        moved = report.moved,
        refreshed = report.refreshed,
        missing = report.missing,
        discarded = report.discarded,
        "a price pass finished"
    );
}

/// Sleeps out the cadence, returning early only when the feed is switched off.
///
/// A repeated `true` doesn't shorten the wait. Switching off ends it, so turning prices
/// back on polls at once. False once the sender is gone.
async fn wait_out(delay: Duration, enabled: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = sleep(delay) => true,
        is_running = wait_until_disabled(enabled) => is_running,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use jelly_core::{
        AssetId, ChainId, Coin, FailureKind, FiatCurrency, TokenId, test_util::FakeClock,
    };
    use jelly_net::{NetError, RetryAfter};
    use parking_lot::Mutex;
    use tokio::task::yield_now;
    use tokio::time::advance;

    use super::*;
    use crate::book::DISCARD_AFTER;
    use crate::candle::CandleRequest;
    use crate::config::PriceFeedConfig;
    use crate::fixtures::{kraken_key, kraken_rate_key, rated_currencies};
    use crate::kraken;

    const BTC: AssetId = AssetId::Native(ChainId::Bitcoin);
    const ETH: AssetId = AssetId::Native(ChainId::Ethereum);
    const BNB: AssetId = AssetId::Native(ChainId::Bsc);
    const XMR: AssetId = AssetId::Native(ChainId::Monero);
    const USDT_TRON: AssetId = AssetId::Token(TokenId::UsdtTrc20);

    const PRICED_ASSETS: usize = AssetId::COUNT;

    /// Every currency but the dollar has a rate.
    const RATED_CURRENCIES: usize = FiatCurrency::COUNT - 1;

    const fn assert_send_sync<T: Send + Sync>() {}

    /// The feed is read from every task that renders a balance, and the poller is
    /// spawned onto the runtime.
    const _: () = assert_send_sync::<PriceFeed>();
    const _: () = assert_send_sync::<PricePoller>();

    fn answering_everything() -> Harness {
        harness(ScriptedFetch::new(everything_at("100.0")))
    }

    #[tokio::test]
    async fn one_pass_prices_every_asset() {
        let mut harness = answering_everything();
        let report = harness.poller.poll_once().await.expect("the clock is sane");

        assert_eq!(report.outcome, PassOutcome::Answered);
        assert_eq!(report.quoted, Ticker::all().count(), "every ticker was priced");
        assert_eq!(report.missing, 0);
        assert_eq!(report.rated, RATED_CURRENCIES);
        assert_eq!(report.unrated, 0);
        assert_eq!(report.moved, PRICED_ASSETS + RATED_CURRENCIES);
        assert!(!report.is_failure());

        let snapshot = harness.feed.snapshot();
        assert_eq!(snapshot.currency(), FiatCurrency::Usd);
        assert_eq!(snapshot.len(), PRICED_ASSETS);
        let btc = snapshot.get(BTC).expect("bitcoin is priced");
        assert_eq!(btc.price.to_string(), "10000000000");
        assert_eq!(btc.as_of_unix_secs, harness.started_unix_secs);
        assert!(snapshot.get(BNB).is_some(), "bnb is priced");
        assert!(snapshot.get(XMR).is_some(), "monero is priced");
        assert_eq!(snapshot.get(USDT_TRON).map(|record| record.price), Some(btc.price));
    }

    #[tokio::test]
    async fn a_pass_sends_one_request_for_every_ticker_and_rate() {
        let mut harness = answering_everything();
        harness.poller.poll_once().await.expect("the clock is sane");

        assert_eq!(
            harness.fetch.calls(),
            [Call { tickers: Ticker::all().collect(), currencies: rated_currencies() }]
        );
    }

    #[tokio::test]
    async fn a_price_in_another_currency_is_the_dollar_price_times_the_rate() {
        let mut harness = answering_everything();
        harness.poller.poll_once().await.expect("the clock is sane");

        // Bitcoin is a hundred dollars, and one dollar buys half a euro and two yen.
        let cases = [
            (FiatCurrency::Eur, "5000000000"),
            (FiatCurrency::Jpy, "20000000000"),
            (FiatCurrency::Usd, "10000000000"),
        ];
        for (currency, expected) in cases {
            harness.feed.set_currency(currency);
            let snapshot = harness.feed.snapshot();
            assert_eq!(snapshot.currency(), currency);
            assert_eq!(snapshot.len(), PRICED_ASSETS, "{currency}");
            assert_eq!(
                snapshot.get(BTC).map(|record| record.price.to_string()),
                Some(expected.to_owned()),
                "{currency}"
            );
        }
    }

    #[tokio::test]
    async fn switching_currency_reprices_at_once_without_a_request() {
        let mut harness = answering_everything();
        harness.poller.poll_once().await.expect("the clock is sane");
        let mut watcher = harness.feed.subscribe();
        watcher.borrow_and_update();

        harness.feed.set_currency(FiatCurrency::Gbp);
        assert!(watcher.has_changed().expect("the feed is alive"), "the switch was not published");
        assert_eq!(watcher.borrow_and_update().currency(), FiatCurrency::Gbp);
        assert_eq!(harness.fetch.count(), 1, "the switch sent a request");

        harness.feed.set_currency(FiatCurrency::Gbp);
        assert!(
            !watcher.has_changed().expect("the feed is alive"),
            "the same currency is not news"
        );
    }

    #[tokio::test]
    async fn without_its_rate_a_currency_shows_no_price_at_all() {
        let mut harness = harness(ScriptedFetch::new(|_| {
            Reply::Body(kraken_body(&[(Ticker::of(Coin::Btc), "100.0")]))
        }));
        let report = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(report.rated, 0);
        assert_eq!(report.unrated, RATED_CURRENCIES);
        assert!(!report.is_failure(), "a pass that priced a coin is not an outage");

        harness.feed.set_currency(FiatCurrency::Chf);
        let snapshot = harness.feed.snapshot();
        assert_eq!(snapshot.currency(), FiatCurrency::Chf);
        assert!(snapshot.is_empty(), "a dollar price was shown as francs");

        harness.feed.set_currency(FiatCurrency::Usd);
        assert!(harness.feed.snapshot().get(BTC).is_some());
    }

    #[tokio::test]
    async fn a_converted_price_is_as_old_as_its_older_quote() {
        let is_rating = Arc::new(AtomicBool::new(true));
        let script = everything_at("100.0");
        let switch = Arc::clone(&is_rating);
        let mut harness = harness(ScriptedFetch::new(move |request| {
            if switch.load(Ordering::SeqCst) {
                script(request)
            } else {
                Reply::Body(kraken_body(&[(Ticker::of(Coin::Btc), "100.0")]))
            }
        }));
        harness.feed.set_currency(FiatCurrency::Aud);
        harness.poller.poll_once().await.expect("the clock is sane");

        is_rating.store(false, Ordering::SeqCst);
        harness.clock.advance(Duration::from_secs(60));
        harness.poller.poll_once().await.expect("the clock is sane");

        let btc =
            harness.feed.snapshot().get(BTC).expect("bitcoin is priced in australian dollars");
        assert_eq!(
            btc.as_of_unix_secs, harness.started_unix_secs,
            "the rate is a minute older than the price"
        );
        let book = harness.feed.subscribe().borrow().clone();
        let usd = book.usd_prices().get(&BTC).expect("a dollar price");
        assert_eq!(usd.as_of_unix_secs, harness.started_unix_secs.saturating_add(60));
    }

    #[tokio::test]
    async fn a_provider_that_asks_to_be_left_alone_is_left_alone() {
        let mut harness =
            harness(ScriptedFetch::new(|_| Reply::RateLimited(Duration::from_secs(600))));

        let first = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(first.outcome, PassOutcome::Failed);
        assert_eq!(first.retry_after, Some(Duration::from_secs(600)));
        assert!(first.is_failure());

        harness.fetch.clear_calls();
        harness.clock.advance(Duration::from_secs(120));
        let second = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(second.outcome, PassOutcome::Resting);
        assert!(!second.is_failure(), "waiting is not failing");
        assert_eq!(harness.fetch.count(), 0, "the wait was not honored");

        harness.clock.advance(Duration::from_secs(600));
        let third = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(third.outcome, PassOutcome::Failed);
        assert_eq!(third.retry_after, Some(Duration::from_secs(600)));
        assert_eq!(harness.fetch.count(), 1, "the wait expired");
    }

    #[tokio::test]
    async fn a_partial_answer_prices_only_what_it_names() {
        let mut harness = harness(ScriptedFetch::new(|_| {
            Reply::Body(kraken_body(&[(Ticker::of(Coin::Eth), "4218.07")]))
        }));

        let report = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(report.outcome, PassOutcome::Answered);
        assert_eq!(report.quoted, 1);
        assert_eq!(report.rejected, Ticker::all().count() - 1);
        assert_eq!(report.missing, Ticker::all().count() - 1);
        assert!(!report.is_failure());

        let snapshot = harness.feed.snapshot();
        assert_eq!(
            snapshot.len(),
            Ticker::of(Coin::Eth).assets().count(),
            "one quote prices ether everywhere"
        );
        assert!(snapshot.get(BTC).is_none(), "bitcoin wore ether's price");
        assert_eq!(
            snapshot.get(ETH).map(|record| record.price.to_string()),
            Some("421807000000".into())
        );
    }

    #[tokio::test]
    async fn a_repeated_price_refreshes_the_record_and_a_moved_one_changes_it() {
        let dial = Dial::new("1.0");
        let mut harness = harness(ScriptedFetch::new(dial.script()));
        let mut watcher = harness.feed.subscribe();

        harness.poller.poll_once().await.expect("the clock is sane");
        assert!(watcher.has_changed().expect("the feed is alive"));
        watcher.borrow_and_update();

        let again = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(again.moved, 0);
        assert_eq!(again.refreshed, 0, "the same quote at the same second is not news");
        assert!(!watcher.has_changed().expect("the feed is alive"));

        harness.clock.advance(Duration::from_secs(60));
        let later = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(later.moved, 0);
        assert_eq!(later.refreshed, PRICED_ASSETS + RATED_CURRENCIES);
        assert!(watcher.has_changed().expect("the feed is alive"), "the age of a price is news");
        let refreshed =
            watcher.borrow_and_update().snapshot().get(BTC).map(|record| record.as_of_unix_secs);
        assert_eq!(refreshed, Some(harness.started_unix_secs.saturating_add(60)));

        dial.set("2.0");
        harness.clock.advance(Duration::from_secs(60));
        let moved = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(moved.moved, PRICED_ASSETS);
        assert_eq!(moved.refreshed, RATED_CURRENCIES, "the rates held still");
        assert_eq!(
            harness.feed.snapshot().get(BTC).map(|record| record.price.to_string()),
            Some("200000000".into())
        );
    }

    #[tokio::test]
    async fn a_record_too_old_to_show_is_dropped() {
        let mut harness = answering_everything();
        harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(harness.feed.snapshot().len(), PRICED_ASSETS);

        harness.fetch.mute();
        harness.clock.advance(Duration::from_hours(25));
        let report = harness.poller.poll_once().await.expect("the clock is sane");

        assert!(report.is_failure());
        assert_eq!(report.discarded, PRICED_ASSETS + RATED_CURRENCIES);
        assert!(harness.feed.snapshot().is_empty(), "a day-old price is worse than none");
        assert!(harness.feed.subscribe().borrow().rates().is_empty(), "a day-old rate was kept");
    }

    #[tokio::test]
    async fn a_pass_that_answered_nothing_leaves_what_is_known_alone() {
        let mut harness = answering_everything();
        harness.poller.poll_once().await.expect("the clock is sane");
        let held = harness.feed.snapshot();

        harness.fetch.mute();
        harness.clock.advance(Duration::from_secs(60));
        let report = harness.poller.poll_once().await.expect("the clock is sane");

        assert_eq!(report.outcome, PassOutcome::Failed);
        assert_eq!(report.quoted, 0);
        assert!(report.is_failure());
        assert_eq!(report.discarded, 0);
        assert_eq!(harness.feed.snapshot(), held, "a failed pass disturbed what was known");
    }

    #[tokio::test(start_paused = true)]
    async fn the_switch_suspends_the_task_without_ending_it() {
        let harness = answering_everything();
        let (switch, enabled) = watch::channel(false);
        let fetch = Arc::clone(&harness.fetch);
        let mut watcher = harness.feed.subscribe();
        let task = tokio::spawn(harness.poller.run(enabled));

        yield_now().await;
        assert_eq!(fetch.count(), 0, "a suspended feed sent a request");

        switch.send_replace(true);
        watcher.changed().await.expect("the feed publishes its first pass");
        assert!(fetch.count() > 0);

        drop(switch);
        task.await.expect("the task ends when the switch is dropped");
    }

    #[tokio::test]
    async fn a_feed_asked_for_nothing_sends_nothing() {
        let config = PriceFeedConfig { tickers: BTreeSet::new() };
        let mut harness = harness_with(&config, ScriptedFetch::new(everything_at("100.0")));

        let report = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(report.outcome, PassOutcome::NotSent);
        assert!(!report.is_failure());
        assert_eq!(harness.fetch.count(), 0);
    }

    #[tokio::test]
    async fn a_body_that_prices_nothing_asked_for_leaves_the_snapshot_alone() {
        let config = PriceFeedConfig { tickers: BTreeSet::from([Ticker::of(Coin::Btc)]) };
        // An answer about a pair nobody asked about.
        let fetch =
            ScriptedFetch::new(|_| Reply::Body(kraken_body(&[(Ticker::of(Coin::Ltc), "96.31")])));
        let mut harness = harness_with(&config, fetch);

        let report = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(report.outcome, PassOutcome::Answered);
        assert_eq!(report.quoted, 0, "litecoin was not what the batch asked about");
        assert_eq!(report.rejected, 1);
        assert_eq!(report.missing, 1);
        assert!(report.is_failure(), "a request spent for nothing usable is an outage");
        assert!(harness.feed.snapshot().is_empty());
    }

    #[tokio::test]
    async fn the_newest_quote_wins_after_the_wall_clock_steps_back() {
        let dial = Dial::new("1.0");
        let mut harness = harness(ScriptedFetch::new(dial.script()));
        harness.poller.poll_once().await.expect("the clock is sane");

        let corrected_millis = FakeClock::DEFAULT_UNIX_MILLIS.saturating_sub(3_600_000);
        harness.clock.set_unix_millis(corrected_millis);
        dial.set("2.0");
        let report = harness.poller.poll_once().await.expect("the clock is sane");
        assert_eq!(report.quoted, Ticker::all().count());

        let btc = harness.feed.snapshot().get(BTC).expect("bitcoin is priced");
        assert_eq!(btc.price.to_string(), "200000000", "the newest quote was dropped");
        assert_eq!(btc.as_of_unix_secs, harness.clock.now_secs().expect("a sane fake clock"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_feed_resting_on_a_long_retry_after_still_drops_what_grew_too_old() {
        let is_limiting = Arc::new(AtomicBool::new(false));
        let limit = Arc::clone(&is_limiting);
        let script = everything_at("100.0");
        let harness = harness(ScriptedFetch::new(move |request| {
            if limit.load(Ordering::SeqCst) {
                Reply::RateLimited(Duration::from_hours(48))
            } else {
                script(request)
            }
        }));
        let (switch, enabled) = watch::channel(true);
        let fetch = Arc::clone(&harness.fetch);
        let clock = Arc::clone(&harness.clock);
        let feed = harness.feed.clone();
        let mut watcher = feed.subscribe();
        let task = tokio::spawn(harness.poller.run(enabled));

        watcher.changed().await.expect("the feed publishes its first pass");
        is_limiting.store(true, Ordering::SeqCst);
        // Past the longest jittered cadence, so the second pass is told to wait two days.
        advance(Duration::from_secs(76)).await;
        for _ in 0..8u8 {
            yield_now().await;
        }
        assert_eq!(fetch.count(), 2, "the second pass was not sent");

        let resting_for = DISCARD_AFTER.saturating_add(Duration::from_hours(1));
        clock.advance(resting_for);
        advance(resting_for).await;
        for _ in 0..8u8 {
            yield_now().await;
        }
        assert_eq!(fetch.count(), 2, "the provider's wait was not honored");
        assert!(feed.snapshot().is_empty(), "a resting feed served a price older than a day");
        assert!(feed.subscribe().borrow().rates().is_empty(), "a day-old rate was kept");

        drop(switch);
        task.await.expect("the task ends when the switch is dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn a_switched_off_feed_serves_no_price_until_it_is_switched_on_again() {
        let harness = answering_everything();
        let (switch, enabled) = watch::channel(true);
        let fetch = Arc::clone(&harness.fetch);
        let clock = Arc::clone(&harness.clock);
        let feed = harness.feed.clone();
        let mut watcher = feed.subscribe();
        let task = tokio::spawn(harness.poller.run(enabled));

        watcher.changed().await.expect("the feed publishes its first pass");
        assert!(feed.snapshot().get(BTC).is_some(), "bitcoin is priced");

        switch.send_replace(false);
        watcher.changed().await.expect("switching off is published");
        assert!(feed.snapshot().is_empty(), "a switched-off feed served a price");
        let sent = fetch.count();

        let switched_off_for = Duration::from_hours(1);
        clock.advance(switched_off_for);
        advance(switched_off_for).await;
        for _ in 0..8u8 {
            yield_now().await;
        }
        assert_eq!(fetch.count(), sent, "a switched-off feed sent a request");

        // Changing the currency is what makes the app publish the book again while the
        // feed is off.
        feed.set_currency(FiatCurrency::Eur);
        assert!(feed.snapshot().is_empty(), "a currency change brought a price back");
        let book = feed.subscribe().borrow().clone();
        assert!(book.usd_prices().is_empty(), "a switched-off feed kept a price");
        assert!(book.rates().is_empty(), "a switched-off feed kept a rate");

        switch.send_replace(true);
        watcher.wait_for(|book| !book.snapshot().is_empty()).await.expect("the feed is alive");
        assert_eq!(fetch.count(), sent.saturating_add(1), "switching on polls at once");

        drop(switch);
        task.await.expect("the task ends when the switch is dropped");
    }

    /// What the scripted provider quotes every dollar fiat pair at.
    ///
    /// Kraken lists the euro, the pound, and the Australian dollar in dollars, so
    /// one dollar buys half of each of them and two of every other currency.
    const PAIR_PRICE: &str = "2.0";

    fn kraken_body(quotes: &[(Ticker, &str)]) -> String {
        kraken_answer(quotes, &[])
    }

    fn kraken_answer(quotes: &[(Ticker, &str)], rates: &[(FiatCurrency, &str)]) -> String {
        let coins = quotes.iter().map(|&(ticker, price)| (kraken_key(ticker), price));
        let pairs = rates
            .iter()
            .filter_map(|&(currency, price)| kraken_rate_key(currency).map(|key| (key, price)));
        let entries: Vec<String> = coins
            .chain(pairs)
            .map(|(key, price)| format!("\"{key}\":{{\"c\":[\"{price}\",\"1.0\"]}}"))
            .collect();
        format!("{{\"error\":[],\"result\":{{{}}}}}", entries.join(","))
    }

    #[derive(Debug, Clone)]
    enum Reply {
        Body(String),
        RateLimited(Duration),
        Unreachable,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Call {
        tickers: Vec<Ticker>,
        currencies: Vec<FiatCurrency>,
    }

    /// A transport with no sockets, where a closure decides what each planned
    /// request answers and every call is recorded.
    struct ScriptedFetch {
        reply: Box<dyn Fn(&QuoteRequest) -> Reply + Send + Sync>,
        calls: Mutex<Vec<Call>>,
        is_muted: AtomicBool,
    }

    impl fmt::Debug for ScriptedFetch {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ScriptedFetch")
                .field("calls", &self.calls.lock().len())
                .finish_non_exhaustive()
        }
    }

    impl ScriptedFetch {
        fn new(reply: impl Fn(&QuoteRequest) -> Reply + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                reply: Box::new(reply),
                calls: Mutex::new(Vec::new()),
                is_muted: AtomicBool::new(false),
            })
        }

        /// Every later request is unreachable, whatever the script says.
        fn mute(&self) {
            self.is_muted.store(true, Ordering::SeqCst);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().clone()
        }

        fn count(&self) -> usize {
            self.calls.lock().len()
        }

        fn clear_calls(&self) {
            self.calls.lock().clear();
        }
    }

    #[async_trait]
    impl Fetch for ScriptedFetch {
        async fn fetch_candles(&self, _request: &CandleRequest) -> Result<String, PriceError> {
            Err(PriceError::Refused)
        }

        async fn fetch_quotes(&self, request: &QuoteRequest) -> Result<String, PriceError> {
            self.calls.lock().push(Call {
                tickers: request.tickers().to_vec(),
                currencies: request.currencies(),
            });
            let label = kraken::label();
            let reply = if self.is_muted.load(Ordering::SeqCst) {
                Reply::Unreachable
            } else {
                (self.reply)(request)
            };
            match reply {
                Reply::Body(body) => Ok(body),
                Reply::RateLimited(wait) => Err(PriceError::Net(NetError::RateLimited {
                    endpoint: label,
                    retry_after: Some(RetryAfter::After(wait)),
                })),
                Reply::Unreachable => Err(PriceError::Net(NetError::endpoint(
                    &label,
                    FailureKind::Connect,
                    io::Error::other("refused by the test"),
                ))),
            }
        }
    }

    /// A price the provider quotes for everything, which a test can turn between
    /// passes.
    #[derive(Debug, Clone)]
    struct Dial(Arc<Mutex<String>>);

    impl Dial {
        fn new(price: &str) -> Self {
            Self(Arc::new(Mutex::new(price.to_owned())))
        }

        fn set(&self, price: &str) {
            price.clone_into(&mut self.0.lock());
        }

        /// A script answering every request with the current price.
        fn script(&self) -> impl Fn(&QuoteRequest) -> Reply + Send + Sync + use<> {
            let price = Arc::clone(&self.0);
            move |request| {
                let amount = price.lock().clone();
                answer(request, &amount)
            }
        }
    }

    /// Answers any request with `amount` for every ticker it asked about and
    /// [`PAIR_PRICE`] for every currency.
    fn answer(request: &QuoteRequest, amount: &str) -> Reply {
        let quotes: Vec<(Ticker, &str)> =
            request.tickers().iter().map(|&ticker| (ticker, amount)).collect();
        let rates: Vec<(FiatCurrency, &str)> =
            request.currencies().into_iter().map(|currency| (currency, PAIR_PRICE)).collect();
        Reply::Body(kraken_answer(&quotes, &rates))
    }

    fn everything_at(amount: &'static str) -> impl Fn(&QuoteRequest) -> Reply + Send + Sync {
        move |request| answer(request, amount)
    }

    /// A feed on a clock only the test moves, and the poller that fills it.
    #[derive(Debug)]
    struct Harness {
        feed: PriceFeed,
        poller: PricePoller,
        fetch: Arc<ScriptedFetch>,
        clock: Arc<FakeClock>,
        started_unix_secs: i64,
    }

    fn harness(fetch: Arc<ScriptedFetch>) -> Harness {
        harness_with(&PriceFeedConfig::default(), fetch)
    }

    fn harness_with(config: &PriceFeedConfig, fetch: Arc<ScriptedFetch>) -> Harness {
        let clock = Arc::new(FakeClock::default());
        let started_unix_secs = clock.now_secs().expect("a sane fake clock");
        let (feed, poller) = PriceFeed::new(
            config,
            FiatCurrency::Usd,
            Arc::<ScriptedFetch>::clone(&fetch),
            Arc::<FakeClock>::clone(&clock),
        );
        Harness { feed, poller, fetch, clock, started_unix_secs }
    }
}
