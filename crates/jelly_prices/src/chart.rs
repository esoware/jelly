use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{AssetId, Clock, Deadline, FiatCurrency};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::candle::{Candle, CandleInterval, CandleRequest, Market};
use crate::config::PriceFeedConfig;
use crate::error::PriceError;
use crate::fetch::Fetch;
use crate::kraken::RatePair;
use crate::ticker::Ticker;

/// One asset's candles in one currency, oldest first.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceChart {
    currency: FiatCurrency,
    /// The last is usually still open, so its close is the latest trade Kraken had.
    candles: Vec<Candle>,
}

type Slot = Arc<Mutex<Option<Held>>>;

/// Candles fetched when a chart asks for them and reused while fresh, so switching
/// ranges, modes, or coins rarely sends a request. Charts in one currency share its
/// rate candles.
#[derive(Debug)]
pub struct PriceCharts {
    tickers: BTreeSet<Ticker>,
    fetch: Arc<dyn Fetch>,
    clock: Arc<dyn Clock>,
    slots: Mutex<BTreeMap<(Market, CandleInterval), Slot>>,
}

#[derive(Debug)]
struct Held {
    candles: Arc<[Candle]>,
    fresh_until: Deadline,
}

impl PriceCharts {
    pub fn new(config: &PriceFeedConfig, fetch: Arc<dyn Fetch>, clock: Arc<dyn Clock>) -> Self {
        Self { tickers: config.tickers.clone(), fetch, clock, slots: Mutex::default() }
    }

    /// Each candle converts at the close of the latest rate candle opened by then, so
    /// it keeps the dollar candle's shape. A candle older than every rate is left out.
    ///
    /// An asset whose ticker isn't configured is refused without a request.
    pub async fn chart(
        &self,
        asset: AssetId,
        interval: CandleInterval,
        currency: FiatCurrency,
    ) -> Result<PriceChart, PriceError> {
        let ticker = Ticker::for_coin(asset.coin());
        if !self.tickers.contains(&ticker) {
            return Err(PriceError::Absent);
        }
        let prices = self.candles(CandleRequest::asset(ticker, interval));
        if currency == FiatCurrency::Usd {
            return Ok(PriceChart { currency, candles: prices.await?.to_vec() });
        }
        let rate = RatePair::of(currency).ok_or(PriceError::Absent)?;
        let (prices, rates) =
            tokio::try_join!(prices, self.candles(CandleRequest::rate(rate, interval)))?;
        Ok(PriceChart { currency, candles: convert(&prices, &rates) })
    }

    /// A caller asking for candles already in flight waits for that request instead of
    /// sending its own. A failure isn't kept, so the next caller asks again.
    async fn candles(&self, request: CandleRequest) -> Result<Arc<[Candle]>, PriceError> {
        let slot = Arc::clone(
            self.slots.lock().await.entry((request.market(), request.interval())).or_default(),
        );
        let mut held = slot.lock().await;
        if let Some(fresh) =
            held.as_ref().filter(|held| !held.fresh_until.is_expired(self.clock.as_ref()))
        {
            return Ok(Arc::clone(&fresh.candles));
        }
        let body = self.fetch.fetch_candles(&request).await?;
        let candles: Arc<[Candle]> = request.decode(&body, self.clock.now_secs()?)?.into();
        *held = Some(Held {
            candles: Arc::clone(&candles),
            fresh_until: self.clock.deadline_in(freshness(request.interval())),
        });
        Ok(candles)
    }
}

/// How long candles are reused. The frontend draws the open candle's close from the
/// live feed, so this bounds only how stale that candle's high and low, and the last
/// finished candle, can be.
const fn freshness(interval: CandleInterval) -> Duration {
    match interval {
        CandleInterval::Hour => Duration::from_mins(5),
        CandleInterval::Day => Duration::from_mins(30),
        CandleInterval::Week => Duration::from_hours(6),
    }
}

