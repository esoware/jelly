//! The chain adapter over `jelly_electrum` for transport and the engine for
//! transactions. Each chain enters as its [`UtxoParams`] table.

mod bump;
mod child_paying;
mod client;
mod fee;
mod replacing;
mod send;
mod state;
mod sync;
mod watched;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    AssetId, ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline,
    EventSink, FeeBump, FeeQuote, KeyMaterial, PendingTracker, SignedTx, TxId, TxRequest,
    UnsignedTx, WatchedAddress,
};
use jelly_electrum::ElectrumClient;
use jelly_net::{NetError, Pool};
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

use crate::adapter::client::UtxoClient;
use crate::adapter::fee::{FeeCache, FeeContext};
use crate::adapter::send::Plan;
use crate::adapter::state::ChainState;
use crate::adapter::watched::Watched;
use crate::claim::OutpointClaims;
use crate::coin::Utxo;
use crate::error::UtxoError;
use crate::fee_api::FeeApi;
use crate::params::UtxoParams;

const TARGET: &str = "jelly::utxo::adapter";

/// Short enough that a recovered endpoint is re-registered well inside one poll
/// interval, long enough that a dead one is not hammered.
const WATCH_RETRY: Duration = Duration::from_secs(5);

/// One UTXO chain over a rotating pool of Electrum servers.
pub struct UtxoAdapter {
    params: UtxoParams,
    client: UtxoClient,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    watched: RwLock<Arc<[Watched]>>,
    state: RwLock<ChainState>,
    claims: OutpointClaims,
    fee_cache: FeeCache,
    /// Held for a whole pass, which is what makes `sync` single-flight.
    pass: Mutex<()>,
    is_blocked: AtomicBool,
}

impl fmt::Debug for UtxoAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UtxoAdapter")
            .field("chain", &self.params.chain)
            .field("watched", &self.watched.read().len())
            .field("claimed", &self.claims.len())
            .field("tip", &self.state.read().tip())
            .finish_non_exhaustive()
    }
}

impl UtxoAdapter {
    /// `fee_api` is the fee service a quote prefers over the Electrum server's own
    /// estimator. Without one the quote falls back to what the server itself can say.
    ///
    /// A pool on the wrong chain or with no endpoints is a configuration error, so
    /// it fails here rather than at runtime.
    pub fn new(
        params: &UtxoParams,
        pool: Arc<Pool<ElectrumClient>>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
        fee_api: Option<Arc<dyn FeeApi>>,
    ) -> Result<Self, ChainError> {
        let chain = params.chain;
        let found = pool.chain();
        if found != chain {
            return Err(UtxoError::ForeignChain { expected: chain, found }.into_chain_error(chain));
        }
        if pool.endpoints().is_empty() {
            return Err(UtxoError::Net(NetError::NoEndpoints { chain }).into_chain_error(chain));
        }
        Ok(Self {
            params: *params,
            client: UtxoClient::new(pool, fee_api),
            cursors,
            clock,
            watched: RwLock::new(Arc::from([])),
            state: RwLock::new(ChainState::default()),
            claims: OutpointClaims::default(),
            fee_cache: FeeCache::default(),
            pass: Mutex::new(()),
            is_blocked: AtomicBool::new(false),
        })
    }

