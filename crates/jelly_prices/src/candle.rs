//! Kraken's OHLC candles, `GET /0/public/OHLC?pair=...&interval=...`, for one market at
//! one interval per request.

use std::collections::BTreeMap;

use jelly_core::{FiatCurrency, wire_enum};
use serde::{Serialize, de::IgnoredAny};
use serde_json::value::RawValue;

use crate::error::PriceError;
use crate::kraken::{self, Listed, Pair, RatePair};
use crate::price::Price;
use crate::ticker::Ticker;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How long one candle lasts. Kraken opens every candle at a multiple of its length
    /// since the Unix epoch, so a week opens on a Thursday.
    pub enum CandleInterval {
        Hour => "hour",
        Day => "day",
        Week => "week",
    }
}

impl CandleInterval {
    pub(crate) const fn secs(self) -> i64 {
        match self {
            Self::Hour => 3_600,
            Self::Day => 86_400,
            Self::Week => 604_800,
        }
    }

    /// Kraken's `interval` parameter, which counts minutes.
    const fn minutes(self) -> u32 {
        match self {
            Self::Hour => 60,
            Self::Day => 1_440,
            Self::Week => 10_080,
        }
    }
}

/// What a candle prices: a ticker in dollars, or the dollar in another currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Market {
    Asset(Ticker),
    Rate(FiatCurrency),
}

/// The first, highest, lowest, and last price of one interval.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candle {
    pub(crate) open_unix_secs: i64,
    pub(crate) open: Price,
    pub(crate) high: Price,
    pub(crate) low: Price,
    pub(crate) close: Price,
}

impl Candle {
    fn is_bounded(&self) -> bool {
        self.low <= self.open.min(self.close) && self.high >= self.open.max(self.close)
    }

    /// The same trading counted the other way round, so the inverse of the low is
    /// the new high.
    fn inverse(self) -> Option<Self> {
        let invert = |price: Price| price.inverse().filter(|inverted| !inverted.is_zero());
        Some(Self {
            open: invert(self.open)?,
            high: invert(self.low)?,
            low: invert(self.high)?,
            close: invert(self.close)?,
            ..self
        })
    }

    /// This candle in another currency, at `rate` units of it per unit of this one.
    /// Every price truncates the same way, so the candle keeps its shape.
    pub(crate) fn convert(self, rate: Price) -> Option<Self> {
        let convert = |price: Price| price.convert(rate).filter(|converted| !converted.is_zero());
        Some(Self {
            open: convert(self.open)?,
            high: convert(self.high)?,
            low: convert(self.low)?,
            close: convert(self.close)?,
            ..self
        })
    }
}

/// One OHLC request, and how to read its answer.
#[derive(Debug, Clone)]
pub struct CandleRequest {
    market: Market,
    interval: CandleInterval,
    url: String,
    pair: Pair,
}

impl CandleRequest {
    /// An answer holds about 720 candles of about a hundred bytes each.
    pub(crate) const MAX_BYTES: usize = 262_144;

    pub(crate) fn asset(ticker: Ticker, interval: CandleInterval) -> Self {
        Self::new(Market::Asset(ticker), interval, Pair::asset(ticker))
    }

    pub(crate) fn rate(rate: RatePair, interval: CandleInterval) -> Self {
        Self::new(Market::Rate(rate.currency), interval, rate.pair)
    }

    fn new(market: Market, interval: CandleInterval, pair: Pair) -> Self {
        let url = format!(
            "https://{}/0/public/OHLC?pair={}&interval={}",
            kraken::HOST,
            pair.canonical,
            interval.minutes()
        );
        Self { market, interval, url, pair }
    }

    pub(crate) const fn market(&self) -> Market {
        self.market
    }

