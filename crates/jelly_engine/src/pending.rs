use std::collections::HashMap;
use std::fmt;
use std::ptr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use jelly_core::{
    AdapterSet, ChainAdapter, ChainId, CoreError, PendingTxId, TxPreview, TxWarning, UnsignedTx,
    duration_millis,
};
use jelly_store::AddressRow;
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use sha2::{Digest as _, Sha256};

/// Domain string of the preview digest. Every field after it is length-prefixed, so no
/// two previews frame to the same bytes.
const PREVIEW_DOMAIN: &[u8] = b"jelly-engine/v1/tx-preview";

/// How long a built transaction stays submittable.
///
/// An expired transaction's reservations are released the next time one is prepared or
/// submitted, not by a timer.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(300);

/// The digest a submit is bound to.
///
/// The interface echoes the digest of the preview the user approved, and a submit that
/// carries a different one is refused, so what was shown is what gets signed. It isn't a
/// secret, so an ordinary comparison is fine.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PreviewDigest(#[cfg_attr(feature = "ipc", specta(type = String))] [u8; Self::LEN]);

impl PreviewDigest {
    pub const LEN: usize = 32;

    /// The digest of `preview` as the user sees it, with `fee_warning` after its own
    /// warnings.
    fn of(preview: &TxPreview, fee_warning: Option<&TxWarning>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(PREVIEW_DOMAIN);
        field(&mut hasher, preview.chain.as_str().as_bytes());
        field(&mut hasher, preview.asset.key().as_bytes());
        count(&mut hasher, preview.from.len());
        for source in &preview.from {
            field(&mut hasher, source.as_bytes());
        }
        field(&mut hasher, preview.to.as_bytes());
        field(&mut hasher, &preview.amount.base_units().to_be_bytes());
        field(&mut hasher, &preview.fee.base_units().to_be_bytes());
        count(&mut hasher, preview.details.len());
        for detail in &preview.details {
            field(&mut hasher, detail.label.as_bytes());
            field(&mut hasher, detail.value.as_bytes());
        }
        count(
            &mut hasher,
            preview.warnings.len().saturating_add(usize::from(fee_warning.is_some())),
        );
        for warning in preview.warnings.iter().chain(fee_warning) {
            field(&mut hasher, warning.kind.as_str().as_bytes());
            field(&mut hasher, warning.message.as_bytes());
        }
        // Last, and only when present. Everything before them is self-delimiting and the
        // two differ in width, so which ones are present is never ambiguous.
        if let Some(rent) = preview.rent {
            field(&mut hasher, &rent.base_units().to_be_bytes());
        }
        if let Some(tag) = preview.destination_tag {
            field(&mut hasher, &tag.to_be_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// The exact inverse of [`Display`](fmt::Display), two lowercase hex digits per
    /// byte. An uppercase or mixed-case echo was never handed out, so it's refused instead
    /// of normalized.
    pub fn parse(text: &str) -> Option<Self> {
        if !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = [0u8; Self::LEN];
        hex::decode_to_slice(text, &mut bytes).ok()?;
        Some(Self(bytes))
    }
}

impl fmt::Display for PreviewDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for PreviewDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Serialize for PreviewDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PreviewDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| D::Error::custom("expected 64 lowercase hex digits"))
    }
}

fn field(hasher: &mut Sha256, bytes: &[u8]) {
    count(hasher, bytes.len());
    hasher.update(bytes);
}