    /// The claims built-but-unbroadcast transactions hold on their coins.
    #[cfg(feature = "test-util")]
    pub(crate) const fn claims(&self) -> &OutpointClaims {
        &self.claims
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watched.read())
    }

    fn ensure_readable(&self) -> Result<(), UtxoError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(UtxoError::UnreadableState);
        }
        Ok(())
    }

    /// Stored state that reads back unreadable blocks every later operation, since
    /// its pending bookkeeping cannot be rebuilt from history.
    fn block_if_unreadable(&self, error: &UtxoError) {
        if matches!(error, UtxoError::UnreadableState) {
            self.is_blocked.store(true, Ordering::Release);
        }
    }

    /// Replaces the watch set with `addresses`, after reading the stored cursors of
    /// the ones it did not hold, so a set with one that does not read back is refused
    /// whole.
    async fn admit(&self, addresses: &[WatchedAddress]) -> Result<(), UtxoError> {
        self.ensure_readable()?;
        let resolved = watched::resolve(&self.params, addresses)?;
        let watched = self.watch_set();
        let admitted = resolved.iter().filter(|entry| !watched.contains(entry));
        sync::load_cursors(&self.params, self.cursors.as_ref(), admitted)
            .await
            .inspect_err(|error| self.block_if_unreadable(error))?;
        self.state.write().retain(&resolved);
        *self.watched.write() = Arc::from(resolved);
        Ok(())
    }

    /// The watched entries of `address_ids`, in their order, each once.
    fn sources(&self, address_ids: &[i64]) -> Result<Vec<Watched>, UtxoError> {
        self.ensure_readable()?;
        if address_ids.is_empty() {
            return Err(UtxoError::NoSource);
        }
        let watched = self.watch_set();
        let mut sources: Vec<Watched> = Vec::with_capacity(address_ids.len());
        for &address_id in address_ids {
            let entry = watched
                .iter()
                .find(|entry| entry.address_id == address_id)
                .ok_or(UtxoError::NotWatched { address_id })?;
            if !sources.contains(entry) {
                sources.push(entry.clone());
            }
        }
        Ok(sources)
    }

    fn check_asset(&self, asset: AssetId) -> Result<(), UtxoError> {
        if asset == AssetId::Native(self.params.chain) {
            Ok(())
        } else {
            Err(UtxoError::ForeignAsset { asset, chain: self.params.chain })
        }
    }

    /// The spendable sets of `sources`, pooled, and the tip, read under one guard
    /// so a build cannot price against one pass's coins and another's height.
    ///
    /// Until a pass this session has reported on an address, its coins come from
    /// its stored cursor and the tip is left for the plan to fetch, so a send
    /// right after unlock is not priced against an empty wallet. Their spentness is
    /// as stale as the pass that stored the cursor, which is the same claim a pass
    /// makes.
    async fn snapshot(&self, sources: &[Watched]) -> Result<(Vec<Utxo>, Option<u32>), UtxoError> {
        let (mut coins, unreported, tip) = {
            let state = self.state.read();
            let mut coins = Vec::new();
            let mut unreported = Vec::new();
            for source in sources {
                match state.spendable(source.address_id, &self.claims) {
                    Some(known) => coins.extend(known),
                    None => unreported.push(source),
                }
            }
            (coins, unreported, state.tip())
        };
        if unreported.is_empty() {
            return Ok((coins, tip));
        }
        let mut stored =
            sync::load_cursors(&self.params, self.cursors.as_ref(), unreported.iter().copied())
                .await?;
        for source in unreported {
            if let Some(cursor) = stored.remove(&source.address_id) {
                coins.extend(self.claims.spendable(&cursor.restore(&source.script)));
            }
        }
        Ok((coins, None))
    }

    async fn fee_context(&self) -> FeeContext {
        self.fee_cache.get(&self.params, &self.client, self.clock.as_ref()).await
    }

    async fn plan(&self, sources: Vec<Watched>, request: &TxRequest) -> Result<Plan, UtxoError> {
        let (coins, tip) = self.snapshot(&sources).await?;
        Plan::resolve(&self.params, &self.client, sources, coins, tip, request).await
    }

    async fn quote(&self, request: &TxRequest) -> Result<FeeQuote, UtxoError> {
        self.check_asset(request.asset)?;
        let sources = self.sources(&request.from_address_ids)?;
        let fee_context = self.fee_context().await;
        let rate = fee::rate_of(&request.fee, &fee_context)?;
        let plan = self.plan(sources, request).await?;
        fee::quote(&plan, &fee_context, rate)
    }

    async fn prepare(&self, request: &TxRequest) -> Result<UnsignedTx, UtxoError> {
        self.check_asset(request.asset)?;
        let sources = self.sources(&request.from_address_ids)?;
        let fee_context = self.fee_context().await;
        let rate = fee::rate_of(&request.fee, &fee_context)?;
        let plan = self.plan(sources, request).await?;
        let built = send::build(&fee_context, &plan, rate)?;
        // The claim keeps a second build from selecting one of these coins and
        // producing two transactions that cannot both confirm.
        self.claims.claim(built.payload.unsigned_txid(), &built.payload.outpoints())?;
        send::unsigned(&built, &fee_context, 0)
    }

    fn sign(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, UtxoError> {
        self.ensure_readable()?;
        send::sign(&self.params, tx, keys)
    }

    async fn deliver(&self, tx: &SignedTx) -> Result<TxId, UtxoError> {
        self.ensure_readable()?;
        send::deliver(&self.params, &self.client, tx).await
    }
}

