use std::collections::BTreeMap;
use std::fmt;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jelly_core::{ChainEntry, ChainId, ChainTable, Clock, CoreError, KeyMaterial, wire_enum};
use jelly_keypair::HdKeychain;
use jelly_store::WalletRow;
use jelly_vault::{UnlockedVault, VaultError, VaultPassword};
use parking_lot::{RwLock, RwLockReadGuard};
use secrecy::SecretString;
use serde::Serialize;
use tokio::sync::{Mutex, watch};
use tokio::time::sleep;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::WalletEngine;
use crate::error::blocking;
use crate::seed::SeedKeys;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Why the wallet is locked.
    pub enum LockReason {
        /// Nothing is open, or the user locked it.
        Requested => "requested",
        /// The session sat idle past its timeout.
        Idle => "idle",
    }
}

/// The lock state the interface shows.
///
/// Advisory, so anything irreversible re-reads the real state instead of trusting a
/// stale event.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockState {
    Locked {
        reason: LockReason,
    },
    #[serde(rename_all = "camelCase")]
    Unlocked {
        wallet_id: i64,
        wallet_name: String,
    },
}

impl LockState {
    pub const fn locked(reason: LockReason) -> Self {
        Self::Locked { reason }
    }

    pub(crate) fn unlocked(wallet: &WalletRow) -> Self {
        Self::Unlocked { wallet_id: wallet.id, wallet_name: wallet.name.clone() }
    }

    pub const fn is_unlocked(&self) -> bool {
        matches!(*self, Self::Unlocked { .. })
    }

    pub const fn wallet_id(&self) -> Option<i64> {
        match *self {
            Self::Unlocked { wallet_id, .. } => Some(wallet_id),
            Self::Locked { .. } => None,
        }
    }
}

/// Exists only while a wallet is unlocked. Nothing outside it holds the vault or keys
/// (except adapters' account keys, until lock), so dropping it drops every secret.
///
/// `last_activity_millis` advances only through [`WalletEngine::touch_session`], so
/// background work and the interface's refreshes after it can't keep the session open.
pub(crate) struct Session {
    wallet_id: i64,
    table: &'static ChainTable,
    /// Writes and Argon2id reauthentication run on the blocking pool, and reads of the
    /// decrypted payload run inline.
    ///
    /// An `RwLock` lets reads proceed during a reauthentication, which only needs shared
    /// access but can take hundreds of milliseconds. A write (a whole-file rewrite, or a
    /// password change's two Argon2id passes) holds it exclusively and blocks inline reads
    /// on their runtime worker.
    vault: Arc<RwLock<UnlockedVault>>,
    keys: SeedKeys,
    last_activity_millis: AtomicU64,
    pub(crate) tasks: TaskTracker,
    pub(crate) cancel: CancellationToken,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("wallet_id", &self.wallet_id).finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(
        wallet_id: i64,
        vault: UnlockedVault,
        keys: SeedKeys,
        clock: &dyn Clock,
        table: &'static ChainTable,
    ) -> Self {
        Self {
            wallet_id,
            table,
            vault: Arc::new(RwLock::new(vault)),
            keys,
            last_activity_millis: AtomicU64::new(clock.elapsed_millis()),
            tasks: TaskTracker::new(),
            cancel: CancellationToken::new(),
        }
    }

    pub(crate) const fn wallet_id(&self) -> i64 {
        self.wallet_id
    }

    pub(crate) fn touch(&self, clock: &dyn Clock) {
        self.last_activity_millis.store(clock.elapsed_millis(), Ordering::SeqCst);
    }

    pub(crate) fn idle(&self, clock: &dyn Clock) -> Duration {
        let last = self.last_activity_millis.load(Ordering::SeqCst);
        Duration::from_millis(clock.elapsed_millis().saturating_sub(last))
    }

