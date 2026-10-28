use std::time::Duration;

use jelly_core::{AssetId, ChainId};

pub(crate) const CHAIN: ChainId = ChainId::Tron;

pub(crate) const TRX: AssetId = AssetId::Native(CHAIN);

/// Tuning for one [`crate::TronAdapter`]. The defaults suit a keyless `TronGrid`
/// user.
///
/// Holds no secret, so the derived `Debug` is safe to log. The API key lives
/// inside [`crate::PooledNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TronConfig {
    /// How long a built transaction stays valid, counted from the head block's
    /// time.
    ///
    /// TRON's `createtransaction` uses 60 seconds, which is tight when someone
    /// reads a preview first. The protocol allows up to 24 hours, but a short
    /// window means an abandoned transaction can't land long after.
    pub expiration: Duration,
    /// Pages read per stream per pass, newest first, while catching up to the last
    /// transaction reported.
    pub forward_pages: u32,
    /// Pages of older history read per stream per pass, so a long history fills in
    /// over several passes instead of hammering a public endpoint.
    pub backfill_pages: u32,
}

impl Default for TronConfig {
    fn default() -> Self {
        Self { expiration: Duration::from_secs(600), forward_pages: 2, backfill_pages: 3 }
    }
}

impl TronConfig {
    /// At least one, or a stream would never notice a new transaction.
    pub(crate) fn forward_budget(&self) -> u32 {
        self.forward_pages.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{MAX_EXPIRATION, MIN_EXPIRATION};

    #[test]
    fn the_defaults_are_inside_every_bound_this_build_signs() {
        let config = TronConfig::default();
        assert!((MIN_EXPIRATION..=MAX_EXPIRATION).contains(&config.expiration));
        assert_eq!(config.forward_budget(), 2);
    }

    #[test]
    fn a_forward_budget_of_zero_still_reads_the_newest_page() {
        let zeroed = TronConfig { forward_pages: 0, ..TronConfig::default() };
        assert_eq!(zeroed.forward_budget(), 1);
    }
}
