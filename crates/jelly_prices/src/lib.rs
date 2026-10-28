//! Background price feed from Kraken. Wallet operations read cached prices and never wait
//! for the provider.
//!
//! Every asset is priced in US dollars. A price in another currency is the dollar price
//! times the dollar's rate in that currency, never a pair of the asset against it. Prices
//! are integers, with excess decimals truncated, and serialize as strings of base units.
//!
//! While prices are on, daily closes for every ticker and rate load from Kraken's OHLC
//! candles, retrying each failed series until it loads, and publish apart from the live
//! feed. Charts fetch candles from the same endpoint on demand and reuse them while
//! they're fresh. Turning prices off withdraws both the live quotes and the closes.

#![deny(clippy::float_arithmetic)]
mod book;
mod candle;
mod chart;
mod config;
mod error;
mod feed;
mod fetch;
#[cfg(test)]
mod fixtures;
mod history;
mod kraken;
mod poll;
mod price;
mod quote;
mod schedule;
mod switch;
mod ticker;

pub use crate::book::{PriceBook, PriceSnapshot};
pub use crate::candle::{CandleInterval, CandleRequest};
pub use crate::chart::{PriceChart, PriceCharts};
pub use crate::config::PriceFeedConfig;
pub use crate::error::PriceError;
pub use crate::feed::PriceFeed;
pub use crate::fetch::Fetch;
pub use crate::history::{PriceHistory, PriceHistoryFeed, PriceHistoryLoader};
pub use crate::poll::PricePoller;
pub use crate::price::Price;
pub use crate::quote::QuoteRequest;
pub use crate::ticker::Ticker;
