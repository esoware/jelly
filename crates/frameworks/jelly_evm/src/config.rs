use std::time::Duration;

/// Tuning for one [`crate::EvmAdapter`] that no network varies. The figures a
/// network varies by are its [`crate::EvmParams`].
///
/// Holds no secret, so the derived `Debug` is safe to log. The history API key
/// lives inside the [`crate::HistoryApi`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmConfig {
    /// The narrowest `eth_getLogs` window the shrink stops at.
    pub min_log_window_blocks: u64,
    /// Tip-ward log windows per scan group per pass.
    pub max_forward_windows: usize,
    /// History-ward log windows per scan group per pass.
    pub max_backfill_windows: usize,
    /// How long the indexer leaves an address unread while nothing says it has
    /// news.
    ///
    /// A pass reads the indexer only for an address whose balances or nonce moved,
    /// so this sweep is what finds activity that moves neither, such as tokens an
    /// approved spender takes back within one pass. At ten minutes a quiet address
    /// costs 432 requests a day, under half a percent of the 100,000 a free
    /// Etherscan key allows.
    pub history_sweep: Duration,
    /// How long an unsigned build holds its nonce slot without an explicit discard.
    /// Signed transactions keep their slots until discarded or settled.
    pub reservation_ttl: Duration,
    /// Percent added to an `eth_estimateGas` answer.
    ///
    /// The estimate reflects state now, not at inclusion. An ERC-20 transfer to
    /// an address whose balance empties between the estimate and inclusion writes
    /// a fresh storage slot and costs about 20k more gas.
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
