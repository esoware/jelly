use std::collections::HashMap;
use std::ptr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use jelly_core::{
    AdapterSet, ChainAdapter, ChainId, CoreError, PendingTxId, TxPreview, TxWarning, UnsignedTx,
    duration_millis,
};
use jelly_store::AddressRow;
use parking_lot::Mutex;
use serde::Serialize;

use crate::digest::PreviewDigest;

/// How long a built transaction stays submittable.
///
/// An expired transaction's reservations are released the next time one is prepared or
/// submitted, not by a timer.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(300);

/// A built and authorized transaction awaiting submit, as the interface sees it.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedTx {
    pub id: PendingTxId,
    pub preview: TxPreview,
    pub preview_digest: PreviewDigest,
}

/// A built transaction and everything submit needs to finish it.
///
/// The preview is the only copy of the chain, the asset, and the amounts, because it's
/// what the digest covers and what the user approved. A second copy could disagree with
/// what was signed.
///
/// `unsigned` stays exactly as the adapter built it, because a signer may rebuild the
/// preview from the payload and refuse one that differs. The engine's own fee warning is
/// kept beside it and only joins the preview the user sees and the digest covers.
///
/// `adapter` built it and holds its outpoint locks or nonce slot. A replacement adapter,
/// such as one a rebuild publishes, never saw those reservations, so the transaction is
/// only spendable while `adapter` is the published one. The handle is weak so it never
/// keeps a retired adapter alive.
#[derive(Debug, Clone)]
pub(crate) struct PendingTx {
    /// The rows of the preview's sources, in its order, whose keys sign it.
    pub(crate) signers: Vec<AddressRow>,
    /// The first of them, whose address the transaction's sent record is filed under.
    pub(crate) record_address_id: i64,
    pub(crate) unsigned: UnsignedTx,
    fee_warning: Option<TxWarning>,
    pub(crate) digest: PreviewDigest,
    pub(crate) adapter: Weak<dyn ChainAdapter>,
    created_elapsed_millis: u64,
}

impl PendingTx {
    pub(crate) fn new(
        signers: Vec<AddressRow>,
        unsigned: UnsignedTx,
        fee_warning: Option<TxWarning>,
        adapter: &Arc<dyn ChainAdapter>,
        created_elapsed_millis: u64,
    ) -> Result<Self, CoreError> {
        let record_address_id = signers
            .first()
            .map(|row| row.id)
            .ok_or(CoreError::internal_without_source("a transaction has no source to sign it"))?;
        let digest = PreviewDigest::of(&unsigned.preview, fee_warning.as_ref());
        Ok(Self {
            signers,
            record_address_id,
            unsigned,
            fee_warning,
            digest,
            adapter: Arc::downgrade(adapter),
            created_elapsed_millis,
        })
    }

    fn is_held_by(&self, adapter: &Arc<dyn ChainAdapter>) -> bool {
        ptr::addr_eq(self.adapter.as_ptr(), Arc::as_ptr(adapter))
    }

    pub(crate) const fn preview(&self) -> &TxPreview {
        &self.unsigned.preview
    }

    pub(crate) const fn chain(&self) -> ChainId {
        self.unsigned.chain()
    }

    pub(crate) fn view(&self, id: PendingTxId) -> PreparedTx {
        let mut preview = self.unsigned.preview.clone();
        preview.warnings.extend(self.fee_warning.clone());
        PreparedTx { id, preview, preview_digest: self.digest }
    }

    fn is_expired(&self, now_millis: u64) -> bool {
        now_millis.saturating_sub(self.created_elapsed_millis) >= duration_millis(PENDING_TTL)
    }
}

/// The authorized-but-unsent transactions of the open session.
///
/// A token is single use. It's removed at the point of no return and never put back, so
/// the same preview can't reach the network twice.
#[derive(Debug, Default)]
pub(crate) struct PendingTxs {
    /// `None` proves that expiry or an adapter rebuild kept this ID from being sent.
    /// Consumed and discarded IDs are absent, and never authorize an automatic retry.
    entries: Mutex<HashMap<PendingTxId, Option<PendingTx>>>,
}

impl PendingTxs {
    pub(crate) fn insert(&self, id: PendingTxId, tx: PendingTx) {
        self.entries.lock().insert(id, Some(tx));
    }

