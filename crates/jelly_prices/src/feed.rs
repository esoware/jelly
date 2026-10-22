use std::sync::Arc;
use std::time::Duration;

use jelly_core::{Clock, FiatCurrency};
use tokio::sync::watch;

use crate::book::{PriceBook, PriceSnapshot, Published};
use crate::config::PriceFeedConfig;
use crate::fetch::Fetch;
use crate::poll::PricePoller;
use crate::quote::Quotes;

/// The published side of the price feed, what the wallet currently believes
/// things are worth.
///
/// Cheap to clone and read from anywhere. Nothing here waits on the network, so
/// a wallet operation is never delayed by a price.
#[derive(Debug, Clone)]
pub struct PriceFeed {
    book: watch::Sender<PriceBook>,
}

impl PriceFeed {
    /// Builds the feed and the poller that fills it. It cannot fail, because the
    /// provider is compiled in.
    pub fn new(
        config: &PriceFeedConfig,
        currency: FiatCurrency,
        fetch: Arc<dyn Fetch>,
        clock: Arc<dyn Clock>,
    ) -> (Self, PricePoller) {
        let feed = Self { book: watch::Sender::new(PriceBook::new(currency)) };
        let poller = PricePoller::new(feed.clone(), &config.tickers, fetch, clock);
        (feed, poller)
    }

    /// Everything known right now, in the currency the wallet shows, as one
    /// coherent value.
    pub fn snapshot(&self) -> PriceSnapshot {
        self.book.borrow().snapshot()
    }

    /// Wakes whenever a price or rate moves, a quote is refreshed, a record ages
    /// out, or the currency changes.
    pub fn subscribe(&self) -> watch::Receiver<PriceBook> {
        self.book.subscribe()
    }

    pub fn currency(&self) -> FiatCurrency {
        self.book.borrow().currency()
    }

    /// Reprices everything from the rates already held, without a request.
    pub fn set_currency(&self, currency: FiatCurrency) {
        self.book.send_if_modified(|book| book.set_currency(currency));
    }

    pub(crate) fn clear(&self) {
        self.book.send_if_modified(PriceBook::clear);
    }

    pub(crate) fn until_expiry(&self, now_unix_secs: i64) -> Option<Duration> {
        self.book.borrow().until_expiry(now_unix_secs)
    }

    /// Applies one pass's quotes and drops what grew too old as a single edit, so a
    /// reader never sees half a pass.
    pub(crate) fn publish(&self, quotes: &Quotes, now_unix_secs: i64) -> Published {
        let mut published = Published::default();
        self.book.send_if_modified(|book| {
            published = book.apply_pass(quotes, now_unix_secs);
            published.has_changes()
        });
        published
    }
}