    pub(crate) fn entry(&self, chain: ChainId) -> &'static dyn ChainEntry {
        self.table.entry(chain)
    }

    pub(crate) const fn keychain(&self) -> &HdKeychain {
        &self.keys.keychain
    }

    /// The account index the keys were opened for, which the wallet's subaddress chains
    /// derive under.
    pub(crate) const fn account(&self) -> u32 {
        self.keys.account
    }

    /// The account key of a chain whose spec needs one.
    pub(crate) fn account_key(&self, chain: ChainId) -> Result<&KeyMaterial, CoreError> {
        self.keys
            .account_keys
            .get(&chain)
            .map(Arc::as_ref)
            .ok_or(CoreError::Unsupported("this chain has no account key"))
    }

    /// Every account key the session holds, for the adapter build.
    pub(crate) fn account_keys(&self) -> BTreeMap<ChainId, Arc<KeyMaterial>> {
        self.keys.account_keys.clone()
    }

    /// No crypto or I/O, so it runs inline, but it blocks while a vault write holds the
    /// lock.
    pub(crate) fn vault_read<T>(
        &self,
        access: impl FnOnce(&UnlockedVault) -> Result<T, VaultError>,
    ) -> Result<T, CoreError> {
        access(&self.vault.read()).map_err(CoreError::from)
    }

    /// Every write rewrites the whole encrypted file, so it runs on the blocking pool.
    pub(crate) async fn vault_write<T, F>(&self, access: F) -> Result<T, CoreError>
    where
        F: FnOnce(&mut UnlockedVault) -> Result<T, VaultError> + Send + 'static,
        T: Send + 'static,
    {
        let vault = Arc::clone(&self.vault);
        blocking(move || access(&mut vault.write())).await
    }

    /// A read that re-derives the key from a password. That's an Argon2id pass, so it
    /// runs on the blocking pool.
    pub(crate) async fn vault_reauth<T, F>(&self, access: F) -> Result<T, CoreError>
    where
        F: FnOnce(&UnlockedVault) -> Result<T, VaultError> + Send + 'static,
        T: Send + 'static,
    {
        let vault = Arc::clone(&self.vault);
        blocking(move || access(&vault.read())).await
    }

    /// The check happens inside `jelly_vault`, which re-derives the key from the
    /// password, so a caller can't skip it.
    pub(crate) async fn verify_password(&self, password: SecretString) -> Result<(), CoreError> {
        let password = VaultPassword::new(password);
        self.vault_reauth(move |vault| vault.verify_password(&password)).await
    }
}

/// Holds the open session, plus channels for the advisory lock state and the idle
/// timeout the watcher follows.
#[derive(Debug)]
pub(crate) struct SessionSlot {
    current: RwLock<Option<Arc<Session>>>,
    /// Held through retirement and through creating, unlocking, editing, or deleting a
    /// wallet. Adapter publication relies on the slot's read guard instead (see
    /// `rebuild_adapters`).
    pub(crate) transition: Mutex<()>,
    state: watch::Sender<LockState>,
    timeout: watch::Sender<Duration>,
}

impl SessionSlot {
    pub(crate) fn new(timeout: Duration) -> Self {
        let (state, _) = watch::channel(LockState::locked(LockReason::Requested));
        let (timeout, _) = watch::channel(timeout);
        Self { current: RwLock::new(None), transition: Mutex::new(()), state, timeout }
    }

    /// The open session, skipping the expiry check. Background work leaves that to the
    /// idle watcher.
    pub(crate) fn current(&self) -> Option<Arc<Session>> {
        self.read().as_ref().map(Arc::clone)
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Option<Arc<Session>>> {
        self.current.read()
    }

    fn install(&self, session: Arc<Session>) {
        *self.current.write() = Some(session);
    }

    fn take(&self) -> Option<Arc<Session>> {
        self.current.write().take()
    }

    pub(crate) fn state(&self) -> LockState {
        self.state.borrow().clone()
    }

    /// Sends nothing when the state is unchanged, because subscribers treat every
    /// notification as a lock or unlock.
    pub(crate) fn publish(&self, state: LockState) {
        self.state.send_if_modified(|current| {
            if *current == state {
                return false;
            }
            *current = state;
            true
        });
    }

    fn subscribe(&self) -> watch::Receiver<LockState> {
        self.state.subscribe()
    }

    fn timeout(&self) -> Duration {
        *self.timeout.borrow()
    }

    pub(crate) fn set_timeout(&self, timeout: Duration) {
        self.timeout.send_replace(timeout);
    }

    fn subscribe_timeout(&self) -> watch::Receiver<Duration> {
        self.timeout.subscribe()
    }
}

/// What the idle watcher should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Idle {
    NoSession,
    /// Auto-lock is off, so wait for the timeout to change.
    Disabled,
    /// The session has this long left if nothing touches it.
    Wait(Duration),
    Expired,
}

impl WalletEngine {
    /// Lock and unlock transitions, including auto-lock.
    pub fn lock_state(&self) -> watch::Receiver<LockState> {
        self.inner.session.subscribe()
    }

    pub fn lock_state_now(&self) -> LockState {
        self.inner.session.state()
    }

