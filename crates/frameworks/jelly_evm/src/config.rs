use std::time::Duration;

/// [`crate::EvmAdapter`] tuning that's the same on every network. What differs per
/// network is in [`crate::EvmParams`].
///
/// Holds no secret, so the derived `Debug` is safe to log. The history API key
/// lives in the [`crate::HistoryApi`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmConfig {
    /// The narrowest `eth_getLogs` window a refused window shrinks to.
    pub min_log_window_blocks: u64,
    /// Forward log windows per scan group per pass.
    pub max_forward_windows: usize,
    /// Backfill log windows per scan group per pass.
    pub max_backfill_windows: usize,
    /// How long an address with no news goes between indexer reads.
    ///
    /// A pass reads the indexer only for an address whose balances or nonce moved,
    /// so the sweep catches activity that nets out between two passes, such as
    /// tokens an approved spender takes straight back. At ten minutes a quiet
    /// address costs 432 requests a day, under half a percent of the 100,000 a free
    /// Etherscan key allows.
    pub history_sweep: Duration,
    /// How long an unsigned build holds its nonce slot without an explicit discard.
    /// Signed transactions keep their slots until discarded or settled.
    pub reservation_ttl: Duration,
    /// Percent added to an `eth_estimateGas` answer. A plain 21,000-gas transfer
    /// isn't padded.
    ///
    /// The estimate reflects state now, not at inclusion. An ERC-20 transfer to
    /// an address whose balance empties in between writes a fresh storage slot and
    /// costs about 20k more gas.
    pub gas_padding_pct: u64,
}

impl Default for EvmConfig {
    fn default() -> Self {
        Self {
            min_log_window_blocks: 32,
            max_forward_windows: 8,
            max_backfill_windows: 4,
            history_sweep: Duration::from_secs(600),
            reservation_ttl: Duration::from_mins(15),
            gas_padding_pct: 20,
        }
    }
}
