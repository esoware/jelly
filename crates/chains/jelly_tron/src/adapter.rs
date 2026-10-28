use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline, EventSink,
    FeeQuote, KeyMaterial, PendingTracker, RetryPolicy, SignedTx, TxId, TxRequest, UnsignedTx,
    WatchedAddress, sole_key,
};
use parking_lot::RwLock;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use crate::broadcast::{self, BroadcastReply};
use crate::client::TronClient;
use crate::config::{CHAIN, TronConfig};
use crate::error::TronError;
use crate::gate::Gate;
use crate::node::TronNode;
use crate::params::ParamCache;
use crate::payload::TronPayload;
use crate::pending::Tracked;
use crate::send::{self, Inputs};
use crate::sign;
use crate::state::States;
use crate::sync::{self, PassInputs};
use crate::watched::{self, Watched};

const TARGET: &str = "jelly::tron::adapter";

/// TRX and the TRC-20 tokens in `token::TOKENS` over one rotating pool of TRON
/// HTTP endpoints.
pub struct TronAdapter {
    config: TronConfig,
    client: TronClient,
    params: ParamCache,
    states: States,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    /// Held for a whole pass, so `sync` is single-flight. Each address's `Gate`
    /// carries over from one pass to the next.
    pass: AsyncMutex<HashMap<i64, Gate>>,
}

impl fmt::Debug for TronAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TronAdapter")
            .field("watched", &self.watch.read().len())
            .field("in_flight", &self.states.pending_count())
            .finish_non_exhaustive()
    }
}

impl TronAdapter {
    pub fn new(
        node: Arc<dyn TronNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config: TronConfig::default(),
            client: TronClient::new(node),
            params: ParamCache::default(),
            states: States::new(cursors),
            clock,
            watch: RwLock::new(Arc::from([])),
            pass: AsyncMutex::new(HashMap::new()),
        }
    }

    #[cfg(feature = "test-util")]
    #[must_use]
    pub const fn config(mut self, config: TronConfig) -> Self {
        self.config = config;
        self
    }

    /// How many sends this adapter is still tracking.
    #[cfg(feature = "test-util")]
    pub fn in_flight(&self) -> usize {
        self.states.pending_count()
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    /// Shared by quotes and builds, so the user approves the same numbers the
    /// transaction is built with. Each call reads a fresh block reference.
    ///
    /// The governance parameters come from [`ParamCache`], which falls back to the
    /// compiled figures until a node answers.
    async fn resolve(&self, request: &TxRequest) -> Result<send::Plan, TronError> {
        let address_id = request.sole_source().ok_or(TronError::NotOneSource)?;
        self.states.ensure_readable()?;
        let watched = self.watch_set();
        let sender = watched
            .iter()
            .find(|entry| entry.address_id == address_id)
            .ok_or(TronError::NotWatched { address_id })?;
        let inputs = Inputs {
            client: &self.client,
            config: &self.config,
            params: self.params.get(&self.client, self.clock.as_ref()).await,
        };
        send::resolve(&inputs, request, sender).await
    }
}

