use std::collections::{BTreeMap, HashMap};
#[cfg(any(test, feature = "test-util"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use jelly_core::{
    AdapterSet, BuildRequest, ChainAdapter, ChainId, CoreError, KeyService, SecretText,
};
use jelly_store::{ChainStatePatch, ChainStateRow, Patch};
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::WalletEngine;
use crate::session::Session;
use crate::settings::AppSettings;
use crate::status::StatusBoard;
use crate::sync::{self, ChainLoop};

/// Quiet period before an endpoint change triggers an adapter rebuild.
const REBUILD_DEBOUNCE: Duration = Duration::from_millis(500);

/// One rebuild's whole input, read once per rebuild from the chain-state rows and the
/// settings file, so the adapter set, the poll intervals, the seeded heights and the
/// settings all come from one moment.
///
/// A set is published only into the session it was planned for, never into a later one,
/// even of the same wallet, so the plan names that session.
#[derive(Debug)]
struct BuildPlan {
    request: BuildRequest,
    session: Weak<Session>,
    poll_secs: BTreeMap<ChainId, u32>,
    synced_heights: BTreeMap<ChainId, u64>,
}

/// One chain's supervision state.
///
/// `announced_tip` is the tip the last confirmation refresh counted from. The board's
/// tip cannot stand in for it, because in-flight progress moves the board before the
/// pass that found the block returns.
#[derive(Debug, Default)]
pub(crate) struct ChainSlot {
    kick: Notify,
    announced_tip: Mutex<Option<u64>>,
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) passes: PassLog,
}

impl ChainSlot {
    fn kick(&self) {
        self.kick.notify_one();
    }

    /// Sets the tip the first refresh counts from, unless a pass has already set one.
    fn seed_announced_tip(&self, tip: u64) {
        self.announced_tip.lock().get_or_insert(tip);
    }

    /// Records `tip` as announced and returns the one it replaces.
    pub(crate) fn announce_tip(&self, tip: u64) -> Option<u64> {
        self.announced_tip.lock().replace(tip)
    }

    pub(crate) async fn kicked(&self) {
        self.kick.notified().await;
    }
}

/// The passes a chain's loop has finished, so a test can wait for one rather than sleep.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Default)]
pub(crate) struct PassLog {
    count: AtomicU64,
    done: Notify,
    last_failure: Mutex<Option<String>>,
}

#[cfg(any(test, feature = "test-util"))]
impl PassLog {
    /// Counts a finished pass and keeps what it failed with, so a waiter that sees the
    /// pass also sees its failure.
    pub(crate) fn finished(&self, failure: Option<String>) {
        *self.last_failure.lock() = failure;
        self.count.fetch_add(1, Ordering::SeqCst);
        self.done.notify_waiters();
    }

    pub(crate) fn last_failure(&self) -> Option<String> {
        self.last_failure.lock().clone()
    }

    pub(crate) fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait_past(&self, after: u64) {
        loop {
            let waiter = self.done.notified();
            tokio::pin!(waiter);
            waiter.as_mut().enable();
            if self.count() > after {
                return;
            }
            waiter.await;
        }
    }
}

/// Everything the engine knows about chains, including the published adapter
/// generation, the token its supervision tasks run under, the rebuild queue, and the
/// status board.
#[derive(Debug, Default)]
pub(crate) struct Chains {
    adapters: ArcSwap<AdapterSet>,
    slots: Mutex<HashMap<ChainId, Arc<ChainSlot>>>,
    /// The token every running supervision task is under. A poll interval change replaces
    /// it and the tasks under it while the adapter generation stays.
    supervision: Mutex<Option<CancellationToken>>,
    /// One rebuild at a time.
    rebuild: AsyncMutex<()>,
    /// Replaces rather than queues, so many requests collapse to the latest.
    queued: Mutex<Option<BuildPlan>>,
    is_debounce_pending: AtomicBool,
    pub(crate) status: StatusBoard,
}

