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
/// - Adapters emit in-flight progress, and the engine publishes the final progress.
///
/// Address derivation and validation live in the chain's entry. Sync reports balances and
/// the chain tip through events, and the engine stores them for later queries.
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
    /// Implementations must be cancellation-safe, since the engine races this future
    /// against its poll timer, and a failing listener must back off instead of spinning.
    /// The default never resolves.
    async fn wait_activity(&self) {
        pending::<()>().await;
    }

    /// Prices the transaction at `request.fee`, reading a level's price from the market as
    /// [`ChainAdapter::build_tx`] would, and prices every other level too.
    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError>;

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError>;

    /// Pure, with no network access. `keys` holds the key of each address in
    /// `tx.preview.from`, in that order. It borrows them and must not retain any.
    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError>;

    /// One attempt against one endpoint, never rotated. An ambiguous failure keeps the
    /// reservations, since the transaction may have reached the network.
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

/// Speeds up an unconfirmed transaction by replacing it with one that pays more or, where
/// nodes relay no replacements, by a child that spends its change to pay for both.
///
/// Quotes and selections use the chain's own fee units.
///
/// - Only the price comes from the caller. Take the sender, recipient, asset, amount, and
///   reserved inputs, nonce, or change from the original transaction, and ignore non-price
///   fields in `fee`.
/// - A fee level must meet the network's minimum for the bump. A custom price below that
///   minimum is rejected, never silently raised.
#[async_trait]
pub trait FeeBump: Send + Sync + fmt::Debug {
    async fn quote_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError>;

    async fn build_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError>;
}

/// Replaces an unconfirmed transaction with one that moves nothing, so the original can
/// never confirm.
///
/// The rules of [`FeeBump`] hold: only the price comes from the caller, in the chain's own
/// fee units, and the sender and the slot come from the original transaction.
#[async_trait]
pub trait TxCancel: Send + Sync + fmt::Debug {
    async fn quote_cancel(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError>;

    async fn build_cancel(&self, txid: &TxId, fee: &FeeSelection)
    -> Result<UnsignedTx, ChainError>;
}

/// Tracks reserved outpoints or account-chain slots.
///
/// Account chains and Zcash persist reservations before broadcast, so a crash can't leave
/// a sent transaction untracked. The other UTXO chains hold their outpoint locks in memory.
#[async_trait]
pub trait PendingTracker: Send + Sync + fmt::Debug {
    /// Releases a transaction's reservations. Call it only when the transaction can never
    /// land, and never after an ambiguous broadcast failure, since those bytes may have
    /// reached a mempool.
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError>;
}

/// Finds the script flavors a key is already used under.
///
/// For chains where other wallets commonly hold one key under several flavors, as TON
/// wallets do across contract versions. A key that enters the wallet is checked once for
/// its other flavors.
#[async_trait]
pub trait ScriptProbe: Send + Sync + fmt::Debug {
    /// Those of `addresses` the chain shows in use: holding a balance, deployed, or with
    /// any history.
    async fn used(&self, addresses: &[Address]) -> Result<Vec<Address>, ChainError>;
}

/// Chains whose new addresses are subaddresses of one account rather than new
/// derivations.
#[async_trait]
pub trait Subaddresses: Send + Sync + fmt::Debug {
    /// Widens the scanner's registered window to cover `(major, minor)`, restarting the
    /// scan from the birthday if the window grows.
    async fn register_window(&self, major: u32, minor: u32) -> Result<(), ChainError>;
}
