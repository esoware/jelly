use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{AssetId, Clock, FiatCurrency};
use serde::Serialize;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::sleep;

use crate::candle::{Candle, CandleInterval, CandleRequest, Market};
use crate::config::PriceFeedConfig;
use crate::error::PriceError;
use crate::fetch::Fetch;
use crate::kraken::RatePair;
use crate::poll::TARGET;
use crate::price::Price;
use crate::switch::{wait_until_disabled, wait_until_enabled};
use crate::ticker::Ticker;

const DAY_SECS: i64 = CandleInterval::Day.secs();
/// Kraken's OHLC endpoint returns at most 720 candles.
const HISTORY_SECS: i64 = 720 * DAY_SECS;
/// How long a series that failed waits before it is asked for again.
///
/// A provider that asks for longer gets longer. A daily close changes once a day, so
/// nothing is lost by waiting, and an unreachable provider is not asked every few
/// seconds.
const RETRY_DELAY: Duration = Duration::from_mins(5);

#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyPrice {
    /// Unix seconds at the start of the UTC day.
    pub(crate) day_unix_secs: i64,
    /// The daily close.
    pub(crate) price: Price,
}

/// One USD price series, shared by every asset in `assets` because they trade as one
/// ticker. Each price is US dollars per whole unit of the asset.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AssetHistory {
    pub(crate) assets: Vec<AssetId>,
    pub(crate) prices: Vec<DailyPrice>,
}

/// One exchange rate series. Each price is units of `currency` per US dollar, so a USD
/// price times the rate is the price in `currency`.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RateHistory {
    pub(crate) currency: FiatCurrency,
    pub(crate) prices: Vec<DailyPrice>,
}

#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PriceHistory {
    prices: Vec<AssetHistory>,
    rates: Vec<RateHistory>,
}

#[derive(Debug, Clone)]
pub struct PriceHistoryFeed {
    history: watch::Sender<PriceHistory>,
}

impl PriceHistoryFeed {
    pub fn new(
        config: &PriceFeedConfig,
        fetch: Arc<dyn Fetch>,
        clock: Arc<dyn Clock>,
    ) -> (Self, PriceHistoryLoader) {
        let feed = Self { history: watch::Sender::new(PriceHistory::default()) };
        let loader = PriceHistoryLoader {
            feed: feed.clone(),
            requests: daily_requests(&config.tickers),
            fetch,
            clock,
        };
        (feed, loader)
    }

    pub fn snapshot(&self) -> PriceHistory {
        self.history.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<PriceHistory> {
        self.history.subscribe()
    }
}

#[derive(Debug)]
pub struct PriceHistoryLoader {
    feed: PriceHistoryFeed,
    requests: Vec<CandleRequest>,
    fetch: Arc<dyn Fetch>,
    clock: Arc<dyn Clock>,
}

impl PriceHistoryLoader {
    /// Loads every series while prices are enabled. Disabling prices cancels unfinished
    /// requests and withdraws the history, so enabling them again loads it afresh.
    pub async fn run(self, mut enabled: watch::Receiver<bool>) {
        loop {
            if !wait_until_enabled(&mut enabled).await {
                return;
            }
            let is_loaded = tokio::select! {
                () = self.load_all() => true,
                is_running = wait_until_disabled(&mut enabled) => {
                    if !is_running {
                        return;
                    }
                    false
                }
            };
            if is_loaded && !wait_until_disabled(&mut enabled).await {
                return;
            }
            self.feed.history.send_if_modified(|history| {
                let is_held = !history.prices.is_empty() || !history.rates.is_empty();
                *history = PriceHistory::default();
                is_held
            });
        }
    }

    /// Asks again for each series that failed until every one has loaded.
    async fn load_all(&self) {
        let mut history = PriceHistory::default();
        let mut missing = self.requests.clone();
        loop {
            let retry_after = self.load(&mut missing, &mut history).await;
            if missing.is_empty() {
                return;
            }
            sleep(retry_after.map_or(RETRY_DELAY, |asked| asked.max(RETRY_DELAY))).await;
        }
    }

