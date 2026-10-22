//! Kraken's public REST API: the host and headers, the pair tables, and the answer
//! envelope that its Ticker and OHLC endpoints share.

use std::collections::BTreeMap;

use jelly_core::{EndpointLabel, FiatCurrency};
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};

use crate::error::PriceError;
use crate::ticker::Ticker;

/// The one host the feed reaches, over TLS. The request URL and the log label
/// are built from this same literal, so they cannot disagree.
pub(crate) const HOST: &str = "api.kraken.com";

pub(crate) const HEADERS: &[(&str, &str)] = &[("accept", "application/json")];

/// The only form of the provider's address allowed in an error or a log.
pub(crate) fn label() -> EndpointLabel {
    EndpointLabel::new("https", HOST, None)
}

/// Which way round Kraken lists a pair, against the number the book holds.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Listed {
    /// Already the number the book holds: `XXBTZUSD` is the dollar price of one
    /// bitcoin, and `USDJPY` how many yen one dollar buys.
    AsQuoted,
    /// The other way round: `EURUSD` is the dollar price of one euro, which is
    /// inverted into how many euros one dollar buys.
    Inverted,
}

/// One market Kraken lists against the dollar, and how to read it.
///
/// Kraken refuses the whole request if it names any pair Kraken does not list, so
/// the pair tables must name only listed pairs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pair {
    /// Kraken's own name for the pair, which a request asks for.
    pub(crate) canonical: &'static str,
    /// The other name Kraken may key its answer by.
    alternative: &'static str,
    pub(crate) listed: Listed,
}

impl Pair {
    /// The dollar market of `ticker`, which Kraken lists as the price of one unit.
    pub(crate) const fn asset(ticker: Ticker) -> Self {
        let (canonical, alternative) = match ticker {
            Ticker::Btc => ("XXBTZUSD", "XBTUSD"),
            Ticker::Ltc => ("XLTCZUSD", "LTCUSD"),
            // Kraken lists dogecoin as XDG, its ISO 4217 style code.
            Ticker::Doge => ("XDGUSD", "XDGUSD"),
            Ticker::Bch => ("BCHUSD", "BCHUSD"),
            Ticker::Zec => ("XZECZUSD", "ZECUSD"),
            Ticker::Eth => ("XETHZUSD", "ETHUSD"),
            Ticker::Bnb => ("BNBUSD", "BNBUSD"),
            Ticker::Pol => ("POLUSD", "POLUSD"),
            Ticker::Avax => ("AVAXUSD", "AVAXUSD"),
            Ticker::Sol => ("SOLUSD", "SOLUSD"),
            Ticker::Trx => ("TRXUSD", "TRXUSD"),
            Ticker::Xrp => ("XXRPZUSD", "XRPUSD"),
            // Kraken still lists Gram under its former TON ticker.
            Ticker::Gram => ("TONUSD", "TONUSD"),
            Ticker::Xmr => ("XXMRZUSD", "XMRUSD"),
            Ticker::Link => ("LINKUSD", "LINKUSD"),
            Ticker::Wbtc => ("WBTCUSD", "WBTCUSD"),
            Ticker::Uni => ("UNIUSD", "UNIUSD"),
            Ticker::Shib => ("SHIBUSD", "SHIBUSD"),
            Ticker::Ena => ("ENAUSD", "ENAUSD"),
            Ticker::Aave => ("AAVEUSD", "AAVEUSD"),
            Ticker::Pump => ("PUMPUSD", "PUMPUSD"),
            Ticker::Ondo => ("ONDOUSD", "ONDOUSD"),
            Ticker::Pepe => ("PEPEUSD", "PEPEUSD"),
            Ticker::Arb => ("ARBUSD", "ARBUSD"),
            Ticker::Jup => ("JUPUSD", "JUPUSD"),
            Ticker::Usdt => ("USDTZUSD", "USDTUSD"),
            Ticker::Usdc => ("USDCUSD", "USDCUSD"),
            Ticker::Usds => ("USDSUSD", "USDSUSD"),
            Ticker::Dai => ("DAIUSD", "DAIUSD"),
            Ticker::Usdg => ("USDGUSD", "USDGUSD"),
            Ticker::Pyusd => ("PYUSDUSD", "PYUSDUSD"),
            Ticker::Xaut => ("XAUTUSD", "XAUTUSD"),
            Ticker::Paxg => ("PAXGUSD", "PAXGUSD"),
        };
        Self { canonical, alternative, listed: Listed::AsQuoted }
    }

    /// The entry an answer keys under either name.
    ///
    /// Both are compared exactly, never by prefix or suffix, so an entry for
    /// `XBTUSDT` can never be filed as a dollar price.
    pub(crate) fn find<'a, T>(&self, result: &'a BTreeMap<String, T>) -> Option<&'a T> {
        result.get(self.canonical).or_else(|| result.get(self.alternative))
    }
}

/// A currency Kraken trades against the dollar, with its pair.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RatePair {
    pub(crate) currency: FiatCurrency,
    pub(crate) pair: Pair,
}

impl RatePair {
    /// The dollar itself has none. These six are every fiat currency Kraken trades
    /// against the dollar.
    pub(crate) const fn of(currency: FiatCurrency) -> Option<Self> {
        let (canonical, alternative, listed) = match currency {
            FiatCurrency::Usd => return None,
            FiatCurrency::Eur => ("ZEURZUSD", "EURUSD", Listed::Inverted),
            FiatCurrency::Gbp => ("ZGBPZUSD", "GBPUSD", Listed::Inverted),
            FiatCurrency::Aud => ("AUDUSD", "AUDUSD", Listed::Inverted),
            FiatCurrency::Cad => ("ZUSDZCAD", "USDCAD", Listed::AsQuoted),
            FiatCurrency::Jpy => ("ZUSDZJPY", "USDJPY", Listed::AsQuoted),
            FiatCurrency::Chf => ("USDCHF", "USDCHF", Listed::AsQuoted),
        };
        Some(Self { currency, pair: Pair { canonical, alternative, listed } })
    }

    /// Every currency whose rate a request asks for, which is every one with a
    /// pair.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        FiatCurrency::ALL.into_iter().filter_map(Self::of)
    }
}

/// What every endpoint answers, with its result keyed by pair name.
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Envelope<T> {
    /// Only its emptiness is read, so a provider's error text never reaches a log
    /// or an error.
    #[serde(default)]
    error: Vec<IgnoredAny>,
    #[serde(default)]
    result: BTreeMap<String, T>,
}

/// The result of one answer, keyed by pair name.
///
/// An empty result beside a reported error is a refusal, and beside no error it
/// is an answer that priced nothing.
pub(crate) fn result<T: DeserializeOwned>(body: &str) -> Result<BTreeMap<String, T>, PriceError> {
    let Envelope { error, result } = serde_json::from_str::<Envelope<T>>(body)?;
    if result.is_empty() && !error.is_empty() {
        return Err(PriceError::Refused);
    }
    Ok(result)
}
