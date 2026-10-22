use std::collections::BTreeMap;
use std::time::Duration;

use jelly_core::{AssetId, FiatCurrency};
use serde::Serialize;

use crate::price::Price;
use crate::quote::Quotes;

/// A record older than this is dropped. A price from yesterday is worse than no
/// price at all.
pub(crate) const DISCARD_AFTER: Duration = Duration::from_hours(24);

/// What one asset was worth, and when the quote was taken.
///
/// A price is shown with its age so a reader can judge it, until it is older
/// than a day and is dropped.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PriceRecord {
    pub(crate) asset: AssetId,
    pub(crate) price: Price,
    pub(crate) as_of_unix_secs: i64,
}

/// Everything the feed knows, as one value that readers see whole: each asset's
/// price in US dollars, the dollar's rate in each other currency, and the
/// currency the wallet shows.
#[derive(Debug, Clone)]
pub struct PriceBook {
    currency: FiatCurrency,
    usd_prices: BTreeMap<AssetId, Dated>,
    /// How many units of each currency one US dollar bought.
    rates: BTreeMap<FiatCurrency, Dated>,
}

/// A price or rate the book holds, and when it was quoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dated {
    pub(crate) value: Price,
    pub(crate) as_of_unix_secs: i64,
}

impl PriceBook {
    pub(crate) const fn new(currency: FiatCurrency) -> Self {
        Self { currency, usd_prices: BTreeMap::new(), rates: BTreeMap::new() }
    }

    pub(crate) const fn currency(&self) -> FiatCurrency {
        self.currency
    }

    #[cfg(test)]
    pub(crate) const fn usd_prices(&self) -> &BTreeMap<AssetId, Dated> {
        &self.usd_prices
    }

    #[cfg(test)]
    pub(crate) const fn rates(&self) -> &BTreeMap<FiatCurrency, Dated> {
        &self.rates
    }