#[async_trait]
impl ChainAdapter for UtxoAdapter {
    fn chain(&self) -> ChainId {
        self.params.chain
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        let _pass = self.pass.lock().await;
        self.admit(addresses).await.map_err(|error| error.into_chain_error(self.params.chain))
    }

    /// Single-flight per instance, so a second call waits for the running pass
    /// rather than starting another.
    ///
    /// The pass is bounded by the engine's `deadline`. Nothing is persisted or
    /// published after it runs out, and a cut pass keeps the previous state.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let _pass = self.pass.lock().await;
        self.ensure_readable().map_err(|error| error.into_chain_error(self.params.chain))?;
        let watched = self.watch_set();
        let pending = self.state.read().pending_by_address();
        let budget = deadline.remaining(self.clock.as_ref());
        let running = sync::run(
            &self.params,
            &self.client,
            self.cursors.as_ref(),
            &self.claims,
            &watched,
            &pending,
            events,
        );
        let Ok(passed) = timeout(budget, running).await else {
            return Err(ChainError::endpoints_down(
                self.params.chain,
                "a sync pass inside its deadline",
                Vec::new(),
            ));
        };
        let outcome = passed
            .inspect_err(|error| self.block_if_unreadable(error))
            .map_err(|error| error.into_chain_error(self.params.chain))?;
        *self.state.write() = outcome.state;
        Ok(outcome.progress)
    }

    /// Returns on an Electrum notification, whose content is never trusted.
    /// Listener failures back off here while polling remains the fallback.
    ///
    /// The pool already logs a walk that found no endpoint, so only a failed
    /// subscription is logged here.
    async fn wait_activity(&self) {
        loop {
            let scripts: Vec<bitcoin::ScriptBuf> =
                self.watch_set().iter().map(|entry| entry.script.script_buf().clone()).collect();
            if let Ok(client) = self.client.watch_client().await {
                match client.watch(&scripts).await {
                    Ok(()) => return,
                    Err(error) => tracing::debug!(
                        target: TARGET,
                        chain = self.params.chain.as_str(),
                        %error,
                        "the activity listener failed"
                    ),
                }
            }
            sleep(WATCH_RETRY).await;
        }
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        self.quote(request).await.map_err(|error| error.into_chain_error(self.params.chain))
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        self.prepare(request).await.map_err(|error| error.into_chain_error(self.params.chain))
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        self.sign(tx, keys).map_err(|error| error.into_chain_error(self.params.chain))
    }

    /// One attempt against one endpoint.
    ///
    /// There is nothing to record first, because the coins were claimed when the
    /// transaction was built and stay claimed until a pass sees them spent.
    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        self.deliver(tx).await.map_err(|error| error.into_chain_error(self.params.chain))
    }

    fn fee_bump(&self) -> Option<&dyn FeeBump> {
        self.params.bump_method.is_some().then_some::<&dyn FeeBump>(self)
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for UtxoAdapter {
    /// Releases the claim of a transaction that will not be broadcast. A broadcast
    /// one keeps its claim until a pass sees its coins spent, so a discarded
    /// replacement hands the coins it took over back to the transaction it would
    /// have replaced.
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let payload = send::payload_of(&self.params, tx)
            .map_err(|error| error.into_chain_error(self.params.chain))?;
        self.claims.release(payload.unsigned_txid());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<UtxoAdapter>();
}
