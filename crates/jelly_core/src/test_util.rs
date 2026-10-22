//! Deterministic test helpers behind `test-util`. Enable this feature only in dev
//! dependencies.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::mem::take;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use zeroize::Zeroizing;

use crate::address::Address;
use crate::amount::Amount;
use crate::asset::AssetId;
use crate::build::BuildRequest;
use crate::chain::ChainId;
use crate::clock::{Clock, ClockError, duration_millis};
use crate::cursor::{
    CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot, ScanAdvance,
    ScanStream,
};
use crate::error::{CoreError, KeyError};
use crate::event::{EventSink, WalletEvent};
use crate::pin::{PinStore, SpkiPin};
use crate::secret::{KeyKind, KeyMaterial};
use crate::tx::{FeeSelection, SendAmount, TxRecord, TxRequest};
use crate::wallet::{DerivationPath, KeySource, WatchedAddress};

pub const MARKER: &str = "zzzzmarker";
pub const MARKER_PHRASE: &str = "zzzzmarker qqqqsecret abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[track_caller]
pub fn assert_redacted(what: &str, input: &str, error: &(impl fmt::Display + fmt::Debug)) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    for token in input.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.chars().count() < 4 {
            continue;
        }
        assert!(!display.contains(token), "{what}: Display leaked a {}-char token", token.len());
        assert!(!debug.contains(token), "{what}: Debug leaked a {}-char token", token.len());
    }
}

#[track_caller]
pub fn assert_rejected_without_leak<T>(what: &str, input: &str, result: Result<T, KeyError>) {
    assert!(result.is_err(), "{what}: expected a rejection");
    if let Err(error) = result {
        assert_redacted(what, input, &error);
    }
}

pub fn records(events: &[WalletEvent]) -> Vec<TxRecord> {
    events
        .iter()
        .filter_map(|event| {
            if let WalletEvent::TxDetected(record) = event { Some(record.clone()) } else { None }
        })
        .collect()
}

/// A clock that only moves when a test moves it.
#[derive(Debug)]
pub struct FakeClock {
    unix_millis: AtomicU64,
    elapsed_millis: AtomicU64,
}

impl FakeClock {
    /// 2026-01-01T00:00:00Z.
    pub const DEFAULT_UNIX_MILLIS: u64 = 1_767_225_600_000;

    pub const fn new(unix_millis: u64) -> Self {
        Self { unix_millis: AtomicU64::new(unix_millis), elapsed_millis: AtomicU64::new(0) }
    }

    /// Moves both the wall clock and the monotonic reading forward.
    pub fn advance(&self, by: Duration) {
        let millis = duration_millis(by);
        self.unix_millis.fetch_add(millis, Ordering::SeqCst);
        self.elapsed_millis.fetch_add(millis, Ordering::SeqCst);
    }

    /// Moves the wall clock alone, as a clock adjustment would.
    pub fn set_unix_millis(&self, unix_millis: u64) {
        self.unix_millis.store(unix_millis, Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(Self::DEFAULT_UNIX_MILLIS)
    }
}

impl Clock for FakeClock {
    fn now_millis(&self) -> Result<u64, ClockError> {
        Ok(self.unix_millis.load(Ordering::SeqCst))
    }

    fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis.load(Ordering::SeqCst)
    }
}

/// A wall clock set before the epoch, so every reading of it fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingClock;

impl Clock for FailingClock {
    fn now_millis(&self) -> Result<u64, ClockError> {
        Err(ClockError::BeforeEpoch)
    }

    fn elapsed_millis(&self) -> u64 {
        0
    }
}

/// An event sink that records instead of delivering.
#[derive(Debug, Default)]
pub struct FakeEventSink {
    events: Mutex<Vec<WalletEvent>>,
}

impl FakeEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&self) -> Vec<WalletEvent> {
        take(&mut *self.events.lock())
    }

    pub fn records(&self) -> Vec<TxRecord> {
        records(&self.drain())
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }
}

#[async_trait]
impl EventSink for FakeEventSink {
    async fn emit(&self, event: WalletEvent) {
        self.events.lock().push(event);
    }

    fn try_emit(&self, event: WalletEvent) {
        self.events.lock().push(event);
    }
}

/// Scan advances queue until an explicit commit; reservations are durable immediately.
///
/// A [`ControlledCursorStore::discard`] drops the queue, as a failed commit does, and a
/// later commit skips any advance whose pair was last read before that discard, so
/// progress from a pass that read rolled-back state never lands.
#[derive(Debug, Default)]
pub struct ControlledCursorStore {
    committed: MemoryCursorStore,
    has_scan_error: AtomicBool,
    has_reservation_error: AtomicBool,
    scan_reads: AtomicU64,
    queued: Mutex<Vec<(u64, ScanAdvance)>>,
    discards: AtomicU64,
    discards_at_read: Mutex<HashMap<(CursorScope, ScanStream), u64>>,
}

