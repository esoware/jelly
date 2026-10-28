//! Rent-exempt minimums.
//!
//! Rent is no longer collected, but a transaction that would create or leave an
//! account below the rent-exempt minimum for its size fails. The figure is part
//! of what a transfer costs and what a preview shows.

use parking_lot::Mutex;

/// Bytes of book-keeping every account carries on top of its data.
const ACCOUNT_STORAGE_OVERHEAD_BYTES: u64 = 128;

/// The rate before SIMD-0437 steps it down toward 696, so every figure derived from it
/// is a ceiling, never a quote.
const LAMPORTS_PER_BYTE_YEAR_CEILING: u64 = 6_960;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RentSource {
    /// The node answered.
    Node,
    /// The node couldn't be asked, so this is the compiled-in ceiling and the preview
    /// says so.
    Ceiling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RentQuote {
    lamports: u64,
    source: RentSource,
}

impl RentQuote {
    const fn from_node(lamports: u64) -> Self {
        Self { lamports, source: RentSource::Node }
    }

    /// Used only when the node couldn't be asked. It saturates rather than failing, so
    /// an uncomputable figure can't make a transfer look free.
    const fn ceiling(bytes: u64) -> Self {
        Self { lamports: rent_ceiling(bytes), source: RentSource::Ceiling }
    }

    pub(crate) const fn lamports(self) -> u64 {
        self.lamports
    }

    /// True when this is the compiled ceiling rather than the node's answer, so
    /// the real cost is at or below what the preview shows.
    pub(crate) const fn is_ceiling(self) -> bool {
        matches!(self.source, RentSource::Ceiling)
    }
}

/// The compiled-in ceiling on the rent-exempt minimum for `bytes` of account
/// data.
const fn rent_ceiling(bytes: u64) -> u64 {
    bytes
        .saturating_add(ACCOUNT_STORAGE_OVERHEAD_BYTES)
        .saturating_mul(LAMPORTS_PER_BYTE_YEAR_CEILING)
}

/// One session's rent answers, keyed by account size. The rate moves only at a feature
/// activation, so a session-long cache can't go stale in a way that matters.
#[derive(Debug, Default)]
pub(crate) struct RentCache {
    entries: Mutex<Vec<(u64, u64)>>,
}

impl RentCache {
    fn get(&self, bytes: u64) -> Option<u64> {
        self.entries.lock().iter().find(|(size, _)| *size == bytes).map(|(_, lamports)| *lamports)
    }

    fn put(&self, bytes: u64, lamports: u64) {
        let mut entries = self.entries.lock();
        if entries.iter().any(|(size, _)| *size == bytes) {
            return;
        }
        entries.push((bytes, lamports));
    }

    /// The cached answer, else the node's answer, else the ceiling.
    pub(crate) fn quote(&self, bytes: u64, answered: Option<u64>) -> RentQuote {
        if let Some(lamports) = self.get(bytes) {
            return RentQuote::from_node(lamports);
        }
        match answered {
            Some(lamports) => {
                self.put(bytes, lamports);
                RentQuote::from_node(lamports)
            }
            None => RentQuote::ceiling(bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::usdc;

    #[test]
    fn the_ceiling_is_the_pre_simd_0437_figure_for_the_two_sizes_that_matter() {
        assert_eq!(rent_ceiling(0), 890_880);
        assert_eq!(rent_ceiling(usdc().account_bytes()), 2_039_280);
        assert_eq!(rent_ceiling(u64::MAX), u64::MAX, "an impossible size cannot look free");
    }

    #[test]
    fn a_quote_says_whether_the_node_answered() {
        let cache = RentCache::default();
        let ceiling = cache.quote(0, None);
        assert!(ceiling.is_ceiling());
        assert_eq!(ceiling.lamports(), 890_880);
        assert_eq!(cache.get(0), None, "a ceiling is never cached as if it were an answer");

        let answered = cache.quote(0, Some(890_000));
        assert!(!answered.is_ceiling());
        assert_eq!(answered, RentQuote::from_node(890_000));
        assert_eq!(cache.get(0), Some(890_000));
    }

    #[test]
    fn the_cache_answers_the_second_time_and_keeps_one_entry_per_size() {
        let cache = RentCache::default();
        let token_account_bytes = usdc().account_bytes();
        assert_eq!(cache.quote(0, Some(1)).lamports(), 1);
        assert_eq!(cache.quote(0, Some(999)).lamports(), 1, "the first answer stands");
        assert_eq!(cache.quote(token_account_bytes, Some(2)).lamports(), 2);
        assert_eq!(cache.get(token_account_bytes), Some(2));
        assert_eq!(cache.get(1), None);
    }
}