#[async_trait]
impl ChainAdapter for TronAdapter {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        let _pass = self.pass.lock().await;
        let resolved = watched::resolve(addresses)?;
        let ids: Vec<i64> = resolved.iter().map(|entry| entry.address_id).collect();
        self.states.ensure_readable()?;
        let current = self.watch_set();
        let admitted: Vec<i64> = resolved
            .iter()
            .filter(|entry| {
                !current.iter().any(|known| {
                    known.address_id == entry.address_id && known.address == entry.address
                })
            })
            .map(|entry| entry.address_id)
            .collect();
        if !admitted.is_empty() {
            self.states.load(&admitted).await?;
        }
        self.states.retain(&ids);
        *self.watch.write() = Arc::from(resolved);
        Ok(())
    }

    /// Single-flight per instance, so a second call waits for the running pass.
    ///
    /// The tip is always read. Follow-up, and each address's balances and history,
    /// start only if the deadline hasn't passed, so the pass stops early with the
    /// progress it has. The whole pass also runs under a timeout
    /// [`Deadline::GRACE`] past the deadline, so a pass that overruns both is
    /// reported instead of waited on.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let mut gates = self.pass.lock().await;
        self.states.ensure_readable()?;
        let watched = self.watch_set();
        self.states.load(&watched.iter().map(|entry| entry.address_id).collect::<Vec<_>>()).await?;
        let cutoff = deadline.cutoff(self.clock.as_ref());
        let inputs = PassInputs {
            client: &self.client,
            config: &self.config,
            states: &self.states,
            clock: self.clock.as_ref(),
        };
        let running = sync::run(&inputs, &watched, &mut gates, deadline, events);
        let Ok(progress) = timeout(cutoff, running).await else {
            return Err(ChainError::endpoints_down(
                CHAIN,
                "a sync pass inside its deadline",
                Vec::new(),
            ));
        };
        Ok(progress?)
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let plan = self.resolve(request).await?;
        Ok(plan.quote(request.amount)?)
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let plan = self.resolve(request).await?;
        Ok(plan.unsigned()?)
    }

    /// Makes no network call. Tracking starts here, the last point where both the
    /// signature and the payload's facts are in hand.
    ///
    /// Nothing is persisted yet, since a build that's never broadcast isn't a
    /// transaction. Its in-memory entry ages out at its expiration.
    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let (facts, signed) = sign::sign(tx, sole_key(keys)?)?;
        self.states.ensure_readable()?;
        let address_id = self
            .watch_set()
            .iter()
            .find(|entry| entry.address == facts.owner)
            .map(|entry| entry.address_id)
            .ok_or(TronError::OwnerNotWatched)?;
        self.states.track(Tracked {
            txid: signed.txid.value().to_owned(),
            address_id,
            owner: facts.owner,
            recipient: facts.recipient,
            asset: facts.asset,
            amount: facts.amount,
            expiration_millis: facts.header.expiration_millis,
            raw: hex::encode(signed.raw.as_bytes()),
            is_sent: false,
        });
        Ok(signed)
    }

    /// One attempt against one endpoint, after the send is persisted.
    ///
    /// If the write fails, nothing is sent. A definite refusal stops tracking, and
    /// anything ambiguous keeps it, since the transaction may have reached the
    /// network.
    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        let chain = tx.chain();
        if chain != CHAIN {
            return Err(TronError::ForeignChain { chain }.into());
        }
        let txid = tx.txid.value();
        if self.states.mark_sent(txid).await?.is_none() {
            return Err(TronError::Untracked.into());
        }
        match broadcast::deliver(&self.client, tx.raw.as_bytes()).await? {
            BroadcastReply::Accepted { txid: echoed } => {
                // We computed the txid as `sha256(raw_data)`, so a different echo
                // means the endpoint is misreporting. Accepting is still safe,
                // since follow-up only asks about our own txid.
                if let Some(echoed) = echoed.filter(|value| !value.is_empty())
                    && echoed != txid
                {
                    tracing::warn!(
                        target: TARGET,
                        "the endpoint echoed a txid other than the one it was sent"
                    );
                }
                Ok(tx.txid.clone())
            }
            BroadcastReply::Refused(rejection) => {
                if matches!(rejection.retry, RetryPolicy::FinalEverywhere | RetryPolicy::DidNotLand)
                    && let Some(address_id) = self.states.forget(txid)
                {
                    self.states.persist_sends(address_id).await?;
                }
                Err(TronError::Rejected(rejection).into())
            }
        }
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for TronAdapter {
    /// TRON has no nonce or output to release, so this just stops tracking an
    /// unsent transaction before it expires.
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        self.states.forget_unsent(&TronPayload::decode(&tx.payload)?.txid_hex());
        Ok(())
    }
}
