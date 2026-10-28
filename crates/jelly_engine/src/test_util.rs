//! Test-only access to persisted state, sessions, sync loops, and the event pump, plus
//! fixtures and fakes. Only `[dev-dependencies]` enable the `test-util` feature.
//!
//! Only the engine holds the wallet database, so a test that has to see where a cursor
//! landed, or plant a row the engine would never write, goes through these.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    AdapterBuilder, AdapterSet, BuildRequest, ChainAdapter, ChainId, Clock, ClockError, CoreError,
    CursorScope, CursorStore, DerivationPath, EventSink, HeldReservation, KeySource,
    ReservationSlot, ScanAdvance, ScanStream, ScriptKind, test_util::FakeClock,
};
use jelly_store::NewAddress;
use jelly_vault::Argon2Params;

use crate::pending::{self, PreparedTx};
use crate::{EngineConfig, WalletEngine, wallet};

/// The shipped configuration with [`Argon2Params::MINIMUM`] as the vault profile.
///
/// That's the lowest profile the vault accepts, so a fixture can't write a file the
/// shipped build would refuse, and a suite skips a 256 MiB derivation per wallet.
pub fn config() -> EngineConfig {
    EngineConfig { vault_kdf: Argon2Params::MINIMUM, ..EngineConfig::default() }
}

/// Where the engine keeps the vault of `wallet_id` under `data_dir`.
pub fn vault_path(data_dir: &Path, wallet_id: i64) -> PathBuf {
    wallet::vault_path(data_dir, wallet_id)
}

/// A builder that answers every request with an empty set.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAdapterBuilder;

#[async_trait]
impl AdapterBuilder for NullAdapterBuilder {
    async fn build(&self, _request: &BuildRequest) -> AdapterSet {
        AdapterSet::default()
    }
}

/// A fake clock whose wall-clock reads can be made to fail. The wallet database reads the
/// wall clock before every write, so a broken clock is the cheapest way to make a write
/// fail.
#[derive(Debug, Default)]
pub struct BreakableClock {
    clock: FakeClock,
    is_broken: AtomicBool,
}

impl BreakableClock {
    /// Moves both the wall clock and the monotonic reading forward.
    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    pub fn set_broken(&self, is_broken: bool) {
        self.is_broken.store(is_broken, Ordering::SeqCst);
    }
}

impl Clock for BreakableClock {
    fn now_millis(&self) -> Result<u64, ClockError> {
        if self.is_broken.load(Ordering::SeqCst) {
            return Err(ClockError::OutOfRange);
        }
        self.clock.now_millis()
    }

    fn elapsed_millis(&self) -> u64 {
        self.clock.elapsed_millis()
    }
}

/// The scan cursors the wallet database holds, without the routing that sends secret
/// streams to the vault.
pub async fn stored_scans(
    engine: &WalletEngine,
    scopes: &[CursorScope],
    streams: &[ScanStream],
) -> Result<Vec<ScanAdvance>, CoreError> {
    Ok(engine.inner.store.load_scans(scopes.to_vec(), streams.to_vec()).await?)
}

/// The reservations the wallet database holds, without the routing that sends secret
/// slots to the vault.
pub async fn stored_reservations(
    engine: &WalletEngine,
    scopes: &[CursorScope],
    slots: &[ReservationSlot],
) -> Result<Vec<HeldReservation>, CoreError> {
    Ok(engine.inner.store.load_reservations(scopes.to_vec(), slots.to_vec()).await?)
}

/// Stores a derived address row as given, without deriving it, the way a tampered
/// database would hold one. Returns the row's id.
pub async fn insert_underived_address(
    engine: &WalletEngine,
    wallet_id: i64,
    chain: ChainId,
    address: &str,
    path: DerivationPath,
    script_kind: ScriptKind,
) -> Result<i64, CoreError> {
    let new = NewAddress {
        wallet_id,
        chain,
        address: address.to_owned(),
        source: KeySource::Derived { path },
        script_kind: Some(script_kind),
        derivation_index: None,
        label: None,
    };
    Ok(engine.inner.store.insert_address(new).await?.id)
}

/// A sink into the event pump, for callers that stand in for an adapter.
pub fn event_sink(engine: &WalletEngine) -> Arc<dyn EventSink> {
    Arc::new(engine.inner.events.sink())
}

/// The cursor port the engine hands a build for `wallet_id`.
pub fn cursors(engine: &WalletEngine, wallet_id: i64) -> Arc<dyn CursorStore> {
    engine.cursors(wallet_id)
}

/// The published adapter for `chain`.
pub fn adapter(engine: &WalletEngine, chain: ChainId) -> Result<Arc<dyn ChainAdapter>, CoreError> {
    engine.adapter(chain)
}

/// Rebuilds and republishes the adapter set for the open session.
pub async fn rebuild_adapters(engine: &WalletEngine) -> Result<(), CoreError> {
    engine.rebuild_adapters().await
}

/// Locks the session if it has sat idle past its timeout, and reports whether it did.
pub async fn poll_auto_lock(engine: &WalletEngine) -> bool {
    engine.poll_auto_lock().await
}

/// How many passes `chain` has completed since the session opened.
pub fn sync_passes(engine: &WalletEngine, chain: ChainId) -> u64 {
    engine.inner.chains.slot(chain).passes.count()
}

/// Waits until `chain` has completed a pass beyond `after`, so a test need not sleep.
pub async fn wait_for_pass(engine: &WalletEngine, chain: ChainId, after: u64) {
    let slot = engine.inner.chains.slot(chain);
    slot.passes.wait_past(after).await;
}

/// What the last finished pass of `chain` failed with, or `None` when it succeeded.
pub fn last_sync_failure(engine: &WalletEngine, chain: ChainId) -> Option<String> {
    engine.inner.chains.slot(chain).passes.last_failure()
}

/// The height the status board holds `chain` as synced to.
pub fn synced_height(engine: &WalletEngine, chain: ChainId) -> Option<u64> {
    engine.inner.chains.status.heights(chain)?.synced_height
}

/// The tip the status board holds for `chain`.
pub fn tip_height(engine: &WalletEngine, chain: ChainId) -> Option<u64> {
    engine.inner.chains.status.heights(chain)?.tip_height
}

/// How many events producers dropped because the sink was full.
pub fn dropped_events(engine: &WalletEngine) -> u64 {
    engine.inner.events.dropped()
}

/// How long an authorization stays submittable, so a test can step the clock past it.
pub const PENDING_TTL: Duration = pending::PENDING_TTL;

/// Authorizations still awaiting a submit, stale ones already released.
pub async fn list_pending_txs(engine: &WalletEngine) -> Result<Vec<PreparedTx>, CoreError> {
    engine.list_pending_txs().await
}