impl Chains {
    pub(crate) fn adapters(&self) -> Arc<AdapterSet> {
        self.adapters.load_full()
    }

    pub(crate) fn slot(&self, chain: ChainId) -> Arc<ChainSlot> {
        let mut guard = self.slots.lock();
        Arc::clone(guard.entry(chain).or_insert_with(|| Arc::new(ChainSlot::default())))
    }

    /// Drops the whole generation, so nothing syncs or reports until a rebuild publishes
    /// a new one.
    pub(crate) fn retire(&self) {
        let previous = self.supervision.lock().take();
        if let Some(previous) = previous {
            previous.cancel();
        }
        self.adapters.store(Arc::default());
        self.slots.lock().clear();
        self.queued.lock().take();
        self.status.clear();
    }
}

/// Every chain without a row that disables it, so a chain is on until the wallet
/// switches it off.
pub(crate) fn enabled_chains(states: &[ChainStateRow]) -> Vec<ChainId> {
    ChainId::ALL
        .into_iter()
        .filter(|chain| !states.iter().any(|row| row.chain == *chain && !row.enabled))
        .collect()
}

pub(crate) fn restore_heights(states: &[ChainStateRow]) -> BTreeMap<ChainId, u64> {
    states.iter().filter_map(|row| row.restore_height.map(|height| (row.chain, height))).collect()
}

fn api_keys(settings: &AppSettings) -> BTreeMap<KeyService, SecretText> {
    KeyService::ALL
        .into_iter()
        .filter_map(|service| {
            let key = match service {
                KeyService::Etherscan => settings.etherscan_api_key.as_ref(),
                KeyService::TronGrid => settings.trongrid_api_key.as_ref(),
            };
            key.map(|key| (service, key.clone()))
        })
        .collect()
}

/// The poll interval each chain's row overrides.
fn poll_overrides(states: &[ChainStateRow]) -> BTreeMap<ChainId, u32> {
    states.iter().filter_map(|row| row.poll_secs.map(|poll_secs| (row.chain, poll_secs))).collect()
}

impl WalletEngine {
    pub(crate) fn adapter(&self, chain: ChainId) -> Result<Arc<dyn ChainAdapter>, CoreError> {
        self.inner.chains.adapters().adapter(chain).cloned().ok_or(CoreError::NoAdapter(chain))
    }

    pub async fn list_enabled_chains(&self) -> Result<Vec<ChainId>, CoreError> {
        let session = self.require_session().await?;
        let states = self.inner.store.list_chain_states(session.wallet_id()).await?;
        Ok(enabled_chains(&states))
    }

