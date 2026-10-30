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
use crate::error::UtxoError;
use crate::fee_api::FeeApi;
use crate::params::UtxoParams;
use crate::utxo::Utxo;

const TARGET: &str = "jelly::utxo::adapter";

/// Resubscribes well within a poll interval without hammering a dead server.
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
    /// Held through a whole pass or watch-set change, so `sync` is single-flight.
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
    /// A quote prefers `fee_api`, when there is one, over the Electrum server's own
    /// estimates.
    ///
    /// Fails on a pool for another chain or one with no endpoints.
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

    /// Unreadable stored state blocks every later operation, since its pending
    /// bookkeeping can't be rebuilt from history.
    fn block_if_unreadable(&self, error: &UtxoError) {
        if matches!(error, UtxoError::UnreadableState) {
            self.is_blocked.store(true, Ordering::Release);
        }
    }

    /// Replaces the watch set with `addresses`. The stored cursors of newly watched
    /// addresses are read first, and one that doesn't decode refuses the whole set.
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

    /// The spendable coins of `sources` and the tip, read under one lock so a build
    /// can't mix one pass's coins with another pass's height.
    ///
    /// An address no pass has reported on this session takes its coins from its
    /// stored cursor, and the tip is left for the plan to fetch, so a send right after
    /// unlock isn't priced against an empty wallet.
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
        // Keeps a second build from spending these coins in a conflicting transaction.
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

    /// Single-flight: a second call waits for the running pass instead of starting
    /// another.
    ///
    /// The pass stops at `deadline`. A cut pass persists and publishes nothing more,
    /// and the in-memory state stays as the last full pass left it.
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

    /// Returns on any Electrum notification without trusting its content. A failed
    /// listener backs off and retries, and polling covers the gap.
    ///
    /// Only a failed subscription is logged here, since the pool logs its own failures.
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
    /// Nothing needs recording first. The build claimed the coins, and they stay
    /// claimed until a pass sees them spent.
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
    /// Releases the claim of a transaction that won't be broadcast. A discarded
    /// replacement hands its coins back to the transaction it would have replaced.
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
