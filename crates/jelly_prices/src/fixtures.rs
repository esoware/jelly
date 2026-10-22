use jelly_core::FiatCurrency;
use serde_json::json;

use crate::candle::Market;
use crate::quote::QuoteRequest;
use crate::ticker::Ticker;

pub(crate) fn planned(tickers: &[Ticker]) -> QuoteRequest {
    QuoteRequest::for_tickers(&tickers.iter().copied().collect()).expect("a planned request")
}

/// Every currency a request asks a rate for, which is every one but the dollar.
pub(crate) fn rated_currencies() -> Vec<FiatCurrency> {
    FiatCurrency::ALL.into_iter().filter(|&currency| currency != FiatCurrency::Usd).collect()
}

/// An OHLC answer for `pair`, one row per open time with its open, high, low, and
/// close. Kraken's `last` sits beside the pair in the result, as it does in a real
/// answer.
pub(crate) fn ohlc_body(pair: &str, rows: &[(i64, [&str; 4])]) -> String {
    let candles: Vec<_> = rows
        .iter()
        .map(|&(open_unix_secs, [open, high, low, close])| {
            json!([open_unix_secs, open, high, low, close, "1.5", "20.25", 300])
        })
        .collect();
    let last_unix_secs = rows.last().map_or(0, |&(open_unix_secs, _)| open_unix_secs);
    json!({ "error": [], "result": { pair: candles, "last": last_unix_secs } }).to_string()
}

/// The pair name an answer about `market` is keyed by.
pub(crate) fn kraken_market_key(market: Market) -> &'static str {
    match market {
        Market::Asset(ticker) => kraken_key(ticker),
        Market::Rate(currency) => kraken_rate_key(currency).expect("only the dollar has no pair"),
    }
}

/// Kraken's result keys, kept independently of the crate's own table so a
/// change to either has to be made twice, on purpose.
pub(crate) const fn kraken_key(ticker: Ticker) -> &'static str {
    match ticker {
        Ticker::Btc => "XXBTZUSD",
        Ticker::Ltc => "XLTCZUSD",
        Ticker::Doge => "XDGUSD",
        Ticker::Bch => "BCHUSD",
        Ticker::Zec => "XZECZUSD",
        Ticker::Eth => "XETHZUSD",
        Ticker::Bnb => "BNBUSD",
        Ticker::Pol => "POLUSD",
        Ticker::Avax => "AVAXUSD",
        Ticker::Sol => "SOLUSD",
        Ticker::Trx => "TRXUSD",
        Ticker::Xrp => "XXRPZUSD",
        Ticker::Gram => "TONUSD",
        Ticker::Xmr => "XXMRZUSD",
        Ticker::Link => "LINKUSD",
        Ticker::Wbtc => "WBTCUSD",
        Ticker::Uni => "UNIUSD",
        Ticker::Shib => "SHIBUSD",
        Ticker::Ena => "ENAUSD",
        Ticker::Aave => "AAVEUSD",
        Ticker::Pump => "PUMPUSD",
        Ticker::Ondo => "ONDOUSD",
        Ticker::Pepe => "PEPEUSD",
        Ticker::Arb => "ARBUSD",
        Ticker::Jup => "JUPUSD",
        Ticker::Usdt => "USDTZUSD",
        Ticker::Usdc => "USDCUSD",
        Ticker::Usds => "USDSUSD",
        Ticker::Dai => "DAIUSD",
        Ticker::Usdg => "USDGUSD",
        Ticker::Pyusd => "PYUSDUSD",
        Ticker::Xaut => "XAUTUSD",
        Ticker::Paxg => "PAXGUSD",
    }
}

/// Kraken's pair of each currency with the dollar, kept independently like
/// [`kraken_key`].
pub(crate) const fn kraken_rate_key(currency: FiatCurrency) -> Option<&'static str> {
    match currency {
        FiatCurrency::Usd => None,
        FiatCurrency::Eur => Some("ZEURZUSD"),
        FiatCurrency::Gbp => Some("ZGBPZUSD"),
        FiatCurrency::Aud => Some("AUDUSD"),
        FiatCurrency::Cad => Some("ZUSDZCAD"),
        FiatCurrency::Jpy => Some("ZUSDZJPY"),
        FiatCurrency::Chf => Some("USDCHF"),
    }
}