    pub(crate) const fn interval(&self) -> CandleInterval {
        self.interval
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Every candle opened by `now_unix_secs`, oldest first. Kraken answers with its
    /// latest candles, and the last of them is still open.
    ///
    /// Drops a candle that's off the interval's grid, has a price that doesn't parse, or
    /// isn't bounded by its own high and low. Of two candles at the same time, the later
    /// one in the answer wins.
    pub(crate) fn decode(&self, body: &str, now_unix_secs: i64) -> Result<Vec<Candle>, PriceError> {
        // The result also holds Kraken's `last`, a number, so each pair stays raw until
        // it's the one asked for.
        let result = kraken::result::<Box<RawValue>>(body)?;
        let raw_rows = self.pair.find(&result).ok_or(PriceError::Absent)?;
        let rows: Vec<Row> = serde_json::from_str(raw_rows.get())?;
        let mut candles = BTreeMap::new();
        for (open_unix_secs, open, high, low, close, _, _, _) in rows {
            if open_unix_secs > now_unix_secs
                || open_unix_secs.rem_euclid(self.interval.secs()) != 0
            {
                continue;
            }
            let candle = quote_candle(open_unix_secs, [&open, &high, &low, &close])
                .filter(Candle::is_bounded)
                .and_then(|quoted| match self.pair.listed {
                    Listed::AsQuoted => Some(quoted),
                    Listed::Inverted => quoted.inverse(),
                });
            if let Some(candle) = candle {
                candles.insert(open_unix_secs, candle);
            }
        }
        Ok(candles.into_values().collect())
    }
}

fn quote_candle(open_unix_secs: i64, [open, high, low, close]: [&str; 4]) -> Option<Candle> {
    Some(Candle {
        open_unix_secs,
        open: Price::quote(open).ok()?,
        high: Price::quote(high).ok()?,
        low: Price::quote(low).ok()?,
        close: Price::quote(close).ok()?,
    })
}

// Kraken's rows are [time, open, high, low, close, vwap, volume, count].
type Row = (i64, String, String, String, String, IgnoredAny, IgnoredAny, IgnoredAny);

#[cfg(test)]
mod tests {
    use jelly_core::Coin;
    use serde_json::json;

    use super::*;
    use crate::fixtures::ohlc_body;

    const OPEN_HOUR_UNIX_SECS: i64 = 1_792_425_600;
    const NOW_UNIX_SECS: i64 = OPEN_HOUR_UNIX_SECS + 2_335;
    const NEXT_HOUR_UNIX_SECS: i64 = OPEN_HOUR_UNIX_SECS + 3_600;
    const LAST_HOUR_UNIX_SECS: i64 = OPEN_HOUR_UNIX_SECS - 3_600;
    const UNALIGNED_UNIX_SECS: i64 = LAST_HOUR_UNIX_SECS + 60;

    fn price(raw: &str) -> Price {
        Price::quote(raw).unwrap()
    }

    fn candle(open_unix_secs: i64, [open, high, low, close]: [&str; 4]) -> Candle {
        Candle {
            open_unix_secs,
            open: price(open),
            high: price(high),
            low: price(low),
            close: price(close),
        }
    }

    fn rate_request(currency: FiatCurrency) -> CandleRequest {
        let rate = RatePair::of(currency).expect("every currency but the dollar has a pair");
        CandleRequest::rate(rate, CandleInterval::Hour)
    }

    #[test]
    fn each_interval_asks_for_its_length_in_minutes() {
        for (interval, parameter) in [
            (CandleInterval::Hour, "&interval=60"),
            (CandleInterval::Day, "&interval=1440"),
            (CandleInterval::Week, "&interval=10080"),
        ] {
            let request = CandleRequest::asset(Ticker::of(Coin::Btc), interval);
            assert!(
                request.url().starts_with("https://api.kraken.com/0/public/OHLC?pair=XXBTZUSD")
            );
            assert!(request.url().ends_with(parameter), "{}", request.url());
            assert_eq!(interval.secs(), i64::from(interval.minutes()).saturating_mul(60));
        }
    }

    #[test]
    fn every_price_is_read_without_floats_from_either_exact_pair_name() {
        let request = CandleRequest::asset(Ticker::of(Coin::Btc), CandleInterval::Hour);
        let row = (OPEN_HOUR_UNIX_SECS, ["81847.1", "81990.4", "81648.4", "81936.400000009"]);
        for pair in ["XXBTZUSD", "XBTUSD"] {
            let candles = request.decode(&ohlc_body(pair, &[row]), NOW_UNIX_SECS).unwrap();
            assert_eq!(
                candles,
                [candle(OPEN_HOUR_UNIX_SECS, ["81847.1", "81990.4", "81648.4", "81936.4"])]
            );
            assert_eq!(candles[0].close.to_string(), "8193640000000");
        }
        for pair in ["XBTUSDT", "XETHZUSD"] {
            assert!(matches!(
                request.decode(&ohlc_body(pair, &[row]), NOW_UNIX_SECS),
                Err(PriceError::Absent)
            ));
        }
    }

    #[test]
    fn a_reported_error_beside_a_usable_result_is_not_a_refusal() {
        let request = CandleRequest::asset(Ticker::of(Coin::Btc), CandleInterval::Hour);
        let row = (OPEN_HOUR_UNIX_SECS, ["1", "2", "0.5", "1.5"]);
        let mut body: serde_json::Value =
            serde_json::from_str(&ohlc_body("XXBTZUSD", &[row])).unwrap();
        body["error"] = json!(["WGeneral:Warning"]);

        let candles = request.decode(&body.to_string(), NOW_UNIX_SECS).unwrap();

        assert_eq!(candles, [candle(OPEN_HOUR_UNIX_SECS, ["1", "2", "0.5", "1.5"])]);
    }

