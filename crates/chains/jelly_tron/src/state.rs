//! Per-address history cursors and pending sends, held in one map but persisted as
//! separate blobs, so a scan write cannot overwrite a concurrent send.
//!
//! `persist_scan` commits with the history rows, and `mark_sent` is durable
//! before a broadcast. An async mutex serialises send writes. Scan writes are
//! already single-flight.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jelly_core::{CoreError, CursorBytes, CursorScope, CursorStore, ReservationSlot, ScanStream};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::cursor::{Stream, StreamCursor};
use crate::error::TronError;
use crate::pending::Tracked;

const TARGET: &str = "jelly::tron::state";

/// Where this adapter's history walks record their frontiers, all three in one
/// blob.
const SCAN_STREAM: ScanStream = ScanStream::TronTransfers;

/// Where this adapter records the sends that have not settled yet.
const SENDS_SLOT: ReservationSlot = ReservationSlot::TronSends;

const SCAN_VERSION: u8 = 1;
const SENDS_VERSION: u8 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AddressState {
    native: StreamCursor,
    token: StreamCursor,
    internal: StreamCursor,
    pending: Vec<Tracked>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanWire {
    version: u8,
    native: StreamCursor,
    token: StreamCursor,
    internal: StreamCursor,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendsWire {
    version: u8,
    pending: Vec<Tracked>,
}

impl ScanWire {
    fn encode(state: &AddressState) -> Result<Vec<u8>, TronError> {
        let wire = Self {
            version: SCAN_VERSION,
            native: state.native.clone(),
            token: state.token.clone(),
            internal: state.internal.clone(),
        };
        serde_json::to_vec(&wire).map_err(|_| TronError::Encode { what: "the history cursors" })
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let wire: Self = serde_json::from_slice(bytes).ok()?;
        (wire.version == SCAN_VERSION).then_some(wire)
    }
}

impl SendsWire {
    /// Only sends handed to an endpoint are written, because a signed but
    /// unbroadcast build means nothing after a restart. `sending` is written as
    /// sent before its flag is set.
    fn encode(pending: &[Tracked], sending: Option<&str>) -> Result<Vec<u8>, TronError> {
        let wire = Self {
            version: SENDS_VERSION,
            pending: pending
                .iter()
                .filter(|entry| entry.is_sent || Some(entry.txid.as_str()) == sending)
                .map(|entry| Tracked { is_sent: true, ..entry.clone() })
                .collect(),
        };
        serde_json::to_vec(&wire).map_err(|_| TronError::Encode { what: "the sends in flight" })
    }

    fn decode(bytes: &[u8]) -> Option<Vec<Tracked>> {
        let wire: Self = serde_json::from_slice(bytes).ok()?;
        (wire.version == SENDS_VERSION).then_some(wire.pending)
    }
}

impl AddressState {
    const fn cursor(&self, stream: Stream) -> &StreamCursor {
        match stream {
            Stream::Native => &self.native,
            Stream::Token => &self.token,
            Stream::Internal => &self.internal,
        }
    }

    const fn cursor_mut(&mut self, stream: Stream) -> &mut StreamCursor {
        match stream {
            Stream::Native => &mut self.native,
            Stream::Token => &mut self.token,
            Stream::Internal => &mut self.internal,
        }
    }

    const fn is_complete(&self) -> bool {
        self.native.is_complete && self.token.is_complete && self.internal.is_complete
    }
}

#[derive(Debug)]
pub(crate) struct States {
    cursors: Arc<dyn CursorStore>,
    map: Mutex<BTreeMap<i64, AddressState>>,
    /// Held across composing the send blob and writing it, so two writers cannot
    /// land out of order.
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

    pub(crate) fn ensure_readable(&self) -> Result<(), TronError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(unreadable_sends());
        }
        Ok(())
    }

    /// Re-reading durable coverage replays a dropped event window without changing
    /// the sends already held in memory.
    pub(crate) async fn load(&self, address_ids: &[i64]) -> Result<(), TronError> {
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
            self.cursors.load_scans(&scopes, &[SCAN_STREAM]).await.map_err(TronError::Cursor)?;
        let sends = self
            .cursors
            .load_reservations(&missing, &[SENDS_SLOT])
            .await
            .map_err(TronError::Cursor)?;
        let mut loaded = BTreeMap::new();
        for &address_id in address_ids {
            let scope = CursorScope::Address(address_id);
            let mut state = AddressState::default();
            if let Some(row) = scans.iter().find(|row| row.scope == scope) {
                if let Some(wire) = row.cursor.with_bytes(ScanWire::decode) {
                    state.native = wire.native;
                    state.token = wire.token;
                    state.internal = wire.internal;
                } else {
                    tracing::warn!(
                        target: TARGET,
                        address_id,
                        "the stored tron cursors are unreadable; this address is walked again"
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
                    let held = held.get_mut();
                    held.native = state.native;
                    held.token = state.token;
                    held.internal = state.internal;
                }
                Entry::Vacant(slot) => {
                    slot.insert(state);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn cursor(&self, address_id: i64, stream: Stream) -> StreamCursor {
        self.map
            .lock()
            .get(&address_id)
            .map(|state| state.cursor(stream).clone())
            .unwrap_or_default()
    }

    /// True once every stream of the address has been walked all the way back.
    pub(crate) fn is_complete(&self, address_id: i64) -> bool {
        self.map.lock().get(&address_id).is_some_and(AddressState::is_complete)
    }

    pub(crate) fn set_cursor(&self, address_id: i64, stream: Stream, cursor: StreamCursor) {
        *self.map.lock().entry(address_id).or_default().cursor_mut(stream) = cursor;
    }

    /// Every send being followed, in address order for determinism.
    pub(crate) fn pending(&self) -> Vec<Tracked> {
        self.map.lock().values().flat_map(|state| state.pending.iter().cloned()).collect()
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.map.lock().values().map(|state| state.pending.len()).sum()
    }

    /// Records a send, replacing any earlier entry for the same transaction id.
    pub(crate) fn track(&self, entry: Tracked) {
        let mut map = self.map.lock();
        let state = map.entry(entry.address_id).or_default();
        state.pending.retain(|held| held.txid != entry.txid);
        state.pending.push(entry);
    }

    /// Records a send as handed to an endpoint, durably, and answers which
    /// address it belongs to.
    ///
    /// The flag is set only once the write lands. A failed write leaves the send
    /// unsent, so no later pass or writer treats as sent the bytes the broadcast
    /// reported it never sent.
    pub(crate) async fn mark_sent(&self, txid: &str) -> Result<Option<i64>, TronError> {
        let _writing = self.write.lock().await;
        self.ensure_readable()?;
        let found = {
            let map = self.map.lock();
            map.iter()
                .find(|(_, state)| state.pending.iter().any(|held| held.txid == txid))
                .map(|(id, state)| {
                    SendsWire::encode(&state.pending, Some(txid)).map(|bytes| (*id, bytes))
                })
                .transpose()?
        };
        let Some((address_id, bytes)) = found else {
            return Ok(None);
        };
        self.cursors
            .reserve(CursorScope::Address(address_id), SENDS_SLOT, CursorBytes::new(bytes))
            .await
            .map_err(TronError::Cursor)?;
        if let Some(state) = self.map.lock().get_mut(&address_id) {
            for held in state.pending.iter_mut().filter(|held| held.txid == txid) {
                held.is_sent = true;
            }
        }
        Ok(Some(address_id))
    }

    /// Stops following a send, and answers which address it belonged to.
    pub(crate) fn forget(&self, txid: &str) -> Option<i64> {
        let mut map = self.map.lock();
        for (id, state) in map.iter_mut() {
            let before = state.pending.len();
            state.pending.retain(|held| held.txid != txid);
            if state.pending.len() != before {
                return Some(*id);
            }
        }
        None
    }

    /// Stops following an unsent transaction. Its id is `sha256(raw_data)`, which
    /// the unsigned payload already fixes, so an abandoned build is named exactly.
    ///
    /// Nothing is written back, because an unsent build was never persisted.
    pub(crate) fn forget_unsent(&self, txid: &str) {
        for state in self.map.lock().values_mut() {
            state.pending.retain(|held| held.is_sent || held.txid != txid);
        }
    }

    pub(crate) fn retain(&self, address_ids: &[i64]) {
        self.map.lock().retain(|id, _| address_ids.contains(id));
    }

    /// Durable once the transaction carrying this pass's rows commits, not when
    /// this returns. A lost write costs a rescan.
    pub(crate) async fn persist_scan(&self, address_id: i64) -> Result<(), TronError> {
        let Some(bytes) = self.map.lock().get(&address_id).map(ScanWire::encode).transpose()?
        else {
            return Ok(());
        };
        self.cursors
            .advance(CursorScope::Address(address_id), SCAN_STREAM, CursorBytes::new(bytes))
            .await
            .map_err(TronError::Cursor)
    }

    /// Rewrites the address's sends after one stopped being followed. Durable
    /// when this returns.
    pub(crate) async fn persist_sends(&self, address_id: i64) -> Result<(), TronError> {
        let _writing = self.write.lock().await;
        self.ensure_readable()?;
        let Some(bytes) = self
            .map
            .lock()
            .get(&address_id)
            .map(|state| SendsWire::encode(&state.pending, None))
            .transpose()?
        else {
            return Ok(());
        };
        self.cursors
            .reserve(CursorScope::Address(address_id), SENDS_SLOT, CursorBytes::new(bytes))
            .await
            .map_err(TronError::Cursor)
    }
}

const fn unreadable_sends() -> TronError {
    TronError::Cursor(CoreError::internal_without_source("stored tron reservations are unreadable"))
}

#[cfg(test)]
mod tests {
    use jelly_core::test_util::MemoryCursorStore;

    use super::*;
    use crate::fixtures::tracked;

    fn states() -> (States, Arc<MemoryCursorStore>) {
        let store = Arc::new(MemoryCursorStore::new());
        (States::new(Arc::<MemoryCursorStore>::clone(&store)), store)
    }

    #[tokio::test]
    async fn the_stream_cursors_and_a_sent_send_round_trip_through_the_store() {
        let (states, store) = states();
        states.load(&[7]).await.expect("a first load");
        let mut native = StreamCursor::default();
        native.advance_forward("aa", Some(9));
        states.set_cursor(7, Stream::Native, native.clone());
        let mut token = StreamCursor::default();
        token.advance_backfill(Some("Fp1"), 1);
        states.set_cursor(7, Stream::Token, token.clone());
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        states.persist_scan(7).await.expect("progress is recorded");
        states.persist_sends(7).await.expect("the send is recorded");

        let fresh = States::new(store);
        fresh.load(&[7]).await.expect("a load");
        assert_eq!(fresh.cursor(7, Stream::Native), native);
        assert_eq!(fresh.cursor(7, Stream::Token), token);
        assert_eq!(fresh.pending().len(), 1);
        assert_eq!(fresh.pending_count(), 1);
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
        assert_eq!(fresh.cursor(7, Stream::Native), StreamCursor::default());
        assert_eq!(fresh.pending_count(), 1);
        assert!(fresh.pending().iter().all(|entry| entry.is_sent));
    }

    #[test]
    fn one_unreadable_entry_refuses_the_whole_reservation() {
        let kept = serde_json::to_value(Tracked { is_sent: true, ..tracked("kept", 7) }).unwrap();
        let blob =
            serde_json::json!({ "version": SENDS_VERSION, "pending": [{"asset":"unknown"}, kept] });
        assert!(SendsWire::decode(&serde_json::to_vec(&blob).unwrap()).is_none());
    }

    #[tokio::test]
    async fn a_send_is_marked_sent_forgotten_and_found_by_id() {
        let (states, _) = states();
        states.track(tracked("one", 7));
        states.track(tracked("two", 9));
        assert_eq!(states.pending_count(), 2);
        assert_eq!(states.mark_sent("one").await.expect("it is written"), Some(7));
        assert!(states.pending().iter().any(|entry| entry.txid == "one" && entry.is_sent));
        assert_eq!(states.mark_sent("missing").await.expect("nothing to write"), None);

        assert_eq!(states.forget("two"), Some(9));
        assert_eq!(states.forget("two"), None);
        assert_eq!(states.pending_count(), 1);
    }

    #[test]
    fn an_abandoned_build_is_forgotten_and_a_sent_one_is_kept() {
        let (states, _) = states();
        states.track(tracked("unsent", 7));
        states.track(Tracked { is_sent: true, ..tracked("sent", 7) });
        states.forget_unsent("sent");
        assert_eq!(states.pending_count(), 2, "a sent transaction stays");
        states.forget_unsent("unsent");
        assert_eq!(
            states.pending().into_iter().map(|entry| entry.txid).collect::<Vec<_>>(),
            vec![String::from("sent")]
        );
    }

    #[test]
    fn tracking_the_same_id_twice_keeps_one_entry() {
        let (states, _) = states();
        states.track(tracked("one", 7));
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        assert_eq!(states.pending_count(), 1);
        assert!(states.pending().iter().all(|entry| entry.is_sent));
    }

    #[tokio::test]
    async fn dropping_an_address_from_the_watch_set_drops_its_state() {
        let (states, _) = states();
        states.load(&[7, 9]).await.expect("a load");
        states.track(tracked("one", 9));
        states.retain(&[7]);
        assert_eq!(states.pending_count(), 0);
        assert!(states.pending().is_empty());
    }

    #[tokio::test]
    async fn persisting_an_address_that_is_not_held_writes_nothing() {
        let (states, store) = states();
        states.persist_scan(7).await.expect("nothing to write");
        states.persist_sends(7).await.expect("nothing to write");
        assert!(store.is_empty());
    }

    /// A send recorded before a broadcast cannot be undone by a pass writing its
    /// cursors, because the two are separate rows.
    #[tokio::test]
    async fn progress_and_sends_are_separate_rows() {
        let (states, store) = states();
        states.load(&[7]).await.expect("a load");
        states.track(Tracked { is_sent: true, ..tracked("one", 7) });
        states.persist_sends(7).await.expect("the send is recorded");
        let mut native = StreamCursor::default();
        native.advance_forward("aa", Some(9));
        states.set_cursor(7, Stream::Native, native.clone());
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
        assert_eq!(fresh.cursor(7, Stream::Native), native);
        assert_eq!(fresh.pending_count(), 1);
    }

    #[test]
    fn an_address_is_complete_only_when_every_stream_is() {
        let (states, _) = states();
        assert!(!states.is_complete(7));
        let mut finished = StreamCursor::default();
        finished.finish_backfill();
        states.set_cursor(7, Stream::Native, finished.clone());
        assert!(!states.is_complete(7));
        states.set_cursor(7, Stream::Token, finished.clone());
        assert!(!states.is_complete(7), "the internal transfers are still unread");
        states.set_cursor(7, Stream::Internal, finished);
        assert!(states.is_complete(7));
    }
}
