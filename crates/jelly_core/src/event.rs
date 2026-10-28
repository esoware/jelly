use std::fmt;

use async_trait::async_trait;

use crate::amount::Amount;
use crate::asset::AssetId;
use crate::chain::ChainId;
use crate::tx::{TxId, TxRecord};

/// Sync progress as an integer in `0..=1000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainSyncProgress {
    chain: ChainId,
    progress_permille: u16,
    synced_height: Option<u64>,
    tip_height: Option<u64>,
}

impl ChainSyncProgress {
    pub const MAX_PERMILLE: u16 = 1000;

    pub const fn new(chain: ChainId) -> Self {
        Self { chain, progress_permille: 0, synced_height: None, tip_height: None }
    }

    #[must_use]
    pub const fn with_progress(mut self, permille: u16) -> Self {
        self.progress_permille =
            if permille > Self::MAX_PERMILLE { Self::MAX_PERMILLE } else { permille };
        self
    }

    /// Progress as `done` of `total`, which [`ChainSyncProgress::permille_of`] converts.
    #[must_use]
    pub fn with_fraction(self, done: u64, total: u64) -> Self {
        self.with_progress(Self::permille_of(done, total))
    }

    /// `done` of `total` in permille, rounded down and capped at full. A total of zero is
    /// full, since a chain with nothing to scan is synced.
    pub fn permille_of(done: u64, total: u64) -> u16 {
        let full = u128::from(Self::MAX_PERMILLE);
        let scaled = u128::from(done.min(total))
            .saturating_mul(full)
            .checked_div(u128::from(total))
            .unwrap_or(full);
        u16::try_from(scaled).unwrap_or(Self::MAX_PERMILLE)
    }

    #[must_use]
    pub const fn with_heights(mut self, synced: Option<u64>, tip: Option<u64>) -> Self {
        self.synced_height = synced;
        self.tip_height = tip;
        self
    }

    pub const fn chain(self) -> ChainId {
        self.chain
    }

    pub const fn progress_permille(self) -> u16 {
        self.progress_permille
    }

    pub const fn synced_height(self) -> Option<u64> {
        self.synced_height
    }

    pub const fn tip_height(self) -> Option<u64> {
        self.tip_height
    }

    #[cfg(feature = "test-util")]
    pub const fn is_complete(self) -> bool {
        self.progress_permille >= Self::MAX_PERMILLE
    }
}

/// One address's balance in one asset, as of the pass that observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceUpdate {
    pub address_id: i64,
    pub asset: AssetId,
    pub balance: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxConfirmation {
    pub txid: TxId,
    pub confirmations: u32,
}

/// Adapter and engine events. The engine pump coalesces some kinds and delivers the rest
/// in order.
///
/// This enum doesn't cross IPC. The shell forwards a detected transaction as its id and
/// asset, and a balance change, an update, or a confirmation as a bare signal. Progress
/// stays in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletEvent {
    /// Coalesced by `(address_id, asset)`.
    BalanceChanged(BalanceUpdate),
    /// Lossless and delivered in arrival order.
    ///
    /// An adapter emits every transaction it reads this way. The engine passes one on as
    /// detected only when the store had never held it, so a rescan announces nothing new.
    TxDetected(TxRecord),
    /// A transaction the store already held, seen again. Only the engine publishes it.
    TxUpdated(TxRecord),
    /// Lossless and delivered in arrival order.
    TxConfirmed(TxConfirmation),
    /// Coalesced by chain.
    SyncProgress(ChainSyncProgress),
}

/// Where events go. The implementation owns the channel, so no chain crate needs an async
/// runtime dependency to emit.
#[async_trait]
pub trait EventSink: Send + Sync + fmt::Debug {
    /// Delivers `event`, waiting for room when the sink applies backpressure. For events
    /// that must not be dropped.
    async fn emit(&self, event: WalletEvent);

    /// Delivers `event` without waiting and drops it when the sink is full. For events the
    /// pump coalesces anyway.
    fn try_emit(&self, event: WalletEvent);
}
