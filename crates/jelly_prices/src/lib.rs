//! Background price feed. Wallet operations read cached prices and never wait
//! for a provider.
//!
//! Every asset is priced in US dollars, and a price in another currency is that
//! dollar price times the dollar's rate in it, never a pair of the asset against
//! that currency. Prices are integers, with excess decimals truncated, and
//! serialize as strings of base units. A quote is stored only when the response
//! names the ticker or currency that was requested.
//!
//! Daily closes, for prices and rates alike, are loaded from Kraken's OHLC candles
//! whenever prices are enabled, with each series that fails asked for again until it
//! loads, and published separately from the live feed. Turning prices off withdraws
//! the closes and the live quotes alike. A chart's candles come from the same
//! endpoint when one is shown, and are reused while they are fresh.

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