    /// Asks for every series in `missing` at once and publishes `history` with each one
    /// that loaded, leaving the rest in `missing`. Returns how long the provider asked
    /// to be left alone, if it did.
    async fn load(
        &self,
        missing: &mut Vec<CandleRequest>,
        history: &mut PriceHistory,
    ) -> Option<Duration> {
        let mut requests = JoinSet::new();
        for request in &*missing {
            let request = request.clone();
            let fetch = Arc::clone(&self.fetch);
            let clock = Arc::clone(&self.clock);
            requests.spawn(async move {
                let body = fetch.fetch_candles(&request).await?;
                let now_unix_secs = clock.now_secs()?;
                let candles = request.decode(&body, now_unix_secs)?;
                Ok::<_, PriceError>((request.market(), daily_closes(&candles, now_unix_secs)))
            });
        }

        let mut loaded = Vec::new();
        let mut retry_after = None;
        while let Some(answered) = requests.join_next().await {
            match answered {
                Ok(Ok((market, prices))) => {
                    match market {
                        Market::Asset(ticker) => {
                            history
                                .prices
                                .push(AssetHistory { assets: ticker.assets().collect(), prices });
                        }
                        Market::Rate(currency) => {
                            history.rates.push(RateHistory { currency, prices });
                        }
                    }
                    loaded.push(market);
                }
                Ok(Err(error)) => {
                    tracing::warn!(target: TARGET, %error, "a price history request failed");
                    let asked = self
                        .clock
                        .now_secs()
                        .ok()
                        .and_then(|now_unix_secs| error.retry_after(now_unix_secs));
                    retry_after = retry_after.max(asked);
                }
                Err(error) => {
                    tracing::warn!(target: TARGET, %error, "price history task did not finish");
                }
            }
        }
        missing.retain(|request| !loaded.contains(&request.market()));
        if !loaded.is_empty() {
            self.feed.history.send_replace(history.clone());
        }
        retry_after
    }
}

/// One daily request per ticker and per rate, never per chain, and none at all
/// without a ticker.
fn daily_requests(tickers: &BTreeSet<Ticker>) -> Vec<CandleRequest> {
    if tickers.is_empty() {
        return Vec::new();
    }
    let assets = tickers.iter().map(|&ticker| CandleRequest::asset(ticker, CandleInterval::Day));
    let rates = RatePair::all().map(|rate| CandleRequest::rate(rate, CandleInterval::Day));
    assets.chain(rates).collect()
}

/// The close of every finished UTC day Kraken can answer for.
fn daily_closes(candles: &[Candle], now_unix_secs: i64) -> Vec<DailyPrice> {
    let today_unix_secs = now_unix_secs.div_euclid(DAY_SECS).saturating_mul(DAY_SECS);
    let oldest_unix_secs = today_unix_secs.saturating_sub(HISTORY_SECS);
    candles
        .iter()
        .filter(|candle| (oldest_unix_secs..today_unix_secs).contains(&candle.open_unix_secs))
        .map(|candle| DailyPrice { day_unix_secs: candle.open_unix_secs, price: candle.close })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use jelly_core::{ChainId, TokenId, test_util::FakeClock};
    use tokio::sync::Barrier;
    use tokio::task::yield_now;
    use tokio::time::timeout;

    use super::*;
    use crate::fixtures::{kraken_market_key, ohlc_body};
    use crate::quote::QuoteRequest;

    /// The fake clock's default time, which opens a UTC day.
    const TODAY_UNIX_SECS: i64 = 1_767_225_600;
    const YESTERDAY_UNIX_SECS: i64 = TODAY_UNIX_SECS - DAY_SECS;

    /// Days that each traded flat at their close.
    fn body(pair: &str, days: &[(i64, &str)]) -> String {
        let rows: Vec<_> =
            days.iter().map(|&(day_unix_secs, close)| (day_unix_secs, [close; 4])).collect();
        ohlc_body(pair, &rows)
    }

    fn closes(request: &CandleRequest, body: &str, now_unix_secs: i64) -> Vec<DailyPrice> {
        daily_closes(&request.decode(body, now_unix_secs).unwrap(), now_unix_secs)
    }

    fn bitcoin_request() -> CandleRequest {
        daily_requests(&BTreeSet::from([Ticker::Btc]))
            .into_iter()
            .next()
            .expect("the asset is planned first")
    }

    #[test]
    fn one_daily_request_per_ticker_and_rate_never_per_chain() {
        let requests = daily_requests(&Ticker::ALL.into_iter().collect());
        assert_eq!(requests.len(), Ticker::COUNT + FiatCurrency::COUNT - 1);
        let urls: BTreeSet<_> = requests.iter().map(CandleRequest::url).collect();
        assert_eq!(urls.len(), requests.len());
        for url in urls {
            assert!(url.starts_with("https://api.kraken.com/0/public/OHLC?pair="));
            assert!(url.ends_with("&interval=1440"));
            assert!(!url.contains(','));
        }
        assert_eq!(daily_requests(&BTreeSet::from([Ticker::Btc])).len(), FiatCurrency::COUNT);
        assert!(daily_requests(&BTreeSet::new()).is_empty());
    }

    #[test]
    fn todays_unfinished_candle_and_days_out_of_the_window_are_excluded() {
        let request = bitcoin_request();
        let oldest_unix_secs = TODAY_UNIX_SECS - HISTORY_SECS;
        let days = [
            (oldest_unix_secs - DAY_SECS, "1"),
            (oldest_unix_secs, "2"),
            (YESTERDAY_UNIX_SECS, "3"),
            (YESTERDAY_UNIX_SECS + 1, "4"),
            (TODAY_UNIX_SECS, "5"),
            (TODAY_UNIX_SECS + DAY_SECS, "6"),
        ];
        let prices = closes(&request, &body("XXBTZUSD", &days), TODAY_UNIX_SECS + 3600);
        assert_eq!(
            prices.iter().map(|price| price.day_unix_secs).collect::<Vec<_>>(),
            vec![oldest_unix_secs, YESTERDAY_UNIX_SECS]
        );
        assert_eq!(prices[1].price.to_string(), "300000000");
    }

    #[test]
    fn a_721_candle_response_keeps_all_720_completed_days() {
        let request = bitcoin_request();
        let days: Vec<_> =
            (0..=720).rev().map(|days_ago| (TODAY_UNIX_SECS - days_ago * DAY_SECS, "1")).collect();
        let prices = closes(&request, &body("XXBTZUSD", &days), TODAY_UNIX_SECS);
        assert_eq!(prices.len(), 720);
        assert_eq!(prices.first().unwrap().day_unix_secs, TODAY_UNIX_SECS - HISTORY_SECS);
        assert_eq!(prices.last().unwrap().day_unix_secs, YESTERDAY_UNIX_SECS);
    }

    #[test]
    fn historical_exchange_rates_use_the_same_days_close_and_correct_direction() {
        let requests = daily_requests(&BTreeSet::from([Ticker::Btc]));
        for (currency, pair, expected) in [
            (FiatCurrency::Eur, "ZEURZUSD", "50000000"),
            (FiatCurrency::Jpy, "ZUSDZJPY", "200000000"),
        ] {
            let request = requests
                .iter()
                .find(|request| request.market() == Market::Rate(currency))
                .expect("every currency but the dollar has a rate");
            let prices =
                closes(request, &body(pair, &[(YESTERDAY_UNIX_SECS, "2")]), TODAY_UNIX_SECS);
            assert_eq!(prices.len(), 1);
            assert_eq!(prices[0].day_unix_secs, YESTERDAY_UNIX_SECS);
            assert_eq!(prices[0].price.to_string(), expected);
        }
    }

    /// Every market there is a daily request for.
    const MARKET_COUNT: usize = Ticker::COUNT + FiatCurrency::COUNT - 1;

    #[derive(Debug, Default)]
    struct HistoryFetch {
        call_count: AtomicUsize,
        barrier: Option<Barrier>,
        /// How many more bitcoin requests are refused.
        btc_refusals: AtomicUsize,
    }

    #[async_trait]
    impl Fetch for HistoryFetch {
        async fn fetch_quotes(&self, _request: &QuoteRequest) -> Result<String, PriceError> {
            Err(PriceError::Refused)
        }

        async fn fetch_candles(&self, request: &CandleRequest) -> Result<String, PriceError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            if request.market() == Market::Asset(Ticker::Btc)
                && self
                    .btc_refusals
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                    .is_ok()
            {
                return Err(PriceError::Refused);
            }
            let pair = kraken_market_key(request.market());
            Ok(body(pair, &[(YESTERDAY_UNIX_SECS, "2"), (TODAY_UNIX_SECS, "3")]))
        }
    }