    /// Switches a chain on or off. The enabled set is part of the build request,
    /// so a chain that goes away loses its adapter rather than merely losing its
    /// supervision task.
    pub async fn set_chain_enabled(
        &self,
        chain: ChainId,
        is_enabled: bool,
    ) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        let patch = ChainStatePatch { enabled: Some(is_enabled), ..ChainStatePatch::default() };
        self.inner.store.set_chain_state(session.wallet_id(), chain, patch).await?;
        self.rebuild_adapters().await?;
        Ok(())
    }

    /// `None` restores the shipped interval for the chain.
    pub async fn set_poll_interval(
        &self,
        chain: ChainId,
        poll_secs: Option<u32>,
    ) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        if poll_secs == Some(0) {
            return Err(CoreError::invalid_input("pollSecs", "Enter at least one second."));
        }
        let patch = ChainStatePatch {
            poll_secs: Patch::set_or_clear(poll_secs),
            ..ChainStatePatch::default()
        };
        self.inner.store.set_chain_state(session.wallet_id(), chain, patch).await?;
        let states = self.inner.store.list_chain_states(session.wallet_id()).await?;
        let current = self.inner.session.read();
        let session = current
            .as_ref()
            .filter(|current| Arc::ptr_eq(current, &session))
            .ok_or(CoreError::Locked)?;
        self.start_sync_tasks(&self.inner.chains.adapters(), &poll_overrides(&states), session);
        Ok(())
    }

    /// The birthday is part of the build request, so the adapters are rebuilt for the
    /// new one to take effect.
    pub async fn set_restore_height(
        &self,
        chain: ChainId,
        height: Option<u64>,
    ) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        let patch = ChainStatePatch {
            restore_height: Patch::set_or_clear(height),
            ..ChainStatePatch::default()
        };
        self.inner.store.set_chain_state(session.wallet_id(), chain, patch).await?;
        self.rebuild_adapters().await?;
        Ok(())
    }

    /// Wakes the sync loop of `chain`, or of every chain with an adapter.
    /// Never spawns a pass of its own, so passes cannot stack.
    pub async fn sync_now(&self, chain: Option<ChainId>) -> Result<Vec<ChainId>, CoreError> {
        self.require_session().await?;
        let set = self.inner.chains.adapters();
        let chains = match chain {
            Some(one) => {
                if set.adapter(one).is_none() {
                    return Err(CoreError::NoAdapter(one));
                }
                vec![one]
            }
            None => set.built(),
        };
        for &chain in &chains {
            self.inner.chains.slot(chain).kick();
        }
        Ok(chains)
    }

    /// Rebuilds and republishes the adapter set.
    ///
    /// Rebuilds run one at a time. A request that arrives while one runs replaces the
    /// pending request, so a burst of settings changes costs one rebuild with the newest
    /// input.
    ///
    /// A set built for a session that has since closed is dropped rather than published,
    /// because its adapters hold that session's keys.
    pub(crate) async fn rebuild_adapters(&self) -> Result<(), CoreError> {
        let plan = self.build_plan().await?;
        self.inner.chains.queued.lock().replace(plan);
        let _serialised = self.inner.chains.rebuild.lock().await;
        let queued = self.inner.chains.queued.lock().take();
        let Some(plan) = queued else {
            return Ok(());
        };

        // Building must see every reservation of a send that precedes publication.
        let session = plan.session.upgrade().ok_or(CoreError::Locked)?;
        let _sending = session
            .cancel
            .run_until_cancelled(self.inner.sends.lock())
            .await
            .ok_or(CoreError::Locked)?;
        let set = session
            .cancel
            .run_until_cancelled(self.inner.builder.build(&plan.request))
            .await
            .ok_or(CoreError::Locked)?;
        // Holding the slot prevents a concurrent lock from retiring the session between
        // the identity check and publication.
        let current = self.inner.session.read();
        let session = current
            .as_ref()
            .filter(|session| plan.session.ptr_eq(&Arc::downgrade(session)))
            .ok_or(CoreError::Locked)?;
        // The generation goes out before the states that describe it, so a subscriber
        // reacting to a state always finds the adapter.
        let set = Arc::new(set);
        self.inner.chains.adapters.store(Arc::clone(&set));
        self.seed_states(&plan, &set);
        self.start_sync_tasks(&set, &plan.poll_secs, session);
        Ok(())
    }

    /// Schedules a debounced rebuild and returns. Several endpoint edits in a row
    /// cost one rebuild.
    pub fn endpoints_changed(&self) {
        if self.inner.chains.is_debounce_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let cancel = self.inner.cancel.clone();
        self.inner.tasks.spawn(async move {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = sleep(REBUILD_DEBOUNCE) => {}
            }
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.chains.is_debounce_pending.store(false, Ordering::SeqCst);
            let engine = Self { inner };
            if engine.inner.session.current().is_none() {
                return;
            }
            if let Err(error) = engine.rebuild_adapters().await {
                tracing::warn!(
                    target: "jelly::engine::chains",
                    %error,
                    "could not rebuild the adapter set"
                );
            }
        });
    }

    /// Seeds each chain's stored height. A chain that was asked for and came back neither
    /// built nor skipped is a builder defect, reported rather than hidden.
    fn seed_states(&self, plan: &BuildPlan, set: &AdapterSet) {
        for chain in &plan.request.chains {
            if set.outcome(*chain).is_none() {
                tracing::warn!(
                    target: "jelly::engine::chains",
                    chain = chain.as_str(),
                    "the builder answered with neither an adapter nor a reason"
                );
            }
        }
        for (&chain, &height) in &plan.synced_heights {
            self.inner.chains.status.seed_height(chain, height);
            self.inner.chains.slot(chain).seed_announced_tip(height);
        }
    }

    async fn build_plan(&self) -> Result<BuildPlan, CoreError> {
        let session = self.inner.session.current().ok_or(CoreError::Locked)?;
        let states = self.inner.store.list_chain_states(session.wallet_id()).await?;
        let settings = self.inner.settings.get().await;
        Ok(BuildPlan {
            request: BuildRequest {
                chains: enabled_chains(&states),
                cursors: self.cursors(session.wallet_id()),
                api_keys: api_keys(&settings),
                proxy: settings.proxy,
                restore_heights: restore_heights(&states),
                account_keys: session.account_keys(),
            },
            session: Arc::downgrade(&session),
            poll_secs: poll_overrides(&states),
            synced_heights: states
                .iter()
                .filter_map(|row| row.synced_height.map(|height| (row.chain, height)))
                .collect(),
        })
    }

    /// Starts one supervision task per built chain of `set` under a fresh supervision
    /// token, cancelling the previous one.
    ///
    /// Cancelling a supervision token stops its loops, and a new one gets fresh ones.
    fn start_sync_tasks(
        &self,
        set: &AdapterSet,
        poll_secs: &BTreeMap<ChainId, u32>,
        session: &Session,
    ) {
        let supervision = session.cancel.child_token();
        let previous = self.inner.chains.supervision.lock().replace(supervision.clone());
        if let Some(previous) = previous {
            previous.cancel();
        }

        for (chain, adapter) in set.adapters() {
            let chain_loop = ChainLoop {
                chain,
                adapter: Arc::clone(adapter),
                slot: self.inner.chains.slot(chain),
                supervision: supervision.clone(),
                poll: sync::interval_for(
                    self.inner.table.spec(chain).poll_secs,
                    poll_secs.get(&chain).copied(),
                ),
            };
            session.tasks.spawn(chain_loop.run(Arc::downgrade(&self.inner)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(chain: ChainId, is_enabled: bool) -> ChainStateRow {
        ChainStateRow {
            wallet_id: 1,
            chain,
            enabled: is_enabled,
            poll_secs: None,
            restore_height: None,
            synced_height: None,
        }
    }

    #[test]
    fn a_chain_with_no_row_is_enabled() {
        assert_eq!(enabled_chains(&[]), ChainId::ALL.to_vec());
    }

    #[test]
    fn only_a_row_that_says_no_disables_a_chain() {
        let states = [row(ChainId::Monero, false), row(ChainId::Bitcoin, true)];
        let enabled = enabled_chains(&states);
        assert!(enabled.contains(&ChainId::Bitcoin));
        assert!(!enabled.contains(&ChainId::Monero));
    }

    #[tokio::test]
    async fn a_pass_log_counts_its_passes_and_a_wait_already_past_returns() {
        let passes = PassLog::default();
        assert_eq!(passes.count(), 0);
        passes.finished(Some(String::from("the endpoint refused")));
        assert_eq!(passes.last_failure().as_deref(), Some("the endpoint refused"));
        passes.finished(None);
        assert_eq!(passes.count(), 2);
        assert_eq!(passes.last_failure(), None, "a pass that succeeds clears the failure");
        // Already past, so this returns without a notification.
        passes.wait_past(1).await;
    }
}
