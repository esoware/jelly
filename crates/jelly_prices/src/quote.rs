//! Kraken's Ticker endpoint, `GET /0/public/Ticker?pair=...`, which asks for every
//! ticker and every rate in one request.

use std::collections::{BTreeMap, BTreeSet};

use jelly_core::FiatCurrency;
use serde::Deserialize;

use crate::error::PriceError;
use crate::kraken::{self, Listed, Pair, RatePair};
use crate::price::{Price, QuoteRejection};
use crate::ticker::Ticker;

/// One HTTP GET to the provider, and the tickers and rates its answer must
/// account for.
#[derive(Debug)]
pub struct QuoteRequest {
    url: String,
    tickers: Vec<Ticker>,
    rate_pairs: Vec<RatePair>,
}

impl QuoteRequest {
    /// One pair entry is a few hundred bytes and a request asks for at most one
    /// pair per [`Ticker`] and one per [`FiatCurrency`].
    pub(crate) const MAX_BYTES: usize = 65_536;

    /// A single request that prices all of `tickers`, or `None` when there are none.
    ///
    /// It asks for the dollar's rate in every currency the wallet can show, not just the
    /// one it shows, so the request says nothing about the user.
    pub(crate) fn for_tickers(tickers: &BTreeSet<Ticker>) -> Option<Self> {
        if tickers.is_empty() {
            return None;
        }
        let tickers: Vec<Ticker> = tickers.iter().copied().collect();
        let rate_pairs: Vec<RatePair> = RatePair::all().collect();
        let coins = tickers.iter().map(|&ticker| Pair::asset(ticker).canonical);
        let rates = rate_pairs.iter().map(|rate| rate.pair.canonical);
        let pairs: Vec<&str> = coins.chain(rates).collect();
        let url = format!("https://{}/0/public/Ticker?pair={}", kraken::HOST, pairs.join(","));
        Some(Self { url, tickers, rate_pairs })
    }

    /// What this request is expected to price. Every one of them appears in the
    /// decoded [`Quotes`], accepted or rejected.
    pub(crate) fn tickers(&self) -> &[Ticker] {
        &self.tickers
    }

    /// The currencies this request asks a rate for, each of which appears in the
    /// decoded [`Quotes`] the same way.
    #[cfg(test)]
    pub(crate) fn currencies(&self) -> Vec<FiatCurrency> {
        self.rate_pairs.iter().map(|rate| rate.currency).collect()
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// A decode error or a refusal rejects the whole body. A single ticker that fails goes
    /// in `Quotes::rejected`, and a single rate in `Quotes::unrated`.
    pub(crate) fn decode(&self, body: &str) -> Result<Quotes, PriceError> {
        let result = kraken::result::<Entry>(body)?;
        let mut quotes = Quotes::default();
        for &ticker in &self.tickers {
            match last_trade(&result, Pair::asset(ticker)) {
                Ok(usd) => quotes.accepted.push(Quote { ticker, usd }),
                Err(reason) => quotes.rejected.push(RejectedQuote { ticker, reason }),
            }
        }
        for &RatePair { currency, pair } in &self.rate_pairs {
            match last_trade(&result, pair) {
                Ok(per_dollar) => quotes.rates.push(Rate { currency, per_dollar }),
                Err(reason) => quotes.unrated.push(RejectedRate { currency, reason }),
            }
        }
        Ok(quotes)
    }
}

#[derive(Debug, Deserialize)]
struct Entry {
    /// The last trade, as price then lot volume.
    #[serde(default)]
    c: Vec<String>,
}

/// The pair's last trade as the number the book holds: a ticker's dollar price,
/// or how many units of a currency one dollar buys.
fn last_trade(result: &BTreeMap<String, Entry>, pair: Pair) -> Result<Price, QuoteRejection> {
    let last = pair.find(result).and_then(|entry| entry.c.first()).ok_or(QuoteRejection::Absent)?;
    let quoted = Price::quote(last)?;
    match pair.listed {
        Listed::AsQuoted => Ok(quoted),
        Listed::Inverted => {
            quoted.inverse().filter(|inverted| !inverted.is_zero()).ok_or(QuoteRejection::Zero)
        }
    }
}

#[derive(Debug)]
pub(crate) struct Quote {
    pub(crate) ticker: Ticker,
    pub(crate) usd: Price,
}

/// One ticker the provider was asked for and didn't usably answer.
#[derive(Debug)]
pub(crate) struct RejectedQuote {
    pub(crate) ticker: Ticker,
    pub(crate) reason: QuoteRejection,
}

/// How many units of a currency one US dollar buys.
#[derive(Debug)]
pub(crate) struct Rate {
    pub(crate) currency: FiatCurrency,
    pub(crate) per_dollar: Price,
}

/// One currency the provider was asked a rate for and didn't usably answer.
#[derive(Debug)]
pub(crate) struct RejectedRate {
    pub(crate) currency: FiatCurrency,
    pub(crate) reason: QuoteRejection,
}

/// What one answered request yielded, in request order. Empty when nothing was answered.
///
/// `accepted` and `rejected` together name each ticker an answered request asked for
/// exactly once, and `rates` and `unrated` each currency.
#[derive(Debug, Default)]
pub(crate) struct Quotes {
    pub(crate) accepted: Vec<Quote>,
    pub(crate) rejected: Vec<RejectedQuote>,
    pub(crate) rates: Vec<Rate>,
    pub(crate) unrated: Vec<RejectedRate>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{kraken_key, kraken_rate_key, planned, rated_currencies};