    /// Every price in the currency the wallet shows.
    ///
    /// A converted price is as old as the older of its two quotes. Without a rate
    /// for the currency there are no prices at all, because a value in the wrong
    /// currency is worse than none. A price that converts to zero or overflows is
    /// left out.
    pub fn snapshot(&self) -> PriceSnapshot {
        let usd_records = self.usd_prices.iter().map(|(&asset, &dated)| PriceRecord {
            asset,
            price: dated.value,
            as_of_unix_secs: dated.as_of_unix_secs,
        });
        let prices = if self.currency == FiatCurrency::Usd {
            usd_records.collect()
        } else if let Some(rate) = self.rates.get(&self.currency) {
            usd_records
                .filter_map(|record| {
                    let price =
                        record.price.convert(rate.value).filter(|price| !price.is_zero())?;
                    Some(PriceRecord {
                        price,
                        as_of_unix_secs: record.as_of_unix_secs.min(rate.as_of_unix_secs),
                        ..record
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        PriceSnapshot { currency: self.currency, prices }
    }

    /// How long until the oldest price or rate held grows too old to show, or `None`
    /// when none is held. Only a pass adds a record, so until the next one the oldest
    /// held is the next to go.
    pub(crate) fn until_expiry(&self, now_unix_secs: i64) -> Option<Duration> {
        let oldest_unix_secs = self
            .usd_prices
            .values()
            .chain(self.rates.values())
            .map(|dated| dated.as_of_unix_secs)
            .min()?;
        let wait_secs =
            oldest_unix_secs.saturating_sub(cutoff_unix_secs(now_unix_secs)).saturating_add(1);
        Some(Duration::from_secs(u64::try_from(wait_secs).unwrap_or(0)))
    }

    /// Drops every price and rate. True when the book held any.
    pub(crate) fn clear(&mut self) -> bool {
        let is_held = !self.usd_prices.is_empty() || !self.rates.is_empty();
        self.usd_prices.clear();
        self.rates.clear();
        is_held
    }

    /// True when the currency changed.
    pub(crate) fn set_currency(&mut self, currency: FiatCurrency) -> bool {
        let has_changed = self.currency != currency;
        self.currency = currency;
        has_changed
    }

    /// Files every accepted quote, taken at `now_unix_secs`, under each asset its
    /// ticker prices and every rate under its currency, then drops every price and
    /// rate too old to show.
    pub(crate) fn apply_pass(&mut self, quotes: &Quotes, now_unix_secs: i64) -> Published {
        let mut published = Published::default();
        for quote in &quotes.accepted {
            let usd = Dated { value: quote.usd, as_of_unix_secs: now_unix_secs };
            for &asset in quote.ticker.assets() {
                published.tally(apply(&mut self.usd_prices, asset, usd));
            }
        }
        for rate in &quotes.rates {
            let per_dollar = Dated { value: rate.per_dollar, as_of_unix_secs: now_unix_secs };
            published.tally(apply(&mut self.rates, rate.currency, per_dollar));
        }
        let cutoff_unix_secs = cutoff_unix_secs(now_unix_secs);
        published.discarded = retain_since(&mut self.usd_prices, cutoff_unix_secs)
            .saturating_add(retain_since(&mut self.rates, cutoff_unix_secs));
        published
    }
}

/// Every price the wallet shows, all in one currency.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceSnapshot {
    currency: FiatCurrency,
    /// In `AssetId` order, at most one per asset.
    prices: Vec<PriceRecord>,
}

impl PriceSnapshot {
    #[cfg(test)]
    pub(crate) const fn currency(&self) -> FiatCurrency {
        self.currency
    }

    #[cfg(test)]
    pub(crate) fn get(&self, asset: AssetId) -> Option<PriceRecord> {
        self.prices
            .binary_search_by_key(&asset, |record| record.asset)
            .ok()
            .and_then(|at| self.prices.get(at))
            .copied()
    }

    #[cfg(test)]
    pub(crate) const fn len(&self) -> usize {
        self.prices.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

/// What one pass did to the book.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Published {
    pub(crate) moved: usize,
    pub(crate) refreshed: usize,
    pub(crate) discarded: usize,
}

impl Published {
    const fn tally(&mut self, applied: Applied) {
        match applied {
            Applied::Moved => self.moved = self.moved.saturating_add(1),
            Applied::Refreshed => self.refreshed = self.refreshed.saturating_add(1),
            Applied::Ignored => {}
        }
    }

    pub(crate) const fn has_changes(self) -> bool {
        self.moved > 0 || self.refreshed > 0 || self.discarded > 0
    }
}

/// What one quote did to the book.
#[derive(Debug, Clone, Copy)]
enum Applied {
    /// Identical to what is already held.
    Ignored,
    /// The same price, taken again just now.
    Refreshed,
    Moved,
}

/// Keeps the latest quote applied per key.
///
/// Quotes arrive in fetch order from the one poller, so the latest replaces the
/// held one whatever its timestamp says. Ordering by the wall clock would freeze
/// every record after the clock stepped back.
///
/// A quote that repeats the last number still counts as a refresh, because the
/// record's age is part of what a reader judges it by.
fn apply<K: Ord>(entries: &mut BTreeMap<K, Dated>, key: K, dated: Dated) -> Applied {
    match entries.insert(key, dated) {
        Some(previous) if previous == dated => Applied::Ignored,
        Some(previous) if previous.value == dated.value => Applied::Refreshed,
        Some(_) | None => Applied::Moved,
    }
}

/// A record quoted before this is too old to show.
fn cutoff_unix_secs(now_unix_secs: i64) -> i64 {
    let discard_after_secs = i64::try_from(DISCARD_AFTER.as_secs()).unwrap_or(i64::MAX);
    now_unix_secs.saturating_sub(discard_after_secs)
}

fn retain_since<K: Ord>(entries: &mut BTreeMap<K, Dated>, cutoff_unix_secs: i64) -> usize {
    let before = entries.len();
    entries.retain(|_, dated| dated.as_of_unix_secs >= cutoff_unix_secs);
    before.saturating_sub(entries.len())
}
