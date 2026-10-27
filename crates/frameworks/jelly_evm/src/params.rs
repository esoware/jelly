use std::time::Duration;

use jelly_core::{AssetId, ChainId, TokenId};

/// What one EIP-155 network varies by, declared once as a `const` by its chain crate.
///
/// Keys, addresses and the transaction format are identical on every network, and only
/// these figures differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvmParams {
    pub chain: ChainId,
    /// Signed into every transaction to prevent cross-chain replay, and checked
    /// against `eth_chainId` before an endpoint serves its first request.
    pub chain_id: u64,
    /// Budget for one JSON-RPC or history request.
    pub request_timeout: Duration,
    /// Token metadata comes from this compiled-in list, never the chain.
    pub tokens: &'static [TokenId],
    /// The token standard as a person reads it, for the preview.
    pub token_standard: &'static str,
    pub block_millis: u64,
    /// The least time between two push-driven wakes, which collapses a burst of
    /// `newHeads` pushes into one pass.
    pub watch_floor: Duration,
    /// Blocks sampled by `eth_feeHistory`.
    pub fee_history_blocks: u64,
    /// Floor for `maxPriorityFeePerGas`.
    pub min_priority_fee_wei: u128,
    /// Ceiling for an automatic `maxPriorityFeePerGas`, where a few senders'
    /// generous tips would otherwise lift the median far above what the network's
    /// own fee oracle suggests.
    pub max_priority_fee_wei: Option<u128>,
    /// Confirmations after which a tracked send stops being watched, and at which a
    /// nonce some other transaction took counts as taken for good.
    pub confirmations_target: u32,
    /// The widest `eth_getLogs` window, which a walk starts at.
    ///
    /// The window halves when an endpoint says the range is too wide and doubles
    /// back after a clean window. It never grows past this, because rediscovering
    /// a width no endpoint serves costs a refused request every pass.
    pub log_window_blocks: u64,
    /// The earliest block that can hold a `Transfer` of any supported contract, so
    /// the backfill stops there instead of at genesis.
    pub backfill_floor: u64,
    /// The node logs every native transfer, as Bor does from its system token
    /// contract, so the log walk finds native history without an indexer.
    pub has_native_transfer_logs: bool,
    /// The OP Stack charges for posting a transaction's data to Ethereum apart
    /// from its gas. Its `GasPriceOracle` predeploy quotes the charge and each
    /// receipt reports it as `l1Fee`. Arbitrum folds the same cost into gas, so it
    /// has none.
    pub has_l1_data_fee: bool,
    /// The shipped history service answers without a key, as Routescan's does, so
    /// it is read whether or not an Etherscan key is saved, and that key is never
    /// sent to it.
    pub has_keyless_history: bool,
}

impl EvmParams {
    pub const fn native(&self) -> AssetId {
        AssetId::Native(self.chain)
    }

    /// The native coin's ticker, which is BNB rather than BSC on BNB Smart Chain.
    pub const fn symbol(&self) -> &'static str {
        self.chain.ticker()
    }

    /// Each forward pass re-reads this many blocks behind its frontier to catch
    /// reorged transfers. The overlap widens an existing request, and records
    /// overwrite by txid.
    pub fn rescan_overlap_blocks(&self) -> u64 {
        u64::from(self.confirmations_target)
    }

    /// Blocks a history service may trail the node by, two minutes on every
    /// network. A complete indexed read claims no coverage closer to the tip than
    /// this, so the next read still asks for rows the service indexes late.
    pub const fn indexer_lag_blocks(&self) -> u64 {
        INDEXER_LAG_SECS.saturating_mul(1_000).div_ceil(self.block_millis)
    }
}

const INDEXER_LAG_SECS: u64 = 120;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{ARBITRUM, AVALANCHE, BASE, BSC, ETHEREUM, OPTIMISM, POLYGON};

    #[test]
    fn the_indexer_lag_is_the_same_wall_clock_on_every_network() {
        for params in [ETHEREUM, ARBITRUM, BASE, OPTIMISM, BSC, POLYGON, AVALANCHE] {
            let seconds = |blocks: u64| {
                blocks.saturating_mul(params.block_millis).checked_div(1_000).unwrap_or_default()
            };
            assert!(seconds(params.indexer_lag_blocks()) >= INDEXER_LAG_SECS, "{:?}", params.chain);
        }
    }
}
