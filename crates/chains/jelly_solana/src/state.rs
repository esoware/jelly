//! Per-address scan progress and pending sends, held in one map but persisted as
//! separate blobs, so a scan write can't overwrite a concurrent send.
//!
//! `persist_scan` commits with the scanned rows, and `persist_sends` is durable before
//! a broadcast. An async mutex serializes send writes. Scan writes are already
//! single-flight.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jelly_core::{CoreError, CursorBytes, CursorScope, CursorStore, ReservationSlot, ScanStream};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::cursor::SignatureCursor;
use crate::error::SolanaError;
use crate::pending::Tracked;
use crate::token::{SOLANA_TOKENS, SolanaToken};

const TARGET: &str = "jelly::solana::state";

/// Where the signature walks' cursors are stored.
const SCAN_STREAM: ScanStream = ScanStream::SolanaSignatures;

/// Where the sends that haven't settled yet are stored.
const SENDS_SLOT: ReservationSlot = ReservationSlot::SolanaSends;

const SCAN_VERSION: u8 = 1;
const SENDS_VERSION: u8 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AddressState {
    /// The owner's own signature walk.
    scan: SignatureCursor,
    /// The walk of each carried token's associated account, keyed by the token's
    /// asset key.
    token_scans: BTreeMap<String, SignatureCursor>,
    pending: Vec<Tracked>,
    /// The first attempts still out, by signature. Never persisted, because no attempt
    /// outlives the process.
    first_attempts: BTreeMap<String, FirstAttempts>,
    is_scan_dirty: bool,
    is_sends_dirty: bool,
}

/// The first attempts at one signature that haven't answered yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FirstAttempts {
    outstanding: usize,
    /// True once one began while another was out, after which no single refusal
    /// says the bytes never left.
    is_overlapped: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanWire {
    version: u8,
    scan: SignatureCursor,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tokens: BTreeMap<String, SignatureCursor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendsWire {
    version: u8,
    pending: Vec<Tracked>,
}

impl ScanWire {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let wire: Self = serde_json::from_slice(bytes).ok()?;
        (wire.version == SCAN_VERSION).then_some(wire)
    }
}

impl SendsWire {
    /// Only sends handed to an endpoint are written, because a signed but unbroadcast
    /// build means nothing after a restart. `sending` is written as sent before it's
    /// marked so.
    fn encode(pending: &[Tracked], sending: Option<&str>) -> Result<Vec<u8>, SolanaError> {
        let pending = pending
            .iter()
            .filter(|entry| entry.is_sent || Some(entry.signature.as_str()) == sending)
            .map(|entry| Tracked { is_sent: true, ..entry.clone() })
            .collect();
        let wire = Self { version: SENDS_VERSION, pending };
        serde_json::to_vec(&wire).map_err(|_| SolanaError::Encode { what: "sends in flight" })
    }

    fn decode(bytes: &[u8]) -> Option<Vec<Tracked>> {
        let wire: Self = serde_json::from_slice(bytes).ok()?;
        (wire.version == SENDS_VERSION).then_some(wire.pending)
    }
}

#[derive(Debug)]
pub(crate) struct States {
    cursors: Arc<dyn CursorStore>,
    map: Mutex<BTreeMap<i64, AddressState>>,
    /// Held across composing the send blob and writing it, so two writers can't land
    /// out of order.
    write: AsyncMutex<()>,
    is_blocked: AtomicBool,
}

impl States {
    pub(crate) fn new(cursors: Arc<dyn CursorStore>) -> Self {
        Self {
            cursors,
            map: Mutex::new(BTreeMap::new()),
            write: AsyncMutex::new(()),
            is_blocked: AtomicBool::new(false),
        }
    }

