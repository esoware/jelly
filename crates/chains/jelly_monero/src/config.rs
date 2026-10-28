//! The adapter's tunables. The defaults are what ships, and what the network fixes
//! lives in [`crate::params`].

use std::time::Duration;

/// The defaults target public remote nodes, which are bandwidth limited and
/// rate-limit wide block ranges.
#[derive(Debug, Clone)]
pub struct MoneroConfig {
    /// Blocks fetched per round trip.
    pub batch_blocks: u64,
    /// Upper bound on the blocks one pass scans, so a restore from a distant
    /// birthday returns partial progress instead of running for hours.
    pub blocks_per_pass: u64,
    /// Blocks below the frontier a pass re-reads after a fork shows, so a
    /// reorganization this shallow is located by the same request that fetches new
    /// blocks. Re-scanning a block is idempotent.
    pub reorg_overlap_blocks: u64,
    /// Minimum wall time between two scan-state checkpoints.
    pub checkpoint_interval: Duration,
    /// Inputs per transaction, beyond which a send must be split.
    ///
    /// Monero limits a transaction's weight, not its inputs. wallet2 aims for two
    /// thirds of the limit, which a ring of sixteen reaches at about 145 inputs.
    pub max_inputs: usize,
    /// Accounts registered with the scanner.
    pub lookahead_major: u32,
    /// Addresses per account registered with the scanner.
    pub lookahead_minor: u32,
    /// Decoys are chosen against one block, so a payload older than this many
    /// blocks is refused instead of signed against a stale ring.
    pub max_payload_age_blocks: u64,
    /// A broadcast transaction still unseen after this many blocks, and no longer held
    /// by the node it was sent to, is treated as dropped: its inputs are released and
    /// it's reported failed.
    pub abandon_after_blocks: u64,
}

impl Default for MoneroConfig {
    fn default() -> Self {
        Self {
            batch_blocks: 100,
            blocks_per_pass: 20_000,
            reorg_overlap_blocks: 10,
            checkpoint_interval: Duration::from_secs(30),
            max_inputs: 140,
            lookahead_major: 10,
            lookahead_minor: 200,
            max_payload_age_blocks: 60,
            abandon_after_blocks: 720,
        }
    }
}

impl MoneroConfig {
    /// Blocks per round trip, never zero.
    pub(crate) const fn batch(&self) -> u64 {
        if self.batch_blocks == 0 { 1 } else { self.batch_blocks }
    }

    /// Blocks one pass may scan, never zero.
    pub(crate) const fn pass_blocks(&self) -> u64 {
        if self.blocks_per_pass == 0 { 1 } else { self.blocks_per_pass }
    }

    /// Inputs per transaction, never zero.
    pub(crate) const fn inputs(&self) -> usize {
        if self.max_inputs == 0 { 1 } else { self.max_inputs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee::estimate_weight;
    use crate::params::REORG_WINDOW;

    #[test]
    fn the_default_overlap_fits_inside_the_reorg_window() {
        let config = MoneroConfig::default();
        assert!(
            config.reorg_overlap_blocks <= REORG_WINDOW,
            "an overlap wider than the window remembers cannot locate a fork"
        );
    }

    /// wallet2's `TX_WEIGHT_TARGET` is two thirds of the largest transaction the
    /// network relays, which is half the 300000-byte minimum full reward zone less
    /// 600 bytes, or 149400.
    #[test]
    fn the_default_input_ceiling_fits_wallet2s_weight_target() {
        let target = 99_600u64;
        let inputs = MoneroConfig::default().inputs();
        assert!(estimate_weight(inputs) <= target);
        assert!(inputs > 16, "the output limit of 16 is not an input limit");
    }

    #[test]
    fn a_configuration_mistake_is_clamped_rather_than_used() {
        let zeroed = MoneroConfig {
            batch_blocks: 0,
            blocks_per_pass: 0,
            max_inputs: 0,
            ..Default::default()
        };
        assert_eq!(zeroed.batch(), 1);
        assert_eq!(zeroed.pass_blocks(), 1);
        assert_eq!(zeroed.inputs(), 1);
    }
}
