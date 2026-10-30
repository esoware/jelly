//! Kraken's public REST API: the host and headers, the pair tables, and the answer
//! envelope that its Ticker and OHLC endpoints share.

use std::collections::BTreeMap;

use jelly_core::{Coin, EndpointLabel, FiatCurrency};
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};

use crate::error::PriceError;
use crate::ticker::Ticker;

/// The only host the feed reaches, over TLS. Request URLs and the log label are both
/// built from this, so they can't disagree.
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
/// Kraken refuses a whole request that names any pair it doesn't list, so the pair tables
/// must name only listed pairs.
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
        let (canonical, alternative) = match ticker.coin() {
            Coin::Btc => ("XXBTZUSD", "XBTUSD"),
            Coin::Ltc => ("XLTCZUSD", "LTCUSD"),
            // Kraken lists dogecoin as XDG, its ISO 4217 style code.
            Coin::Doge => ("XDGUSD", "XDGUSD"),
            Coin::Bch => ("BCHUSD", "BCHUSD"),
            Coin::Zec => ("XZECZUSD", "ZECUSD"),
            Coin::Eth | Coin::Weth => ("XETHZUSD", "ETHUSD"),
            Coin::Bnb => ("BNBUSD", "BNBUSD"),
            Coin::Pol => ("POLUSD", "POLUSD"),
            Coin::Avax => ("AVAXUSD", "AVAXUSD"),
            Coin::Sol => ("SOLUSD", "SOLUSD"),
            Coin::Trx => ("TRXUSD", "TRXUSD"),
            Coin::Xrp => ("XXRPZUSD", "XRPUSD"),
            // Kraken still lists Gram under its former TON ticker.
            Coin::Gram => ("TONUSD", "TONUSD"),
            Coin::Xmr => ("XXMRZUSD", "XMRUSD"),
            Coin::Link => ("LINKUSD", "LINKUSD"),
            Coin::Wbtc => ("WBTCUSD", "WBTCUSD"),
            Coin::Uni => ("UNIUSD", "UNIUSD"),
            Coin::Shib => ("SHIBUSD", "SHIBUSD"),
            Coin::Ena => ("ENAUSD", "ENAUSD"),
            Coin::Aave => ("AAVEUSD", "AAVEUSD"),
            Coin::Ondo => ("ONDOUSD", "ONDOUSD"),
            Coin::Pepe => ("PEPEUSD", "PEPEUSD"),
            Coin::Arb => ("ARBUSD", "ARBUSD"),
            Coin::Op => ("OPUSD", "OPUSD"),
            Coin::Pump => ("PUMPUSD", "PUMPUSD"),
            Coin::Jup => ("JUPUSD", "JUPUSD"),
            Coin::Usdt => ("USDTZUSD", "USDTUSD"),
            Coin::Usdc => ("USDCUSD", "USDCUSD"),
            Coin::Usds => ("USDSUSD", "USDSUSD"),
            Coin::Dai => ("DAIUSD", "DAIUSD"),
            Coin::Usdg => ("USDGUSD", "USDGUSD"),
            Coin::Pyusd => ("PYUSDUSD", "PYUSDUSD"),
            Coin::Xaut => ("XAUTUSD", "XAUTUSD"),
            Coin::Paxg => ("PAXGUSD", "PAXGUSD"),
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
    /// `None` for the dollar itself.
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
/// An empty result beside a reported error is a refusal, and beside no error it's an
/// answer that priced nothing.
pub(crate) fn result<T: DeserializeOwned>(body: &str) -> Result<BTreeMap<String, T>, PriceError> {
    let Envelope { error, result } = serde_json::from_str::<Envelope<T>>(body)?;
    if result.is_empty() && !error.is_empty() {
        return Err(PriceError::Refused);
    }
    Ok(result)
}
