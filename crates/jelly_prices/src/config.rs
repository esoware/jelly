use std::collections::BTreeSet;

use crate::ticker::Ticker;

/// What the feed, the history loader, and the charts ask the provider about.
///
/// Product policy, not a user setting. The feed can't be pointed at another host,
/// because the price provider is trusted to say what a wallet is worth.
#[derive(Debug, Clone)]
pub struct PriceFeedConfig {
    /// An empty set sends no request at all, not even for rates, which price
    /// nothing on their own.
    pub tickers: BTreeSet<Ticker>,
}

impl Default for PriceFeedConfig {
    fn default() -> Self {
        Self { tickers: Ticker::all().collect() }
    }
}