impl ControlledCursorStore {
    pub const fn committed(&self) -> &MemoryCursorStore {
        &self.committed
    }

    pub const fn scan_failure(&self) -> &AtomicBool {
        &self.has_scan_error
    }

    pub const fn reservation_failure(&self) -> &AtomicBool {
        &self.has_reservation_error
    }

    pub fn scan_read_count(&self) -> u64 {
        self.scan_reads.load(Ordering::SeqCst)
    }

    pub async fn commit(&self) -> Result<(), CoreError> {
        self.commit_prefix(usize::MAX).await
    }

    /// Commits only the oldest advances, leaving later pump windows independently queued.
    pub async fn commit_prefix(&self, count: usize) -> Result<(), CoreError> {
        let queued: Vec<_> = {
            let mut queued = self.queued.lock();
            let count = count.min(queued.len());
            queued.drain(..count).collect()
        };
        for (discards_at_read, row) in queued {
            if discards_at_read == self.discards.load(Ordering::SeqCst) {
                self.committed.advance(row.scope, row.stream, row.cursor).await?;
            }
        }
        Ok(())
    }

    pub fn discard(&self) {
        self.queued.lock().clear();
        self.discards.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl CursorStore for ControlledCursorStore {
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        self.scan_reads.fetch_add(1, Ordering::SeqCst);
        if self.has_scan_error.load(Ordering::SeqCst) {
            return Err(CoreError::internal_without_source("injected scan read failure"));
        }
        let discards = self.discards.load(Ordering::SeqCst);
        let rows = self.committed.load_scans(scopes, streams).await?;
        let mut discards_at_read = self.discards_at_read.lock();
        for &scope in scopes {
            for &stream in streams {
                discards_at_read.insert((scope, stream), discards);
            }
        }
        Ok(rows)
    }

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        if self.has_reservation_error.load(Ordering::SeqCst) {
            return Err(CoreError::internal_without_source("injected reservation read failure"));
        }
        self.committed.load_reservations(scopes, slots).await
    }

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        let discards_at_read =
            self.discards_at_read.lock().get(&(scope, stream)).copied().unwrap_or(0);
        self.queued.lock().push((discards_at_read, ScanAdvance { scope, stream, cursor }));
        Ok(())
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        self.committed.reserve(scope, slot, payload).await
    }
}

#[derive(Debug, Default)]
pub struct MemoryPinStore {
    pins: Mutex<HashMap<(ChainId, String), SpkiPin>>,
}

impl MemoryPinStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PinStore for MemoryPinStore {
    fn get(&self, chain: ChainId, host: &str) -> Result<Option<SpkiPin>, CoreError> {
        Ok(self.pins.lock().get(&(chain, host.to_owned())).copied())
    }

    fn put(&self, chain: ChainId, host: &str, pin: SpkiPin) -> Result<(), CoreError> {
        self.pins.lock().insert((chain, host.to_owned()), pin);
        Ok(())
    }
}

/// Writes apply immediately. Counters track accepted writes so tests can detect
/// needless persistence.
#[derive(Debug, Default)]
pub struct MemoryCursorStore {
    scans: Mutex<HashMap<(CursorScope, ScanStream), CursorBytes>>,
    reservations: Mutex<HashMap<(CursorScope, ReservationSlot), CursorBytes>>,
    advance_count: AtomicU64,
    reserve_count: AtomicU64,
}

impl MemoryCursorStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.scans.lock().len().saturating_add(self.reservations.lock().len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn advance_count(&self) -> u64 {
        self.advance_count.load(Ordering::SeqCst)
    }

    pub fn reserve_count(&self) -> u64 {
        self.reserve_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CursorStore for MemoryCursorStore {
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        let guard = self.scans.lock();
        let mut scans = Vec::new();
        for &scope in scopes {
            for &stream in streams {
                if let Some(cursor) = guard.get(&(scope, stream)) {
                    scans.push(ScanAdvance { scope, stream, cursor: cursor.clone() });
                }
            }
        }
        Ok(scans)
    }

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        let guard = self.reservations.lock();
        let mut reservations = Vec::new();
        for &scope in scopes {
            for &slot in slots {
                if let Some(payload) = guard.get(&(scope, slot)) {
                    reservations.push(HeldReservation { scope, slot, payload: payload.clone() });
                }
            }
        }
        Ok(reservations)
    }

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        self.advance_count.fetch_add(1, Ordering::SeqCst);
        self.scans.lock().insert((scope, stream), cursor);
        Ok(())
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        self.reserve_count.fetch_add(1, Ordering::SeqCst);
        self.reservations.lock().insert((scope, slot), payload);
        Ok(())
    }
}

/// A cursor store that forgets every write.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullCursorStore;

