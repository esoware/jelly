use std::fmt;
use std::future::pending;

use async_trait::async_trait;

use crate::address::Address;
use crate::chain::ChainId;
use crate::clock::Deadline;
use crate::error::ChainError;
use crate::event::{ChainSyncProgress, EventSink};
use crate::secret::KeyMaterial;
use crate::tx::{FeeQuote, FeeSelection, SignedTx, TxId, TxRequest, UnsignedTx};
use crate::wallet::WatchedAddress;

/// Contract implemented by each chain adapter.
///
/// - `build_tx` must decode everything the user will authorize in the preview.
/// - Only one `sync` call may run per instance. Other calls wait.
/// - Adapters emit in-flight progress. The engine publishes terminal progress.
///
/// A chain's entry handles address derivation and validation. Sync reports balances and the
/// chain tip through events. The engine stores them for later queries.
#[async_trait]
pub trait ChainAdapter: Send + Sync + fmt::Debug {
    fn chain(&self) -> ChainId;

    /// Replaces the entire watch set to match the store.
    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError>;

    /// Runs one pass within `deadline`, emits progress, and returns its final position.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError>;

    /// Waits for node activity that warrants an early sync. Notifications carry no data and
    /// may be spurious.
    ///
    /// Implementations must be cancellation-safe because the engine races this future
    /// against its poll timer. Listener failures must back off internally, not cause a busy
    /// loop. The default never resolves.
    async fn wait_activity(&self) {
        pending::<()>().await;
    }

    /// Prices the transaction at `request.fee`, reading a fee level from the market
    /// the way [`ChainAdapter::build_tx`] would, and prices every level beside it.
    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError>;

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError>;

    /// Pure, with no network access. `keys` holds the key of each address in
    /// `tx.preview.from`, in that order. It borrows them and must not retain any.
    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError>;

    /// One attempt against one endpoint. Never rotate a broadcast. Ambiguous failures retain
    /// reservations because the transaction may have reached the network.
    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError>;

    fn fee_bump(&self) -> Option<&dyn FeeBump> {
        None
    }

    fn tx_cancel(&self) -> Option<&dyn TxCancel> {
        None
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        None
    }

    fn subaddresses(&self) -> Option<&dyn Subaddresses> {
        None
    }

    fn script_probe(&self) -> Option<&dyn ScriptProbe> {
        None
    }
}

/// The key of a transaction whose preview names one sending address, as every chain but
/// a UTXO chain builds.
pub fn sole_key(keys: &[KeyMaterial]) -> Result<&KeyMaterial, ChainError> {
    match keys {
        [key] => Ok(key),
        _ => Err(ChainError::signing("a transaction from one address takes one key")),
    }
}

/// Speeds up an unconfirmed transaction, by replacing it with one that pays more or, on a
/// chain whose nodes relay no replacement, by a child that spends its change to pay for
/// both.
///
/// Quotes and selections use the chain's own fee units.
///
/// - Only the price comes from the caller. Read the sender, recipient, asset, amount, and
///   reserved inputs, nonce, or change from the original transaction. Ignore non-price
///   fields in `fee`.
/// - A fee level must meet the network's minimum for the bump.
/// - Reject prices below that minimum rather than silently raising them.
#[async_trait]
pub trait FeeBump: Send + Sync + fmt::Debug {
    async fn quote_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError>;

    async fn build_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError>;
}

/// Replaces an unconfirmed transaction with one that moves nothing, so the original can
/// never confirm.
///
/// Quotes and selections use the chain's own fee units, and the replacement rules of
/// [`FeeBump`] hold. Only the price comes from the caller. The sender and the slot come
/// from the original transaction, and the replacement pays nothing to anyone.
#[async_trait]
pub trait TxCancel: Send + Sync + fmt::Debug {
    async fn quote_cancel(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError>;

    async fn build_cancel(&self, txid: &TxId, fee: &FeeSelection)
    -> Result<UnsignedTx, ChainError>;
}

/// Tracks reserved outpoints or account-chain slots.
///
/// Account-chain adapters and Zcash persist reservations before broadcast so a crash
/// cannot leave a sent transaction untracked. Other UTXO outpoint locks stay in memory.
#[async_trait]
pub trait PendingTracker: Send + Sync + fmt::Debug {
    /// Releases a transaction's reservations. Callers use it only when the transaction
    /// can never land, never after an ambiguous broadcast failure, because those bytes
    /// may have reached a mempool.
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError>;
}

/// Finds the script flavours a key is already used under.
///
/// A chain whose wallets commonly hold one key under several flavours, as TON wallets
/// hold it under several wallet contract versions, has a key that enters the wallet
/// checked once for the others.
#[async_trait]
pub trait ScriptProbe: Send + Sync + fmt::Debug {
    /// The ones of `addresses` the chain shows in use: holding a balance, deployed, or
    /// with any history.
    async fn used(&self, addresses: &[Address]) -> Result<Vec<Address>, ChainError>;
}

/// Chains whose new addresses are subaddresses of one account rather than new
/// derivations.
#[async_trait]
pub trait Subaddresses: Send + Sync + fmt::Debug {
    /// Widens the scanner's registered window so `(major, minor)` is
    /// recognized without a rescan.
    async fn register_window(&self, major: u32, minor: u32) -> Result<(), ChainError>;
}