    /// The only thing that resets the auto-lock countdown. The interface re-reads after
    /// every background event, so other operations don't count as user activity, and the
    /// command layer calls this on user input instead.
    pub async fn touch_session(&self) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        session.touch(self.inner.clock.as_ref());
        Ok(())
    }

    /// Cancels the session's tasks, drops the adapter set, and wipes every key it held.
    /// Idempotent.
    pub async fn lock_wallet(&self) {
        let _transition = self.inner.session.transition.lock().await;
        self.lock_inner(LockReason::Requested).await;
    }

    /// Locks the session if it's been idle past the timeout, and returns whether it did.
    ///
    /// The idle watcher and every session-scoped operation go through this, so an expired
    /// session is never usable even if the watcher is late.
    pub(crate) async fn poll_auto_lock(&self) -> bool {
        if !matches!(self.idle_check(), Idle::Expired) {
            return false;
        }
        let _transition = self.inner.session.transition.lock().await;
        if !matches!(self.idle_check(), Idle::Expired) {
            return false;
        }
        self.lock_inner(LockReason::Idle).await;
        true
    }

    /// The open session, refusing an expired one. Doesn't count as activity.
    pub(crate) async fn require_session(&self) -> Result<Arc<Session>, CoreError> {
        if self.poll_auto_lock().await {
            return Err(CoreError::Locked);
        }
        self.inner
            .session
            .current()
            .filter(|session| !session.cancel.is_cancelled())
            .ok_or(CoreError::Locked)
    }

    pub(crate) fn install_session(&self, session: Session, wallet: &WalletRow) {
        let cancel = session.cancel.clone();
        self.inner.session.install(Arc::new(session));
        self.inner.session.publish(LockState::unlocked(wallet));
        self.spawn_idle_watch(cancel);
        tracing::info!(target: "jelly::engine::session", wallet_id = wallet.id, "wallet unlocked");
    }

    /// The caller holds the transition lock. The retiring task keeps the send guard even
    /// if this future is dropped, so no later session can install before the old tasks
    /// drain.
    pub(crate) async fn lock_inner(&self, reason: LockReason) {
        if let Some(session) = self.inner.session.current() {
            session.cancel.cancel();
        }
        let sending = Arc::clone(&self.inner.sends).lock_owned().await;
        let Some(session) = self.inner.session.take() else {
            return;
        };
        self.inner.session.publish(LockState::locked(reason));
        session.tasks.close();
        let inner = Arc::clone(&self.inner);
        let retiring = self.inner.tasks.spawn(async move {
            let _sending = sending;
            session.tasks.wait().await;
            inner.chains.retire();
            inner.pending.clear();
            let wallet_id = session.wallet_id();
            drop(session);
            tracing::info!(
                target: "jelly::engine::session",
                wallet_id,
                reason = reason.as_str(),
                "wallet locked"
            );
        });
        if let Err(error) = retiring.await {
            tracing::error!(target: "jelly::engine::session", %error, "session retirement failed");
        }
    }

    fn idle_check(&self) -> Idle {
        let timeout = self.inner.session.timeout();
        let Some(session) = self.inner.session.current() else {
            return Idle::NoSession;
        };
        if timeout.is_zero() {
            return Idle::Disabled;
        }
        let left = timeout.saturating_sub(session.idle(self.inner.clock.as_ref()));
        if left.is_zero() { Idle::Expired } else { Idle::Wait(left) }
    }

    /// Runs on the engine's tracker, not the session's, because it may trigger
    /// `lock_inner`, which waits for the session's tracker to drain. It holds only a weak
    /// handle, so it never keeps the engine alive.
    fn spawn_idle_watch(&self, cancel: CancellationToken) {
        let weak = Arc::downgrade(&self.inner);
        let mut retuned = self.inner.session.subscribe_timeout();
        self.inner.tasks.spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let engine = Self { inner };
                let left = match engine.idle_check() {
                    Idle::NoSession => return,
                    Idle::Expired => {
                        if engine.poll_auto_lock().await {
                            return;
                        }
                        continue;
                    }
                    Idle::Disabled => None,
                    Idle::Wait(left) => Some(left),
                };
                drop(engine);
                let expiry = async {
                    match left {
                        Some(left) => sleep(left).await,
                        None => pending().await,
                    }
                };
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    changed = retuned.changed() => if changed.is_err() { return },
                    () = expiry => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::fixtures::{harness, seed_wallet};

    #[tokio::test]
    async fn debug_of_a_session_names_only_its_wallet() {
        let harness = harness().await;
        let wallet = seed_wallet(&harness.engine, "printed").await;
        let session = harness.engine.inner.session.current().expect("the new wallet is open");

        let rendered = format!("{session:?}");
        assert_eq!(rendered, format!("Session {{ wallet_id: {}, .. }}", wallet.id));
    }
}