#[async_trait]
impl CursorStore for NullCursorStore {
    async fn load_scans(
        &self,
        _scopes: &[CursorScope],
        _streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        Ok(Vec::new())
    }

    async fn load_reservations(
        &self,
        _scopes: &[CursorScope],
        _slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        Ok(Vec::new())
    }

    async fn advance(
        &self,
        _scope: CursorScope,
        _stream: ScanStream,
        _cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    async fn reserve(
        &self,
        _scope: CursorScope,
        _slot: ReservationSlot,
        _payload: CursorBytes,
    ) -> Result<(), CoreError> {
        Ok(())
    }
}

/// A [`MemoryCursorStore`] whose writes fail while a test says so, the way a full
/// disk would. A refused write stores nothing and is not counted.
#[derive(Debug, Default)]
pub struct RefusingCursorStore {
    stored: MemoryCursorStore,
    is_refusing_scans: AtomicBool,
    is_refusing_reservations: AtomicBool,
}

impl RefusingCursorStore {
    pub const fn scan_refusal(&self) -> &AtomicBool {
        &self.is_refusing_scans
    }

    pub const fn reservation_refusal(&self) -> &AtomicBool {
        &self.is_refusing_reservations
    }

    fn refuse_if(flag: &AtomicBool) -> Result<(), CoreError> {
        if flag.load(Ordering::SeqCst) {
            return Err(CoreError::internal_without_source("the store refused the write"));
        }
        Ok(())
    }
}

#[async_trait]
impl CursorStore for RefusingCursorStore {
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
        Self::refuse_if(&self.is_refusing_scans)?;
        self.stored.advance(scope, stream, cursor).await
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        Self::refuse_if(&self.is_refusing_reservations)?;
        self.stored.reserve(scope, slot, payload).await
    }
}

/// 32 bytes of `fill`, the right length for every `KeyKind`.
pub fn key_material(kind: KeyKind, fill: u8) -> KeyMaterial {
    key_material_of(kind, vec![fill; 32])
}

/// For test vectors and deliberately invalid lengths.
pub fn key_material_of(kind: KeyKind, bytes: Vec<u8>) -> KeyMaterial {
    KeyMaterial::new(kind, Zeroizing::new(bytes))
}

/// A build of `chains` over a fresh [`MemoryCursorStore`], with no proxy, API key, restore
/// height or account key.
pub fn build_request(chains: &[ChainId]) -> BuildRequest {
    BuildRequest {
        chains: chains.to_vec(),
        cursors: Arc::new(MemoryCursorStore::new()),
        proxy: None,
        api_keys: BTreeMap::new(),
        restore_heights: BTreeMap::new(),
        account_keys: BTreeMap::new(),
    }
}

/// Builds a [`WatchedAddress`] without a store behind it.
#[derive(Debug, Clone)]
#[must_use]
pub struct WatchedAddressBuilder {
    watched: WatchedAddress,
}

impl WatchedAddressBuilder {
    pub fn new(id: i64, chain: ChainId, address: &str) -> Self {
        Self {
            watched: WatchedAddress {
                id,
                address: Address::assume_canonical(chain, address),
                source: KeySource::Derived { path: DerivationPath::new("m/0") },
                pubkey: None,
            },
        }
    }

    pub fn derived(mut self, path: &str) -> Self {
        self.watched.source = KeySource::Derived { path: DerivationPath::new(path) };
        self
    }

    pub fn subaddress(mut self, major: u32, minor: u32) -> Self {
        self.watched.source = KeySource::Subaddress { major, minor };
        self
    }

    pub fn build(self) -> WatchedAddress {
        self.watched
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct TxRequestBuilder {
    request: TxRequest,
}

impl TxRequestBuilder {
    pub fn new(source_id: i64, to: &str, asset: AssetId, amount: Amount) -> Self {
        Self {
            request: TxRequest {
                from_address_ids: vec![source_id],
                to: to.to_owned(),
                destination_tag: None,
                comment: None,
                should_delete_account: false,
                asset,
                amount: SendAmount::Exact { value: amount },
                fee: FeeSelection::NORMAL,
                nonce: None,
                data: None,
            },
        }
    }

    pub const fn send_max(mut self) -> Self {
        self.request.amount = SendAmount::Max;
        self
    }

    /// Replaces the one source `new` named with `source_ids`, in that order.
    pub fn sources(mut self, source_ids: &[i64]) -> Self {
        self.request.from_address_ids = source_ids.to_vec();
        self
    }

    pub fn fee(mut self, fee: FeeSelection) -> Self {
        self.request.fee = fee;
        self
    }

    pub const fn nonce(mut self, nonce: u64) -> Self {
        self.request.nonce = Some(nonce);
        self
    }

    pub fn data(mut self, data: &str) -> Self {
        self.request.data = Some(data.to_owned());
        self
    }

    pub fn build(self) -> TxRequest {
        self.request
    }
}