    /// One recorded body and what the decoder must make of it.
    #[derive(Debug, Deserialize)]
    struct DecodeCase {
        name: String,
        /// Ticker codes the request asks for.
        tickers: Vec<String>,
        body: String,
        /// Ticker code to expected price, in base units.
        #[serde(default)]
        accepted: BTreeMap<String, String>,
        /// Ticker code to expected rejection.
        #[serde(default)]
        rejected: BTreeMap<String, String>,
        /// Currency code to expected units per dollar, in base units.
        #[serde(default)]
        rates: BTreeMap<String, String>,
        /// Currency code to expected rejection. Every currency neither here nor in
        /// `rates` is expected to be absent.
        #[serde(default)]
        unrated: BTreeMap<String, String>,
        /// Set when the whole body is unusable.
        #[serde(default)]
        error: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct DecodeFixture {
        cases: Vec<DecodeCase>,
    }

    const KRAKEN_TICKER: &str = include_str!("../tests/fixtures/kraken_ticker.json");

    fn fixture() -> DecodeFixture {
        serde_json::from_str(KRAKEN_TICKER).expect("the fixture parses")
    }

    fn tickers(codes: &[String]) -> Vec<Ticker> {
        codes
            .iter()
            .map(|code| {
                Ticker::all().find(|ticker| ticker.coin().symbol() == code).expect("a known ticker")
            })
            .collect()
    }

    fn currency(code: &str) -> FiatCurrency {
        FiatCurrency::parse(code).expect("a known currency")
    }

    const fn error_name(error: &PriceError) -> &'static str {
        match *error {
            PriceError::Net(_) => "net",
            PriceError::Malformed { .. } => "malformed",
            PriceError::Refused => "refused",
            PriceError::Absent => "absent",
            PriceError::Clock(_) => "clock",
        }
    }