    fn loader_on(fetch: &Arc<HistoryFetch>) -> (PriceHistoryFeed, PriceHistoryLoader) {
        PriceHistoryFeed::new(
            &PriceFeedConfig::default(),
            Arc::<HistoryFetch>::clone(fetch),
            Arc::new(FakeClock::default()),
        )
    }

    fn is_complete(history: &PriceHistory) -> bool {
        history.prices.len() == Ticker::COUNT && history.rates.len() == FiatCurrency::COUNT - 1
    }

    #[tokio::test(start_paused = true)]
    async fn a_load_requests_every_market_in_parallel_and_publishes_once() {
        let count = MARKET_COUNT;
        let fetch = Arc::new(HistoryFetch {
            barrier: Some(Barrier::new(count)),
            ..HistoryFetch::default()
        });
        let (feed, loader) = loader_on(&fetch);
        let mut changes = feed.subscribe();
        timeout(Duration::from_secs(2), loader.load_all()).await.unwrap();
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), count);
        assert!(changes.has_changed().unwrap());
        let history = changes.borrow_and_update().clone();
        assert_eq!(history.prices.len(), Ticker::COUNT);
        assert_eq!(history.rates.len(), FiatCurrency::COUNT - 1);
        assert!(!changes.has_changed().unwrap());
        let stablecoin = history
            .prices
            .iter()
            .find(|series| series.assets.contains(&AssetId::Token(TokenId::UsdtErc20)))
            .unwrap();
        assert_eq!(stablecoin.assets, Ticker::Usdt.assets().collect::<Vec<_>>());
        assert_eq!(stablecoin.prices.len(), 1);
        let encoded = serde_json::to_value(&stablecoin.prices[0]).unwrap();
        assert_eq!(encoded["price"], "200000000");
        assert_eq!(encoded["dayUnixSecs"], YESTERDAY_UNIX_SECS);
    }

    #[tokio::test]
    async fn one_market_failure_does_not_discard_successful_markets() {
        let fetch =
            Arc::new(HistoryFetch { btc_refusals: AtomicUsize::new(1), ..HistoryFetch::default() });
        let (feed, loader) = loader_on(&fetch);
        let mut missing = loader.requests.clone();
        loader.load(&mut missing, &mut PriceHistory::default()).await;
        let history = feed.snapshot();
        assert_eq!(history.prices.len(), Ticker::COUNT - 1);
        assert_eq!(history.rates.len(), FiatCurrency::COUNT - 1);
        assert!(
            !history
                .prices
                .iter()
                .any(|series| series.assets.contains(&AssetId::Native(ChainId::Bitcoin)))
        );
        let still_missing: Vec<Market> = missing.iter().map(CandleRequest::market).collect();
        assert_eq!(still_missing, [Market::Asset(Ticker::Btc)]);
    }

    #[tokio::test]
    async fn no_configured_tickers_means_no_history_requests_including_rates() {
        let config = PriceFeedConfig { tickers: BTreeSet::new() };
        let fetch = Arc::new(HistoryFetch::default());
        let (feed, loader) = PriceHistoryFeed::new(
            &config,
            Arc::<HistoryFetch>::clone(&fetch),
            Arc::new(FakeClock::default()),
        );
        loader.load_all().await;
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(feed.snapshot(), PriceHistory::default());
    }

    #[tokio::test(start_paused = true)]
    async fn history_waits_until_prices_are_enabled_and_loads_only_once() {
        let fetch = Arc::new(HistoryFetch::default());
        let (feed, loader) = loader_on(&fetch);
        let mut changes = feed.subscribe();
        let (enabled, receiver) = watch::channel(false);
        let task = tokio::spawn(loader.run(receiver));
        yield_now().await;
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), 0);
        enabled.send_replace(true);
        timeout(Duration::from_secs(2), changes.wait_for(is_complete)).await.unwrap().unwrap();

        sleep(RETRY_DELAY.saturating_mul(4)).await;
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), MARKET_COUNT);
        drop(enabled);
        timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_series_that_failed_is_asked_for_again_until_it_loads() {
        let fetch =
            Arc::new(HistoryFetch { btc_refusals: AtomicUsize::new(2), ..HistoryFetch::default() });
        let (feed, loader) = loader_on(&fetch);
        let mut changes = feed.subscribe();
        let (enabled, receiver) = watch::channel(true);
        let task = tokio::spawn(loader.run(receiver));

        let waited = RETRY_DELAY.saturating_mul(3);
        timeout(waited, changes.wait_for(is_complete)).await.unwrap().unwrap();
        assert_eq!(
            fetch.call_count.load(Ordering::SeqCst),
            MARKET_COUNT + 2,
            "only the series that failed was asked for again"
        );
        drop(enabled);
        timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn turning_prices_off_withdraws_the_history_and_turning_them_on_loads_it_again() {
        let fetch = Arc::new(HistoryFetch::default());
        let (feed, loader) = loader_on(&fetch);
        let mut changes = feed.subscribe();
        let (enabled, receiver) = watch::channel(true);
        let task = tokio::spawn(loader.run(receiver));
        timeout(Duration::from_secs(2), changes.wait_for(is_complete)).await.unwrap().unwrap();

        enabled.send_replace(false);
        let is_withdrawn = |history: &PriceHistory| *history == PriceHistory::default();
        timeout(Duration::from_secs(2), changes.wait_for(is_withdrawn)).await.unwrap().unwrap();
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), MARKET_COUNT);

        enabled.send_replace(true);
        timeout(Duration::from_secs(2), changes.wait_for(is_complete)).await.unwrap().unwrap();
        assert_eq!(fetch.call_count.load(Ordering::SeqCst), MARKET_COUNT * 2);
        drop(enabled);
        timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_prices_cancels_inflight_history_without_publishing_it() {
        let count = Ticker::COUNT + FiatCurrency::COUNT - 1;
        let fetch = Arc::new(HistoryFetch {
            barrier: Some(Barrier::new(count + 1)),
            ..HistoryFetch::default()
        });
        let (feed, loader) = PriceHistoryFeed::new(
            &PriceFeedConfig::default(),
            Arc::<HistoryFetch>::clone(&fetch),
            Arc::new(FakeClock::default()),
        );
        let (enabled, receiver) = watch::channel(true);
        let task = tokio::spawn(loader.run(receiver));
        timeout(Duration::from_secs(2), async {
            while fetch.call_count.load(Ordering::SeqCst) < count {
                yield_now().await;
            }
        })
        .await
        .unwrap();
        enabled.send_replace(false);
        yield_now().await;
        drop(enabled);
        timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
        assert_eq!(feed.snapshot(), PriceHistory::default());
    }
}