    pub(crate) fn ensure_readable(&self) -> Result<(), SolanaError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(unreadable_sends());
        }
        Ok(())
    }

    /// Reloads the scan cursors of every address in `address_ids`, so history whose
    /// events were dropped is walked again. Sends are loaded only for addresses not
    /// already held, so the sends and attempts in memory are kept.
    pub(crate) async fn load(&self, address_ids: &[i64]) -> Result<(), SolanaError> {
        let _writing = self.write.lock().await;
        self.ensure_readable()?;
        let missing: Vec<CursorScope> = {
            let map = self.map.lock();
            address_ids
                .iter()
                .copied()
                .filter(|id| !map.contains_key(id))
                .map(CursorScope::Address)
                .collect()
        };
        let scopes: Vec<CursorScope> =
            address_ids.iter().copied().map(CursorScope::Address).collect();
        let scans =
            self.cursors.load_scans(&scopes, &[SCAN_STREAM]).await.map_err(SolanaError::Cursor)?;
        let sends = self
            .cursors
            .load_reservations(&missing, &[SENDS_SLOT])
            .await
            .map_err(SolanaError::Cursor)?;
        let mut loaded = BTreeMap::new();
        for &address_id in address_ids {
            let scope = CursorScope::Address(address_id);
            let mut state = AddressState::default();
            if let Some(row) = scans.iter().find(|row| row.scope == scope) {
                if let Some(wire) = row.cursor.with_bytes(ScanWire::decode) {
                    state.scan = wire.scan;
                    state.token_scans = wire.tokens;
                } else {
                    tracing::warn!(
                        target: TARGET,
                        address_id,
                        "the stored solana scan cursor is unreadable; this address is scanned again"
                    );
                }
            }
            if let Some(row) = sends.iter().find(|row| row.scope == scope) {
                if let Some(pending) = row.payload.with_bytes(SendsWire::decode).filter(|entries| {
                    entries.iter().all(|entry| entry.address_id == address_id && entry.is_sent)
                }) {
                    state.pending = pending;
                } else {
                    self.is_blocked.store(true, Ordering::Release);
                    return Err(unreadable_sends());
                }
            }
            loaded.insert(address_id, state);
        }
        let mut map = self.map.lock();
        for (address_id, state) in loaded {
            match map.entry(address_id) {
                Entry::Occupied(mut held) => {
                    let current = held.get_mut();
                    current.scan = state.scan;
                    current.token_scans = state.token_scans;
                    current.is_scan_dirty = false;
                }
                Entry::Vacant(free) => {
                    free.insert(state);
                }
            }
        }
        Ok(())
    }

    /// The cursor of the owner's own walk when `token` is `None`, else of that
    /// token's associated account.
    pub(crate) fn scan(&self, address_id: i64, token: Option<SolanaToken>) -> SignatureCursor {
        let map = self.map.lock();
        let Some(state) = map.get(&address_id) else { return SignatureCursor::default() };
        match token {
            None => state.scan.clone(),
            Some(token) => state.token_scans.get(&token.asset().key()).cloned().unwrap_or_default(),
        }
    }

    pub(crate) fn set_scan(
        &self,
        address_id: i64,
        token: Option<SolanaToken>,
        scan: SignatureCursor,
    ) {
        let mut map = self.map.lock();
        let state = map.entry(address_id).or_default();
        state.is_scan_dirty = true;
        match token {
            None => state.scan = scan,
            Some(token) => {
                state.token_scans.insert(token.asset().key(), scan);
            }
        }
    }

    /// True once the owner's history and every carried token account's history
    /// have each been read at least once.
    pub(crate) fn is_complete(&self, address_id: i64) -> bool {
        self.scan(address_id, None).is_complete()
            && SOLANA_TOKENS
                .into_iter()
                .all(|token| self.scan(address_id, Some(token)).is_complete())
    }

    /// The sends a pass may act on, in address order for determinism.
    ///
    /// A send whose first attempt is still out is left to that attempt, because a
    /// resend meanwhile would make that attempt's refusal meaningless.
    pub(crate) fn pending(&self) -> Vec<Tracked> {
        self.map
            .lock()
            .values()
            .flat_map(|state| {
                state
                    .pending
                    .iter()
                    .filter(|held| !state.first_attempts.contains_key(&held.signature))
                    .cloned()
            })
            .collect()
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.map.lock().values().map(|state| state.pending.len()).sum()
    }

    /// Records a send, replacing any earlier entry for the same signature but
    /// keeping its sent mark, so re-signing never undoes a durable record.
    pub(crate) fn track(&self, mut entry: Tracked) {
        let mut map = self.map.lock();
        let state = map.entry(entry.address_id).or_default();
        entry.is_sent |=
            state.pending.iter().any(|held| held.signature == entry.signature && held.is_sent);
        state.is_sends_dirty |= entry.is_sent;
        state.pending.retain(|held| held.signature != entry.signature);
        state.pending.push(entry);
    }

    /// Writes a send as handed to an endpoint, then marks it so in memory, and returns
    /// its address.
    ///
    /// The mark follows a successful write, so a send whose record failed is never
    /// followed up or resent. It's made before the write lock is released, so no later
    /// writer composes a blob without it.
    pub(crate) async fn mark_sent(&self, signature: &str) -> Result<Option<i64>, SolanaError> {
        let _writing = self.write.lock().await;
        self.ensure_readable()?;
        let composed = self
            .map
            .lock()
            .iter()
            .find(|(_, state)| state.pending.iter().any(|held| held.signature == signature))
            .map(|(id, state)| (*id, SendsWire::encode(&state.pending, Some(signature))));
        let Some((address_id, bytes)) = composed else { return Ok(None) };
        self.cursors
            .reserve(CursorScope::Address(address_id), SENDS_SLOT, CursorBytes::new(bytes?))
            .await
            .map_err(SolanaError::Cursor)?;
        if let Some(state) = self.map.lock().get_mut(&address_id) {
            for held in &mut state.pending {
                if held.signature == signature {
                    held.is_sent = true;
                }
            }
        }
        Ok(Some(address_id))
    }

    /// Marks a first attempt at `signature` as out until the returned guard drops.
    ///
    /// The guard knows whether the bytes may already have left another way, through an
    /// earlier attempt or another first attempt still out.
    pub(crate) fn first_attempt(&self, signature: &str) -> FirstAttempt<'_> {
        let mut map = self.map.lock();
        let followed = map.iter_mut().find_map(|(id, state)| {
            let is_already_sent =
                state.pending.iter().find(|held| held.signature == signature)?.is_sent;
            Some((*id, is_already_sent, state))
        });
        let Some((address_id, is_already_sent, state)) = followed else {
            return FirstAttempt {
                states: self,
                signature: signature.to_owned(),
                address_id: None,
                is_alone: false,
            };
        };
        let attempts = state.first_attempts.entry(signature.to_owned()).or_default();
        let is_alone = !is_already_sent && attempts.outstanding == 0;
        attempts.is_overlapped |= attempts.outstanding > 0;
        attempts.outstanding = attempts.outstanding.saturating_add(1);
        FirstAttempt {
            states: self,
            signature: signature.to_owned(),
            address_id: Some(address_id),
            is_alone,
        }
    }

    /// Stops following a send whose first attempt was refused, and returns its address.
    ///
    /// A refusal speaks only for its own attempt, so a send whose bytes may have left
    /// another way is still followed.
    pub(crate) fn forget_refused(&self, attempt: &FirstAttempt<'_>) -> Option<i64> {
        let address_id = attempt.address_id.filter(|_| attempt.is_alone)?;
        let mut map = self.map.lock();
        let state = map.get_mut(&address_id)?;
        if state.first_attempts.get(&attempt.signature).is_some_and(|held| held.is_overlapped) {
            return None;
        }
        let before = state.pending.len();
        state.pending.retain(|held| held.signature != attempt.signature);
        if state.pending.len() == before {
            return None;
        }
        state.is_sends_dirty = true;
        Some(address_id)
    }

    /// Stops following a send, and returns its address.
    pub(crate) fn forget(&self, signature: &str) -> Option<i64> {
        let mut map = self.map.lock();
        for (id, state) in map.iter_mut() {
            let before = state.pending.len();
            state.pending.retain(|held| held.signature != signature);
            if state.pending.len() != before {
                state.is_sends_dirty = true;
                return Some(*id);
            }
        }
        None
    }

    /// Stops following every send `is_abandoned` picks that was never handed to an
    /// endpoint. Nothing is marked for writing, because such a send was never persisted.
    pub(crate) fn forget_unsent(&self, is_abandoned: impl Fn(&Tracked) -> bool) {
        for state in self.map.lock().values_mut() {
            state.pending.retain(|held| held.is_sent || !is_abandoned(held));
        }
    }

    pub(crate) fn retain(&self, watched: &[i64]) {
        self.map.lock().retain(|id, _| watched.contains(id));
    }

    pub(crate) async fn persist_dirty(&self) -> Result<(), SolanaError> {
        let dirty: Vec<_> = self
            .map
            .lock()
            .iter()
            .filter(|(_, state)| state.is_scan_dirty || state.is_sends_dirty)
            .map(|(id, state)| (*id, state.is_scan_dirty, state.is_sends_dirty))
            .collect();
        for (address_id, is_scan_dirty, is_sends_dirty) in dirty {
            if is_scan_dirty {
                self.persist_scan(address_id).await?;
            }
            if is_sends_dirty {
                self.persist_sends(address_id).await?;
            }
        }
        Ok(())
    }

    /// Durable once the database transaction carrying this pass's rows commits, not
    /// when this returns. Cancellation before the store accepts it keeps the cursor
    /// dirty.
    pub(crate) async fn persist_scan(&self, address_id: i64) -> Result<(), SolanaError> {
        let Some(snapshot) = self.map.lock().get(&address_id).map(|state| ScanWire {
            version: SCAN_VERSION,
            scan: state.scan.clone(),
            tokens: state.token_scans.clone(),
        }) else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|_| SolanaError::Encode { what: "scan cursor" })?;
        self.cursors
            .advance(CursorScope::Address(address_id), SCAN_STREAM, CursorBytes::new(bytes))
            .await
            .map_err(SolanaError::Cursor)?;
        if let Some(state) = self.map.lock().get_mut(&address_id)
            && state.scan == snapshot.scan
            && state.token_scans == snapshot.tokens
        {
            state.is_scan_dirty = false;
        }
        Ok(())
    }

    /// Durable when this returns, so a broadcast may follow. A send nobody recorded is
    /// a payment nobody would follow up.
    pub(crate) async fn persist_sends(&self, address_id: i64) -> Result<(), SolanaError> {
        let _writing = self.write.lock().await;
        self.ensure_readable()?;
        let Some(snapshot) = self.map.lock().get(&address_id).map(|state| state.pending.clone())
        else {
            return Ok(());
        };
        let bytes = SendsWire::encode(&snapshot, None)?;
        self.cursors
            .reserve(CursorScope::Address(address_id), SENDS_SLOT, CursorBytes::new(bytes))
            .await
            .map_err(SolanaError::Cursor)?;
        if let Some(state) = self.map.lock().get_mut(&address_id)
            && state.pending == snapshot
        {
            state.is_sends_dirty = false;
        }
        Ok(())
    }
}