    #[test]
    fn the_open_candle_is_kept_and_future_and_unaligned_candles_are_not() {
        let request = CandleRequest::asset(Ticker::of(Coin::Btc), CandleInterval::Hour);
        let flat = ["2", "2", "2", "2"];
        let rows = [
            (NEXT_HOUR_UNIX_SECS, flat),
            (OPEN_HOUR_UNIX_SECS, flat),
            (UNALIGNED_UNIX_SECS, flat),
            (LAST_HOUR_UNIX_SECS, flat),
        ];
        let candles = request.decode(&ohlc_body("XXBTZUSD", &rows), NOW_UNIX_SECS).unwrap();
        assert_eq!(
            candles.iter().map(|candle| candle.open_unix_secs).collect::<Vec<_>>(),
            [LAST_HOUR_UNIX_SECS, OPEN_HOUR_UNIX_SECS]
        );
    }

    #[test]
    fn a_candle_that_makes_no_sense_is_dropped_without_its_neighbors() {
        let request = CandleRequest::asset(Ticker::of(Coin::Btc), CandleInterval::Hour);
        let hours_ago =
            |count: i64| OPEN_HOUR_UNIX_SECS.saturating_sub(count.saturating_mul(3_600));
        let rows = [
            (hours_ago(8), ["1", "1", "1", "1"]),
            (hours_ago(7), ["3", "4", "2", "3"]),
            (hours_ago(6), ["0", "4", "0", "3"]),
            (hours_ago(5), ["-1", "4", "2", "3"]),
            (hours_ago(4), ["3", "1000000001", "2", "3"]),
            (hours_ago(3), ["3", "not a price", "2", "3"]),
            (hours_ago(2), ["3", "4", "2", "5"]),
            (hours_ago(1), ["3", "4", "3.5", "3.75"]),
            (hours_ago(7), ["3", "5", "2", "4"]),
        ];
        let candles = request.decode(&ohlc_body("XXBTZUSD", &rows), NOW_UNIX_SECS).unwrap();
        assert_eq!(
            candles,
            [
                candle(hours_ago(8), ["1", "1", "1", "1"]),
                candle(hours_ago(7), ["3", "5", "2", "4"]),
            ]
        );
    }

    #[test]
    fn an_inverted_rate_swaps_its_high_and_low() {
        let row = (OPEN_HOUR_UNIX_SECS, ["2", "4", "1", "2"]);
        let euros = rate_request(FiatCurrency::Eur)
            .decode(&ohlc_body("ZEURZUSD", &[row]), NOW_UNIX_SECS)
            .unwrap();
        assert_eq!(euros, [candle(OPEN_HOUR_UNIX_SECS, ["0.5", "1", "0.25", "0.5"])]);

        let yen = rate_request(FiatCurrency::Jpy)
            .decode(&ohlc_body("ZUSDZJPY", &[row]), NOW_UNIX_SECS)
            .unwrap();
        assert_eq!(yen, [candle(OPEN_HOUR_UNIX_SECS, ["2", "4", "1", "2"])]);
    }

    #[test]
    fn a_converted_candle_keeps_its_shape_or_is_dropped_whole() {
        let dollars = candle(OPEN_HOUR_UNIX_SECS, ["100", "120", "90", "110"]);
        assert_eq!(
            dollars.convert(price("0.5")),
            Some(candle(OPEN_HOUR_UNIX_SECS, ["50", "60", "45", "55"]))
        );
        let tiny =
            candle(OPEN_HOUR_UNIX_SECS, ["0.00000001", "0.00000002", "0.00000001", "0.00000002"]);
        assert_eq!(tiny.convert(price("0.5")), None);
    }

    #[test]
    fn the_wire_form_names_each_price_in_base_units() {
        let encoded =
            serde_json::to_value(candle(OPEN_HOUR_UNIX_SECS, ["1", "2", "0.5", "1.5"])).unwrap();
        assert_eq!(
            encoded,
            json!({
                "openUnixSecs": OPEN_HOUR_UNIX_SECS,
                "open": "100000000",
                "high": "200000000",
                "low": "50000000",
                "close": "150000000",
            })
        );
    }

    #[test]
    fn malformed_and_refused_answers_never_expose_response_text() {
        let request = CandleRequest::asset(Ticker::of(Coin::Btc), CandleInterval::Day);
        let secret = "https://private.example/secret";
        for body in [
            json!({ "error": [secret] }).to_string(),
            json!({ "result": { "XXBTZUSD": secret } }).to_string(),
            json!({ "result": { "XXBTZUSD": [[OPEN_HOUR_UNIX_SECS, secret]] } }).to_string(),
            String::from("not json"),
        ] {
            let error = request.decode(&body, NOW_UNIX_SECS).unwrap_err();
            assert!(!error.to_string().contains(secret));
            assert!(!format!("{error:?}").contains(secret));
        }
    }
}