fn convert(prices: &[Candle], rates: &[Candle]) -> Vec<Candle> {
    let mut rates = rates.iter().peekable();
    let mut rate = None;
    prices
        .iter()
        .filter_map(|candle| {
            while let Some(opened) =
                rates.next_if(|next| next.open_unix_secs <= candle.open_unix_secs)
            {
                rate = Some(opened.close);
            }
            candle.convert(rate?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use jelly_core::{ChainId, test_util::FakeClock};
    use serde_json::json;
    use tokio::task::yield_now;

    use super::*;
    use crate::fixtures::{kraken_market_key, ohlc_body};
    use crate::quote::QuoteRequest;

    const BTC: AssetId = AssetId::Native(ChainId::Bitcoin);
    const ETH: AssetId = AssetId::Native(ChainId::Ethereum);

    /// Every asset trades at 100, then 90 to 120 closing at 110, then 110, over the
    /// last three candles of whatever interval is asked for. The euro is quoted from
    /// the second of them on, at two dollars.
    #[derive(Debug, Default)]
    struct CandleFetch {
        calls: parking_lot::Mutex<Vec<(Market, CandleInterval)>>,
        is_failing: AtomicBool,
    }

    impl CandleFetch {
        fn calls(&self) -> Vec<(Market, CandleInterval)> {
            self.calls.lock().clone()
        }
    }

    fn opens(interval: CandleInterval) -> [i64; 3] {
        let step = interval.secs();
        let now_unix_secs = FakeClock::default().now_secs().unwrap();
        let open_unix_secs = now_unix_secs.div_euclid(step).saturating_mul(step);
        [
            open_unix_secs.saturating_sub(step.saturating_mul(2)),
            open_unix_secs.saturating_sub(step),
            open_unix_secs,
        ]
    }

    #[async_trait]
    impl Fetch for CandleFetch {
        async fn fetch_quotes(&self, _request: &QuoteRequest) -> Result<String, PriceError> {
            Err(PriceError::Refused)
        }

        async fn fetch_candles(&self, request: &CandleRequest) -> Result<String, PriceError> {
            self.calls.lock().push((request.market(), request.interval()));
            yield_now().await;
            if self.is_failing.load(Ordering::SeqCst) {
                return Err(PriceError::Refused);
            }
            let [first, second, third] = opens(request.interval());
            let rows = match request.market() {
                Market::Asset(_) => vec![
                    (first, ["100", "100", "100", "100"]),
                    (second, ["100", "120", "90", "110"]),
                    (third, ["110", "110", "110", "110"]),
                ],
                Market::Rate(_) => vec![(second, ["2", "2", "2", "2"])],
            };
            Ok(ohlc_body(kraken_market_key(request.market()), &rows))
        }
    }

    struct Harness {
        charts: PriceCharts,
        fetch: Arc<CandleFetch>,
        clock: Arc<FakeClock>,
    }

    fn harness(config: &PriceFeedConfig) -> Harness {
        let fetch = Arc::new(CandleFetch::default());
        let clock = Arc::new(FakeClock::default());
        let charts = PriceCharts::new(
            config,
            Arc::<CandleFetch>::clone(&fetch),
            Arc::<FakeClock>::clone(&clock),
        );
        Harness { charts, fetch, clock }
    }

    fn closes(chart: &PriceChart) -> Vec<(i64, String)> {
        chart
            .candles
            .iter()
            .map(|candle| (candle.open_unix_secs, candle.close.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn a_dollar_chart_is_one_request_for_the_assets_ticker() {
        let Harness { charts, fetch, .. } = harness(&PriceFeedConfig::default());
        let chart = charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Usd).await.unwrap();
        let [first, second, third] = opens(CandleInterval::Hour);
        assert_eq!(
            closes(&chart),
            [
                (first, String::from("10000000000")),
                (second, String::from("11000000000")),
                (third, String::from("11000000000")),
            ]
        );
        assert_eq!(fetch.calls(), [(Market::Asset(Ticker::Btc), CandleInterval::Hour)]);
    }

    #[tokio::test]
    async fn another_currency_converts_each_candle_at_the_latest_rate_by_then() {
        let Harness { charts, .. } = harness(&PriceFeedConfig::default());
        let chart = charts.chart(BTC, CandleInterval::Day, FiatCurrency::Eur).await.unwrap();
        let [_, second, third] = opens(CandleInterval::Day);
        let encoded = serde_json::to_value(&chart).unwrap();
        assert_eq!(encoded["currency"], "eur");
        assert_eq!(
            encoded["candles"],
            json!([
                {
                    "openUnixSecs": second,
                    "open": "5000000000",
                    "high": "6000000000",
                    "low": "4500000000",
                    "close": "5500000000",
                },
                {
                    "openUnixSecs": third,
                    "open": "5500000000",
                    "high": "5500000000",
                    "low": "5500000000",
                    "close": "5500000000",
                },
            ])
        );
    }

    #[tokio::test]
    async fn fresh_candles_are_reused_and_one_currencys_rates_serve_every_coin() {
        let Harness { charts, fetch, clock } = harness(&PriceFeedConfig::default());
        for _ in 0..2u8 {
            charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Eur).await.unwrap();
        }
        assert_eq!(fetch.calls().len(), 2);

        charts.chart(ETH, CandleInterval::Hour, FiatCurrency::Eur).await.unwrap();
        assert_eq!(fetch.calls().len(), 3, "the euro's hourly rates were asked for again");

        charts.chart(BTC, CandleInterval::Week, FiatCurrency::Eur).await.unwrap();
        assert_eq!(fetch.calls().len(), 5, "a week is not an hour");

        clock.advance(freshness(CandleInterval::Hour));
        charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Eur).await.unwrap();
        charts.chart(BTC, CandleInterval::Week, FiatCurrency::Eur).await.unwrap();
        assert_eq!(fetch.calls().len(), 7, "only the hourly candles went stale");
    }

    #[tokio::test]
    async fn charts_asked_for_at_once_share_one_request() {
        let Harness { charts, fetch, .. } = harness(&PriceFeedConfig::default());
        let (first, second) = tokio::join!(
            charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Usd),
            charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Usd),
        );
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(fetch.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_failure_is_not_kept() {
        let Harness { charts, fetch, .. } = harness(&PriceFeedConfig::default());
        fetch.is_failing.store(true, Ordering::SeqCst);
        assert!(matches!(
            charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Usd).await,
            Err(PriceError::Refused)
        ));
        fetch.is_failing.store(false, Ordering::SeqCst);
        charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Usd).await.unwrap();
        assert_eq!(fetch.calls().len(), 2);
    }

    #[tokio::test]
    async fn an_asset_whose_ticker_is_not_configured_sends_nothing() {
        let config = PriceFeedConfig { tickers: BTreeSet::from([Ticker::Eth]) };
        let Harness { charts, fetch, .. } = harness(&config);
        assert!(matches!(
            charts.chart(BTC, CandleInterval::Hour, FiatCurrency::Eur).await,
            Err(PriceError::Absent)
        ));
        assert!(fetch.calls().is_empty());
    }
}