    /// A copy of the entry, for work that must happen before the token is spent.
    pub(crate) fn get(&self, id: PendingTxId) -> Result<PendingTx, CoreError> {
        self.entries
            .lock()
            .get(&id)
            .cloned()
            .ok_or(CoreError::UnknownPendingTx(id))?
            .ok_or(CoreError::RebuildRequired)
    }

    /// Spends the token. The entry is gone whatever happens next.
    pub(crate) fn take(&self, id: PendingTxId) -> Result<PendingTx, CoreError> {
        self.entries.lock().remove(&id).flatten().ok_or(CoreError::UnknownPendingTx(id))
    }

    /// Spends the token unless it has expired or `adapter`, the one about to broadcast it,
    /// doesn't hold its reservations. Both are checked under the same lock as the spend. A
    /// refused entry stays for [`PendingTxs::stale`] to hand out for release.
    pub(crate) fn take_spendable(
        &self,
        id: PendingTxId,
        now_millis: u64,
        adapter: &Arc<dyn ChainAdapter>,
    ) -> Result<PendingTx, CoreError> {
        let mut entries = self.entries.lock();
        let tx = entries
            .get(&id)
            .ok_or(CoreError::UnknownPendingTx(id))?
            .as_ref()
            .ok_or(CoreError::RebuildRequired)?;
        if tx.is_expired(now_millis) || !tx.is_held_by(adapter) {
            return Err(CoreError::RebuildRequired);
        }
        entries.remove(&id).flatten().ok_or(CoreError::UnknownPendingTx(id))
    }

    /// Removes and returns every entry that has expired or whose reservations the
    /// published adapter for its chain doesn't hold.
    pub(crate) fn stale(&self, now_millis: u64, adapters: &AdapterSet) -> Vec<PendingTx> {
        self.entries
            .lock()
            .values_mut()
            .filter_map(|entry| {
                let tx = entry.as_ref()?;
                if tx.is_expired(now_millis)
                    || !adapters.adapter(tx.chain()).is_some_and(|adapter| tx.is_held_by(adapter))
                {
                    entry.take()
                } else {
                    None
                }
            })
            .collect()
    }