    fn check(case: &DecodeCase) {
        let wanted_tickers = tickers(&case.tickers);
        let quotes = match planned(&wanted_tickers).decode(&case.body) {
            Err(error) => {
                assert_eq!(Some(error_name(&error)), case.error.as_deref(), "{}", case.name);
                return;
            }
            Ok(quotes) => quotes,
        };
        assert_eq!(case.error, None, "{}: the body was accepted", case.name);

        assert_eq!(quotes.accepted.len(), case.accepted.len(), "{}: accepted count", case.name);
        for (code, expected) in &case.accepted {
            let found = quotes.accepted.iter().find(|quote| quote.ticker.coin().symbol() == code);
            match found {
                None => panic!("{}: {code} was not priced", case.name),
                // Display is base units, which is the wire form the fixture records.
                Some(quote) => {
                    assert_eq!(quote.usd.to_string(), *expected, "{}: {code}", case.name);
                }
            }
        }

        assert_eq!(quotes.rejected.len(), case.rejected.len(), "{}: rejected count", case.name);
        for (code, expected) in &case.rejected {
            let found = quotes.rejected.iter().find(|quote| quote.ticker.coin().symbol() == code);
            match found {
                None => panic!("{}: {code} was not rejected", case.name),
                Some(quote) => {
                    assert_eq!(quote.reason.as_str(), *expected, "{}: {code}", case.name);
                }
            }
        }

        for &ticker in &wanted_tickers {
            let accepted = quotes.accepted.iter().filter(|quote| quote.ticker == ticker).count();
            let rejected = quotes.rejected.iter().filter(|quote| quote.ticker == ticker).count();
            assert_eq!(
                accepted.saturating_add(rejected),
                1,
                "{}: {ticker:?} was accounted for twice or not at all",
                case.name
            );
        }

        assert_eq!(quotes.rates.len(), case.rates.len(), "{}: rated count", case.name);
        for (code, expected) in &case.rates {
            let asked = currency(code);
            match quotes.rates.iter().find(|rate| rate.currency == asked) {
                None => panic!("{}: {code} was not rated", case.name),
                Some(rate) => {
                    assert_eq!(rate.per_dollar.to_string(), *expected, "{}: {code}", case.name);
                }
            }
        }

        for currency in rated_currencies() {
            let code = currency.as_str();
            let rates = quotes.rates.iter().filter(|rate| rate.currency == currency).count();
            let declined: Vec<QuoteRejection> = quotes
                .unrated
                .iter()
                .filter(|rate| rate.currency == currency)
                .map(|rate| rate.reason)
                .collect();
            assert_eq!(
                rates.saturating_add(declined.len()),
                1,
                "{}: {code} was accounted for twice or not at all",
                case.name
            );
            if let Some(reason) = declined.first() {
                let expected = case.unrated.get(code).map_or("absent", String::as_str);
                assert_eq!(reason.as_str(), expected, "{}: {code}", case.name);
            }
        }
    }

    #[test]
    fn every_recorded_body_decodes_to_its_expected_outcome() {
        let fixture = fixture();
        assert_eq!(fixture.cases.len(), 26, "the fixture lost or gained a case");
        for case in &fixture.cases {
            check(case);
        }
    }

    #[test]
    fn every_rejection_appears_in_a_recorded_case() {
        let mut seen = BTreeSet::new();
        for case in fixture().cases {
            for reason in case.rejected.values().chain(case.unrated.values()) {
                let found = QuoteRejection::parse(reason)
                    .unwrap_or_else(|| panic!("{reason} is not a rejection"));
                seen.insert(found);
            }
        }
        assert_eq!(seen.len(), QuoteRejection::COUNT, "a rejection has no recorded body: {seen:?}");
    }

    #[test]
    fn one_request_asks_for_every_ticker_and_every_rate() {
        let all: Vec<Ticker> = Ticker::all().collect();
        let request = planned(&all);
        assert_eq!(request.tickers(), all);
        assert_eq!(request.currencies(), rated_currencies());

        let coins = all.iter().map(|&ticker| kraken_key(ticker));
        let rates = rated_currencies().into_iter().filter_map(kraken_rate_key);
        let pairs: Vec<&str> = coins.chain(rates).collect();
        assert_eq!(
            request.url(),
            format!("https://api.kraken.com/0/public/Ticker?pair={}", pairs.join(","))
        );
    }

    #[test]
    fn nothing_wanted_plans_no_request() {
        assert!(QuoteRequest::for_tickers(&BTreeSet::new()).is_none());
    }
}
