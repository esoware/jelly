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

/// Domain string of the preview digest. Every field that follows it is
/// length-prefixed, so no two previews can frame to the same bytes.
const PREVIEW_DOMAIN: &[u8] = b"jelly-engine/v1/tx-preview";

/// How long a built but unsubmitted transaction stays authorised.
///
/// An expired authorisation cannot be submitted. Its reservations are released the next
/// time a transaction is prepared or submitted, rather than by a timer.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(300);

/// The digest a submit is bound to.
///
/// The interface echoes the digest of the preview the user approved, and a submit that
/// carries a different one is refused, so what was shown is what gets signed. It is not
/// a secret, so an ordinary comparison is fine.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PreviewDigest(#[cfg_attr(feature = "ipc", specta(type = String))] [u8; Self::LEN]);

impl PreviewDigest {
    pub const LEN: usize = 32;

    /// The digest of `preview`. This and nothing else defines the binding.
    fn of(preview: &TxPreview) -> Self {
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
        count(&mut hasher, preview.warnings.len());
        for warning in &preview.warnings {
            field(&mut hasher, warning.kind.as_str().as_bytes());
            field(&mut hasher, warning.message.as_bytes());
        }
        // Last and only when present. Everything before it is self-delimiting, so its
        // presence alone tells two previews apart.
        if let Some(tag) = preview.destination_tag {
            field(&mut hasher, &tag.to_be_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// The exact inverse of [`Display`](fmt::Display), two lowercase hex digits per
    /// byte. An uppercase or mixed-case echo was never handed out, so it is refused
    /// rather than normalised.
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

/// A built and authorised transaction awaiting submit, as the interface sees it.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedTx {
    pub id: PendingTxId,
    pub preview: TxPreview,
    pub preview_digest: PreviewDigest,
}

/// A built transaction and everything the submit path needs to finish it.
///
/// The preview is the only copy of the chain, the asset, and the amounts, because it is
/// what the digest covers and what the user approved. A second copy could disagree with
/// what was signed.
///
/// `unsigned` stays exactly as the adapter built it, because a signer may rebuild the
/// preview from the payload and refuse one that differs. The engine's own fee warning is
/// held beside it and joins the preview only in what the user sees and the digest covers.
///
/// `adapter` is the adapter that built it and holds its outpoint locks or nonce slot.
/// Another adapter for the same chain, such as the one a rebuild puts in its place,
/// never saw those reservations, so the transaction is only spendable while `adapter`
/// is the published one. The handle is weak, so an authorisation never keeps a retired
/// adapter alive.
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
        let digest = PreviewDigest::of(&shown(&unsigned.preview, fee_warning.as_ref()));
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
        PreparedTx {
            id,
            preview: shown(&self.unsigned.preview, self.fee_warning.as_ref()),
            preview_digest: self.digest,
        }
    }

    fn is_expired(&self, now_millis: u64) -> bool {
        now_millis.saturating_sub(self.created_elapsed_millis) >= duration_millis(PENDING_TTL)
    }
}

/// The adapter's preview with the engine's fee warning after its own, which is what the
/// user approves.
fn shown(preview: &TxPreview, fee_warning: Option<&TxWarning>) -> TxPreview {
    let mut shown = preview.clone();
    shown.warnings.extend(fee_warning.cloned());
    shown
}

/// The authorised-but-unsent transactions of the open session.
///
/// A token is single use. It is removed at the point of no return and never put back,
/// so the same preview cannot reach the network twice.
#[derive(Debug, Default)]
pub(crate) struct PendingTxs {
    /// An empty entry proves that expiry or replacement prevented this ID from being sent.
    /// Consumed and explicitly discarded IDs are absent, and never authorise an automatic
    /// retry.
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
    /// does not hold its reservations, deciding under one lock so neither can change
    /// between the check and the spend. A refused entry stays for [`PendingTxs::stale`]
    /// to hand out for release.
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

    /// Everything that has expired or whose reservations are not held by the published
    /// adapter for its chain, removed.
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

    /// Used on lock, where the adapters holding the matching reservations are dropped
    /// with the session, so there is nothing to release.
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
            details: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// A change to the framing has to fail here first.
    #[test]
    fn the_digest_is_frozen() {
        let digest = PreviewDigest::of(&preview());
        assert_eq!(
            digest.to_string(),
            "b90d2746b3961cd04fe803bb3f99282e7082a223f67366aff29c1f8446055440"
        );
    }

    #[test]
    fn a_digest_round_trips_through_its_text() {
        let digest = PreviewDigest::of(&preview());
        assert_eq!(PreviewDigest::parse(&digest.to_string()), Some(digest));
        assert_eq!(PreviewDigest::parse("not-a-digest"), None);
        assert_eq!(PreviewDigest::parse(""), None);
    }

    #[test]
    fn only_the_lowercase_spelling_is_a_digest() {
        let text = PreviewDigest::of(&preview()).to_string();
        assert!(PreviewDigest::parse(&text).is_some());
        assert_eq!(PreviewDigest::parse(&text.to_uppercase()), None);
        let mixed = text.replacen('a', "A", 1);
        assert_ne!(mixed, text, "the fixed digest has to contain an 'a' to mix");
        assert_eq!(PreviewDigest::parse(&mixed), None);
        // The same contract on the wire, whose error message already promises it.
        deserialize(&text).unwrap();
        deserialize(&text.to_uppercase()).unwrap_err();
    }

    /// One digest off the wire, through the impl the interface's echo travels.
    fn deserialize(text: &str) -> Result<PreviewDigest, ValueError> {
        PreviewDigest::deserialize(StrDeserializer::<ValueError>::new(text))
    }

    #[test]
    fn every_field_of_a_preview_changes_its_digest() {
        let base = PreviewDigest::of(&preview());

        let mut amount = preview();
        amount.amount = Amount::from_base_units(10_001);
        assert_ne!(PreviewDigest::of(&amount), base);

        let mut fee = preview();
        fee.fee = Amount::from_base_units(251);
        assert_ne!(PreviewDigest::of(&fee), base);

        let mut to = preview();
        to.to = String::from("bc1qelse");
        assert_ne!(PreviewDigest::of(&to), base);

        let mut source = preview();
        source.from.push(String::from("bc1qsecond"));
        assert_ne!(PreviewDigest::of(&source), base);

        let mut order = source.clone();
        order.from.reverse();
        assert_ne!(PreviewDigest::of(&order), PreviewDigest::of(&source));

        let mut detail = preview();
        detail.details.push(TxDetail::new("Fee rate", "1 sat/vB"));
        assert_ne!(PreviewDigest::of(&detail), base);

        let mut warning = preview();
        warning.warnings.push(TxWarning::new(TxWarningKind::DustOutput, "dust"));
        assert_ne!(PreviewDigest::of(&warning), base);

        let mut tagged = preview();
        tagged.destination_tag = Some(0);
        assert_ne!(PreviewDigest::of(&tagged), base);
        let mut retagged = preview();
        retagged.destination_tag = Some(1);
        assert_ne!(PreviewDigest::of(&retagged), PreviewDigest::of(&tagged));
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
        assert_ne!(PreviewDigest::of(&first), PreviewDigest::of(&second));

        let mut split = preview();
        split.from = vec![String::from("aa"), String::from("bb")];
        let mut joined = preview();
        joined.from = vec![String::from("aabb")];
        assert_ne!(PreviewDigest::of(&split), PreviewDigest::of(&joined));
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
        assert_eq!(view.preview_digest, PreviewDigest::of(&view.preview));
        assert_ne!(view.preview_digest, PreviewDigest::of(&preview()));
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