    /// Called on lock, where the adapters holding the reservations drop with the session,
    /// so there's nothing to release.
    pub(crate) fn clear(&self) {
        self.entries.lock().clear();
    }

    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn views(&self) -> Vec<PreparedTx> {
        self.entries
            .lock()
            .iter()
            .filter_map(|(&id, tx)| tx.as_ref().map(|tx| tx.view(id)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::iter::once;

    use async_trait::async_trait;
    use jelly_core::{
        ChainError, ChainOutcome, ChainSyncProgress, Deadline, DerivationPath, EventSink, FeeQuote,
        KeyMaterial, KeySource, SignedTx, TxId, TxPayload, TxRequest, TxWarningKind,
        WatchedAddress,
    };

    use super::*;
    use crate::fixtures::preview;

    /// Stands in for the adapter that built a transaction. Only its identity is read.
    #[derive(Debug)]
    struct StandIn;

    #[async_trait]
    impl ChainAdapter for StandIn {
        fn chain(&self) -> ChainId {
            ChainId::Bitcoin
        }

        async fn set_watch_set(&self, _addresses: &[WatchedAddress]) -> Result<(), ChainError> {
            Ok(())
        }

        async fn sync(
            &self,
            _deadline: Deadline,
            _events: &dyn EventSink,
        ) -> Result<ChainSyncProgress, ChainError> {
            Ok(ChainSyncProgress::new(ChainId::Bitcoin))
        }

        async fn quote_fee(&self, _request: &TxRequest) -> Result<FeeQuote, ChainError> {
            Err(ChainError::unsupported("the stand-in quotes nothing"))
        }

        async fn build_tx(&self, _request: &TxRequest) -> Result<UnsignedTx, ChainError> {
            Err(ChainError::unsupported("the stand-in builds nothing"))
        }

        fn sign_tx(&self, _tx: &UnsignedTx, _keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
            Err(ChainError::unsupported("the stand-in signs nothing"))
        }

        async fn broadcast(&self, _tx: &SignedTx) -> Result<TxId, ChainError> {
            Err(ChainError::unsupported("the stand-in broadcasts nothing"))
        }
    }

    fn adapter() -> Arc<dyn ChainAdapter> {
        Arc::new(StandIn)
    }

    fn published(adapter: &Arc<dyn ChainAdapter>) -> AdapterSet {
        once((ChainId::Bitcoin, ChainOutcome::Built(Arc::clone(adapter)))).collect()
    }

    fn unsigned() -> UnsignedTx {
        UnsignedTx { preview: preview(), payload: TxPayload::from_bytes(vec![1]) }
    }

    fn built(adapter: &Arc<dyn ChainAdapter>, created_elapsed_millis: u64) -> PendingTx {
        PendingTx::new(vec![row()], unsigned(), None, adapter, created_elapsed_millis).unwrap()
    }

    /// A signer that rebuilds the preview from the payload refuses any other preview.
    #[test]
    fn the_engines_fee_warning_is_approved_but_never_handed_to_the_signer() {
        let warning = TxWarning::new(TxWarningKind::HighFee, "The fee is high.");
        let held =
            PendingTx::new(vec![row()], unsigned(), Some(warning.clone()), &adapter(), 0).unwrap();
        assert_eq!(held.unsigned, unsigned());

        let view = held.view(PendingTxId::from_bytes([1u8; PendingTxId::LEN]));
        assert_eq!(view.preview.warnings, [warning]);
        assert_eq!(view.preview_digest, PreviewDigest::of(&view.preview, None));
        assert_ne!(view.preview_digest, PreviewDigest::of(&preview(), None));
    }

    #[test]
    fn a_token_is_spent_exactly_once() {
        let pending = PendingTxs::default();
        let id = PendingTxId::from_bytes([1u8; PendingTxId::LEN]);
        pending.insert(id, built(&adapter(), 0));
        pending.get(id).unwrap();
        pending.take(id).unwrap();
        assert!(matches!(pending.take(id), Err(CoreError::UnknownPendingTx(_))));
    }

    #[test]
    fn expiry_removes_only_what_has_run_out() {
        let pending = PendingTxs::default();
        let ttl = duration_millis(PENDING_TTL);
        let adapter = adapter();
        for (index, created) in [0, 5_000, ttl.saturating_sub(1)].into_iter().enumerate() {
            let id = PendingTxId::from_bytes([u8::try_from(index).unwrap_or(0); PendingTxId::LEN]);
            pending.insert(id, built(&adapter, created));
        }
        // Five seconds past the first one's window, the first two have been held for the
        // whole window or more and the third has nearly all of it to go.
        assert_eq!(pending.stale(ttl.saturating_add(5_000), &published(&adapter)).len(), 2);
        assert_eq!(pending.views().len(), 1);
    }

    #[test]
    fn an_expired_token_cannot_be_spent_and_waits_to_be_released() {
        let pending = PendingTxs::default();
        let ttl = duration_millis(PENDING_TTL);
        let adapter = adapter();
        let id = PendingTxId::from_bytes([1u8; PendingTxId::LEN]);
        pending.insert(id, built(&adapter, 0));

        assert!(matches!(
            pending.take_spendable(id, ttl, &adapter),
            Err(CoreError::RebuildRequired)
        ));
        assert_eq!(
            pending.stale(ttl, &published(&adapter)).len(),
            1,
            "it is still there to release"
        );

        pending.insert(id, built(&adapter, 0));
        let last_moment = ttl.saturating_sub(1);
        pending.take_spendable(id, last_moment, &adapter).unwrap();
        assert!(matches!(
            pending.take_spendable(id, last_moment, &adapter),
            Err(CoreError::UnknownPendingTx(_))
        ));
    }

    /// A rebuild publishes a fresh adapter that never saw the reservation.
    #[test]
    fn only_the_adapter_that_built_a_token_can_spend_it() {
        let pending = PendingTxs::default();
        let (retired, rebuilt) = (adapter(), adapter());
        let id = PendingTxId::from_bytes([1u8; PendingTxId::LEN]);
        pending.insert(id, built(&retired, 0));

        assert!(matches!(pending.take_spendable(id, 0, &rebuilt), Err(CoreError::RebuildRequired)));
        assert!(pending.stale(0, &published(&retired)).is_empty());
        assert_eq!(pending.stale(0, &published(&rebuilt)).len(), 1);
        assert!(pending.views().is_empty());
    }

    fn row() -> AddressRow {
        AddressRow {
            id: 1,
            wallet_id: 1,
            chain: ChainId::Bitcoin,
            address: String::from("bc1qfrom"),
            source: KeySource::Derived { path: DerivationPath::new("m/84'/0'/0'/0/0") },
            script_kind: None,
            derivation_index: Some(0),
            label: None,
        }
    }
}