fn count(hasher: &mut Sha256, value: usize) {
    hasher.update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
}

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
        Amount, AssetId, ChainError, ChainOutcome, ChainSyncProgress, Deadline, DerivationPath,
        EventSink, FeeQuote, KeyMaterial, KeySource, SignedTx, TxDetail, TxId, TxPayload,
        TxRequest, TxWarningKind, WatchedAddress,
    };
    use serde::de::value::{Error as ValueError, StrDeserializer};

    use super::*;

    fn preview() -> TxPreview {
        TxPreview {
            chain: ChainId::Bitcoin,
            asset: AssetId::Native(ChainId::Bitcoin),
            from: vec![String::from("bc1qfrom")],
            to: String::from("bc1qto"),
            destination_tag: None,
            amount: Amount::from_base_units(10_000),
            fee: Amount::from_base_units(250),
            rent: None,
            details: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// A change to the framing has to fail here first.
    #[test]
    fn the_digest_is_frozen() {
        let digest = PreviewDigest::of(&preview(), None);
        assert_eq!(
            digest.to_string(),
            "b90d2746b3961cd04fe803bb3f99282e7082a223f67366aff29c1f8446055440"
        );
    }

    #[test]
    fn a_digest_round_trips_through_its_text() {
        let digest = PreviewDigest::of(&preview(), None);
        assert_eq!(PreviewDigest::parse(&digest.to_string()), Some(digest));
        assert_eq!(PreviewDigest::parse("not-a-digest"), None);
        assert_eq!(PreviewDigest::parse(""), None);
    }

    #[test]
    fn only_the_lowercase_spelling_is_a_digest() {
        let text = PreviewDigest::of(&preview(), None).to_string();
        assert!(PreviewDigest::parse(&text).is_some());
        assert_eq!(PreviewDigest::parse(&text.to_uppercase()), None);
        let mixed = text.replacen('a', "A", 1);
        assert_ne!(mixed, text, "the fixed digest has to contain an 'a' to mix");
        assert_eq!(PreviewDigest::parse(&mixed), None);
        // The same contract on the wire, whose error message already promises it.
        deserialize(&text).unwrap();
        deserialize(&text.to_uppercase()).unwrap_err();
    }

    /// Deserializes a digest the way the interface's echo arrives.
    fn deserialize(text: &str) -> Result<PreviewDigest, ValueError> {
        PreviewDigest::deserialize(StrDeserializer::<ValueError>::new(text))
    }

    #[test]
    fn every_field_of_a_preview_changes_its_digest() {
        let base = PreviewDigest::of(&preview(), None);

        let mut amount = preview();
        amount.amount = Amount::from_base_units(10_001);
        assert_ne!(PreviewDigest::of(&amount, None), base);

        let mut fee = preview();
        fee.fee = Amount::from_base_units(251);
        assert_ne!(PreviewDigest::of(&fee, None), base);

        let mut to = preview();
        to.to = String::from("bc1qelse");
        assert_ne!(PreviewDigest::of(&to, None), base);

        let mut source = preview();
        source.from.push(String::from("bc1qsecond"));
        assert_ne!(PreviewDigest::of(&source, None), base);

        let mut order = source.clone();
        order.from.reverse();
        assert_ne!(PreviewDigest::of(&order, None), PreviewDigest::of(&source, None));

        let mut detail = preview();
        detail.details.push(TxDetail::new("Fee rate", "1 sat/vB"));
        assert_ne!(PreviewDigest::of(&detail, None), base);

        let mut warning = preview();
        warning.warnings.push(TxWarning::new(TxWarningKind::DustOutput, "dust"));
        assert_ne!(PreviewDigest::of(&warning, None), base);

        let mut tagged = preview();
        tagged.destination_tag = Some(0);
        assert_ne!(PreviewDigest::of(&tagged, None), base);
        let mut retagged = preview();
        retagged.destination_tag = Some(1);
        assert_ne!(PreviewDigest::of(&retagged, None), PreviewDigest::of(&tagged, None));

        let mut rented = preview();
        rented.rent = Some(Amount::ZERO);
        assert_ne!(PreviewDigest::of(&rented, None), base);
        let mut rerented = preview();
        rerented.rent = Some(Amount::from_base_units(1));
        assert_ne!(PreviewDigest::of(&rerented, None), PreviewDigest::of(&rented, None));
        let mut both = tagged.clone();
        both.rent = Some(Amount::ZERO);
        assert_ne!(PreviewDigest::of(&both, None), PreviewDigest::of(&tagged, None));
        assert_ne!(PreviewDigest::of(&both, None), PreviewDigest::of(&rented, None));
    }

    /// Without length prefixing these previews would frame to the same bytes.
    #[test]
    fn adjacent_fields_cannot_be_confused() {
        let mut first = preview();
        first.from = vec![String::from("aa")];
        first.to = String::from("bb");
        let mut second = preview();
        second.from = vec![String::from("aabb")];
        second.to = String::new();
        assert_ne!(PreviewDigest::of(&first, None), PreviewDigest::of(&second, None));

        let mut split = preview();
        split.from = vec![String::from("aa"), String::from("bb")];
        let mut joined = preview();
        joined.from = vec![String::from("aabb")];
        assert_ne!(PreviewDigest::of(&split, None), PreviewDigest::of(&joined, None));
    }

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
            created_at_unix_secs: 0,
        }
    }
}