const fn unreadable_sends() -> SolanaError {
    SolanaError::Cursor(CoreError::internal_without_source(
        "stored solana reservations are unreadable",
    ))
}

/// One first attempt at a signature, out until this drops.
#[derive(Debug)]
pub(crate) struct FirstAttempt<'a> {
    states: &'a States,
    signature: String,
    /// The address following the send, or `None` when nothing follows it.
    address_id: Option<i64>,
    /// True when no earlier attempt sent these bytes and no other first attempt
    /// was out when this one began.
    is_alone: bool,
}

impl Drop for FirstAttempt<'_> {
    fn drop(&mut self) {
        let Some(address_id) = self.address_id else { return };
        let mut map = self.states.map.lock();
        let Some(state) = map.get_mut(&address_id) else { return };
        let Some(attempts) = state.first_attempts.get_mut(&self.signature) else { return };
        attempts.outstanding = attempts.outstanding.saturating_sub(1);
        if attempts.outstanding == 0 {
            state.first_attempts.remove(&self.signature);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use async_trait::async_trait;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use jelly_core::{HeldReservation, ScanAdvance, TokenId, test_util::MemoryCursorStore};

    use super::*;
    use crate::fixtures::{tracked, usdc};

    #[derive(Debug, Default)]
    struct GatedStore {
        stored: MemoryCursorStore,
        writes: AsyncMutex<()>,
    }

    #[async_trait]
    impl CursorStore for GatedStore {
        async fn load_scans(
            &self,
            scopes: &[CursorScope],
            streams: &[ScanStream],
        ) -> Result<Vec<ScanAdvance>, CoreError> {
            self.stored.load_scans(scopes, streams).await
        }

        async fn load_reservations(
            &self,
            scopes: &[CursorScope],
            slots: &[ReservationSlot],
        ) -> Result<Vec<HeldReservation>, CoreError> {
            self.stored.load_reservations(scopes, slots).await
        }

        async fn advance(
            &self,
            scope: CursorScope,
            stream: ScanStream,
            cursor: CursorBytes,
        ) -> Result<(), CoreError> {
            let _writing = self.writes.lock().await;
            self.stored.advance(scope, stream, cursor).await
        }

        async fn reserve(
            &self,
            scope: CursorScope,
            slot: ReservationSlot,
            payload: CursorBytes,
        ) -> Result<(), CoreError> {
            let _writing = self.writes.lock().await;
            self.stored.reserve(scope, slot, payload).await
        }
    }

    #[tokio::test]
    async fn a_canceled_scan_write_is_retried_without_another_cursor_change() {
        let store = Arc::new(GatedStore::default());
        let states = States::new(Arc::<GatedStore>::clone(&store));
        states.set_scan(
            7,
            None,
            SignatureCursor { newest: Some("latest".to_owned()), ..SignatureCursor::default() },
        );
        let blocked = store.writes.lock().await;
        tokio::select! {
            biased;
            result = states.persist_scan(7) => panic!("the store is blocked: {result:?}"),
            () = ready(()) => {}
        }
        assert_eq!(store.stored.advance_count(), 0);
        drop(blocked);
        states.persist_dirty().await.unwrap();
        assert_eq!(store.stored.advance_count(), 1);
        states.persist_dirty().await.unwrap();
        assert_eq!(store.stored.advance_count(), 1, "a quiet pass adds no write");
        let fresh = States::new(store);
        fresh.load(&[7]).await.unwrap();
        assert_eq!(fresh.scan(7, None).newest.as_deref(), Some("latest"));
    }

    #[tokio::test]
    async fn a_canceled_settlement_write_does_not_resurrect_the_send_after_retry() {
        let store = Arc::new(GatedStore::default());
        let states = States::new(Arc::<GatedStore>::clone(&store));
        states.track(Tracked { is_sent: true, ..tracked("settled", 7) });
        states.persist_sends(7).await.unwrap();
        assert_eq!(states.forget("settled"), Some(7));
        let blocked = store.writes.lock().await;
        tokio::select! {
            biased;
            result = states.persist_sends(7) => panic!("the store is blocked: {result:?}"),
            () = ready(()) => {}
        }
        assert_eq!(store.stored.reserve_count(), 1);
        assert_eq!(states.pending_count(), 0);
        drop(blocked);
        states.persist_dirty().await.unwrap();
        assert_eq!(store.stored.reserve_count(), 2);
        states.persist_dirty().await.unwrap();
        assert_eq!(store.stored.reserve_count(), 2);
        let fresh = States::new(store);
        fresh.load(&[7]).await.unwrap();
        assert_eq!(fresh.pending_count(), 0);
    }

    fn states() -> (States, Arc<MemoryCursorStore>) {
        let store = Arc::new(MemoryCursorStore::new());
        (States::new(Arc::<MemoryCursorStore>::clone(&store)), store)
    }

    #[tokio::test]
    async fn a_scan_a_token_walk_and_a_send_round_trip_through_the_store() {
        let usdc = usdc();
        let (states, store) = states();
        states.load(&[7]).await.expect("a first load");
        states.set_scan(
            7,
            None,
            SignatureCursor {
                newest: Some(String::from("sig")),
                is_backfill_complete: true,
                ..SignatureCursor::default()
            },
        );
        states.set_scan(
            7,
            Some(usdc),
            SignatureCursor { newest: Some(String::from("token")), ..SignatureCursor::default() },
        );
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        states.persist_scan(7).await.expect("progress is recorded");
        states.persist_sends(7).await.expect("the send is recorded");

        let fresh = States::new(store);
        fresh.load(&[7]).await.expect("a load");
        assert_eq!(fresh.scan(7, None).newest.as_deref(), Some("sig"));
        assert_eq!(fresh.scan(7, Some(usdc)).newest.as_deref(), Some("token"));
        assert!(!fresh.is_complete(7), "a token account's history is still unread");
        assert_eq!(fresh.pending().len(), 1);
        assert_eq!(fresh.pending_count(), 1);
    }

    /// An untouched token walk is absent from the sparse map.
    #[test]
    fn a_scan_blob_with_no_token_walks_still_reads() {
        let wire =
            ScanWire::decode(br#"{"version":1,"scan":{"newest":"a","backfillComplete":true}}"#)
                .expect("a sparse current blob");
        assert_eq!(wire.scan.newest.as_deref(), Some("a"));
        assert!(wire.tokens.is_empty());
    }

    #[tokio::test]
    async fn a_token_walk_missing_from_the_stored_scan_starts_fresh_and_holds_back_completion() {
        let (states, store) = states();
        let complete = SignatureCursor {
            newest: Some("latest".to_owned()),
            is_backfill_complete: true,
            ..SignatureCursor::default()
        };
        states.set_scan(7, None, complete.clone());
        for token in SOLANA_TOKENS.into_iter().filter(|token| token.id() != TokenId::PaxgSpl) {
            states.set_scan(7, Some(token), complete.clone());
        }
        states.persist_scan(7).await.unwrap();
        let fresh = States::new(store);
        fresh.load(&[7]).await.unwrap();
        assert_eq!(fresh.scan(7, None), complete);
        assert!(!fresh.is_complete(7));
        let paxg = SolanaToken::of(TokenId::PaxgSpl).unwrap();
        assert_eq!(fresh.scan(7, Some(paxg)), SignatureCursor::default());
        fresh.set_scan(7, Some(paxg), complete);
        assert!(fresh.is_complete(7));
    }

    #[tokio::test]
    async fn a_build_that_was_never_sent_is_not_written_out() {
        let (states, store) = states();
        states.load(&[7]).await.expect("a load");
        states.track(tracked("unsent", 7));
        states.persist_sends(7).await.expect("the send blob is written");

        let fresh = States::new(store);
        fresh.load(&[7]).await.expect("a load");
        assert_eq!(fresh.pending_count(), 0);
    }

    #[tokio::test]
    async fn an_abandoned_build_is_forgotten_by_its_message_without_a_write() {
        let mut raw = vec![1u8];
        raw.extend([0u8; 64]);
        raw.extend_from_slice(b"the message");
        let encoded = BASE64.encode(&raw);

        let (states, store) = states();
        states.track(Tracked { raw: encoded.clone(), ..tracked("unsent", 7) });
        states.forget_unsent(|held| held.is_signed_message(b"another message"));
        assert_eq!(states.pending_count(), 1);
        states.forget_unsent(|held| held.is_signed_message(b"the message"));
        assert_eq!(states.pending_count(), 0);
        states.persist_dirty().await.unwrap();
        assert_eq!(store.reserve_count(), 0, "an unsent build was never written");

        states.track(Tracked { raw: encoded, is_sent: true, ..tracked("sent", 7) });
        states.forget_unsent(|held| held.is_signed_message(b"the message"));
        assert_eq!(states.pending_count(), 1, "a sent transaction stays");
    }

    #[tokio::test]
    async fn unreadable_reservations_block_repeated_loads_and_every_send_write() {
        for bytes in [
            b"not json".as_slice(),
            br#"{"version":99,"pending":[]}"#,
            br#"{"version":1,"pending":[{}]}"#,
        ] {
            let store = Arc::new(MemoryCursorStore::new());
            store
                .reserve(CursorScope::Address(7), SENDS_SLOT, CursorBytes::copied(bytes))
                .await
                .unwrap();
            let states = States::new(Arc::<MemoryCursorStore>::clone(&store));
            let (first, second) = tokio::join!(states.load(&[1, 7]), states.load(&[1, 7]));
            assert!(first.is_err());
            assert!(second.is_err());
            assert!(states.map.lock().is_empty());
            states.retain(&[]);
            states.track(tracked("new", 7));
            assert!(states.ensure_readable().is_err());
            states.mark_sent("new").await.unwrap_err();
            assert!(states.persist_sends(7).await.is_err());
            assert!(states.load(&[7]).await.is_err());
            assert!(
                store
                    .load_reservation(CursorScope::Address(7), SENDS_SLOT)
                    .await
                    .unwrap()
                    .unwrap()
                    .with_bytes(|stored| stored == bytes)
            );
            assert_eq!(store.reserve_count(), 1);
        }
    }

    #[tokio::test]
    async fn an_unreadable_scan_preserves_the_current_reservations() {
        let (states, store) = states();
        states.load(&[7]).await.unwrap();
        states.track(Tracked { is_sent: true, ..tracked("held", 7) });
        states.persist_sends(7).await.unwrap();
        store
            .advance(CursorScope::Address(7), SCAN_STREAM, CursorBytes::copied(b"not json"))
            .await
            .unwrap();
        states.load(&[7]).await.unwrap();
        assert_eq!(states.pending_count(), 1);
        let fresh = States::new(store);
        fresh.load(&[7]).await.unwrap();
        assert_eq!(fresh.scan(7, None), SignatureCursor::default());
        assert_eq!(fresh.pending_count(), 1);
        assert!(fresh.pending().iter().all(|entry| entry.is_sent));
    }

    #[tokio::test]
    async fn a_send_is_marked_sent_forgotten_and_found_by_signature() {
        let (states, store) = states();
        states.track(tracked("one", 7));
        states.track(tracked("two", 9));
        assert_eq!(states.pending_count(), 2);
        assert_eq!(states.mark_sent("one").await.expect("the send is recorded"), Some(7));
        assert!(states.pending().iter().any(|entry| entry.signature == "one" && entry.is_sent));
        assert_eq!(states.mark_sent("missing").await.expect("nothing to record"), None);
        let fresh = States::new(store);
        fresh.load(&[7]).await.expect("a load");
        assert!(fresh.pending().iter().all(|entry| entry.signature == "one" && entry.is_sent));
        assert_eq!(fresh.pending_count(), 1);

        assert_eq!(states.forget("two"), Some(9));
        assert_eq!(states.forget("two"), None);
        assert_eq!(states.pending_count(), 1);
    }

    #[tokio::test]
    async fn a_refusal_forgets_only_a_send_no_other_attempt_could_have_forwarded() {
        let (states, _) = states();
        states.track(tracked("first", 7));
        let first = states.first_attempt("first");
        states.mark_sent("first").await.unwrap();
        assert!(states.pending().is_empty(), "a pass leaves a send alone while it is out");
        assert_eq!(states.forget_refused(&first), Some(7));
        drop(first);

        states.track(Tracked { is_sent: true, ..tracked("retry", 7) });
        let retry = states.first_attempt("retry");
        assert_eq!(states.forget_refused(&retry), None, "the earlier attempt may have landed");
        drop(retry);

        states.track(tracked("overlapped", 7));
        let one = states.first_attempt("overlapped");
        let other = states.first_attempt("overlapped");
        drop(other);
        assert!(states.pending().iter().all(|held| held.signature != "overlapped"));
        assert_eq!(states.forget_refused(&one), None, "the other attempt may have forwarded it");
        drop(one);
        assert_eq!(states.pending().len(), 2, "both kept sends are a pass's to follow again");
    }

    #[tokio::test]
    async fn tracking_the_same_signature_twice_keeps_one_entry() {
        let (states, _) = states();
        states.track(tracked("one", 7));
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        assert_eq!(states.pending_count(), 1);
        assert!(states.pending().iter().all(|entry| entry.is_sent));
    }

    #[tokio::test]
    async fn resigning_a_sent_signature_preserves_its_durable_tracking() {
        let (states, store) = states();
        states.track(tracked("one", 7));
        assert_eq!(states.mark_sent("one").await.unwrap(), Some(7));
        let before = States::new(Arc::<MemoryCursorStore>::clone(&store));
        before.load(&[7]).await.unwrap();
        assert_eq!(before.pending(), vec![Tracked { is_sent: true, ..tracked("one", 7) }]);

        states.track(tracked("one", 7));
        states.persist_dirty().await.unwrap();

        let fresh = States::new(store);
        fresh.load(&[7]).await.unwrap();
        assert_eq!(
            fresh.pending(),
            vec![Tracked { is_sent: true, ..tracked("one", 7) }],
            "signing identical bytes again must not erase an already-sent reservation"
        );
    }

    #[tokio::test]
    async fn dropping_an_address_from_the_watch_set_drops_its_state() {
        let (states, _) = states();
        states.load(&[7]).await.expect("a load");
        states.load(&[9]).await.expect("a load");
        states.track(tracked("one", 9));
        states.retain(&[7]);
        assert_eq!(states.pending_count(), 0);
        assert_eq!(states.pending().len(), 0);
    }

    #[tokio::test]
    async fn persisting_an_address_that_is_not_held_writes_nothing() {
        let (states, store) = states();
        states.persist_scan(7).await.expect("nothing to write");
        states.persist_sends(7).await.expect("nothing to write");
        assert!(store.is_empty());
    }

    /// A send recorded before a broadcast cannot be undone by a pass writing its
    /// scan cursor, because the two are separate rows.
    #[tokio::test]
    async fn progress_and_sends_are_separate_rows() {
        let (states, store) = states();
        states.load(&[7]).await.expect("a load");
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        states.persist_sends(7).await.expect("the send is recorded");
        states.set_scan(
            7,
            None,
            SignatureCursor { newest: Some(String::from("sig")), ..SignatureCursor::default() },
        );
        states.persist_scan(7).await.expect("progress is recorded");

        assert_eq!(store.reserve_count(), 1);
        assert_eq!(store.advance_count(), 1);
        assert!(
            store.load_scan(CursorScope::Address(7), SCAN_STREAM).await.expect("a read").is_some()
        );
        assert!(
            store
                .load_reservation(CursorScope::Address(7), SENDS_SLOT)
                .await
                .expect("a read")
                .is_some()
        );

        let fresh = States::new(store);
        fresh.load(&[7]).await.expect("a load");
        assert_eq!(fresh.scan(7, None).newest.as_deref(), Some("sig"));
        assert_eq!(fresh.pending_count(), 1);
    }
}
