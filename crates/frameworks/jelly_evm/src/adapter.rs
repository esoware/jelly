mod draft;
mod replacing;

use std::fmt;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes};
use async_trait::async_trait;
use jelly_core::{
    ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline, EventSink,
    FeeBump, FeeQuote, KeyMaterial, PendingTracker, SignedTx, TxCancel, TxId, TxRequest,
    UnsignedTx, WatchedAddress, sole_key,
};
use jelly_net::{NetError, Outcome, Pool, WsClient, WsSubscription};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{sleep, timeout};

use self::draft::Draft;
use crate::client::EvmClient;
use crate::config::EvmConfig;
use crate::error::EvmError;
use crate::history::HistoryApi;
use crate::ledger::{Envelope, Ledger, State, Tracked};
use crate::node::EvmNode;
use crate::params::EvmParams;
use crate::pending::{load_pending, store_pending};
use crate::sync::{self, Carried, PassInputs};
use crate::token::TokenTable;
use crate::watched::{self, Watched};
use crate::{send, sign};

/// Short enough to re-register a recovered endpoint well inside one poll
/// interval, long enough not to hammer a dead one.
const WATCH_RETRY: Duration = Duration::from_secs(5);

/// Every push failure rotates, because no endpoint's subscription failure says
/// anything about the next endpoint.
const fn classify_watch(_: &NetError) -> Outcome {
    Outcome::Rotate
}

/// One EIP-155 network over one rotating pool of JSON-RPC endpoints.
pub struct EvmAdapter {
    params: &'static EvmParams,
    config: EvmConfig,
    client: EvmClient,
    tokens: TokenTable,
    history: Option<Arc<dyn HistoryApi>>,
    pushes: Option<Arc<Pool<WsClient>>>,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    ledger: Mutex<Ledger>,
    /// Held for a whole pass, which makes `sync` single-flight. It carries what
    /// one pass learned about its services to the next.
    pass: AsyncMutex<Carried>,
    /// Held for a whole build, so the nonce one build picks is held in the
    /// ledger before another build can pick it.
    building: AsyncMutex<()>,
    /// Serialises pending-set writes, so a slower write of an older snapshot
    /// cannot overwrite a newer one.
    writing: AsyncMutex<()>,
    is_blocked: AtomicBool,
}

impl fmt::Debug for EvmAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmAdapter")
            .field("chain", &self.params.chain)
            .field("indexed_history", &self.history.is_some())
            .field("watched", &self.watch.read().len())
            .field("in_flight", &self.ledger.lock().len())
            .finish_non_exhaustive()
    }
}

impl EvmAdapter {
    pub fn builder(
        params: &'static EvmParams,
        node: Arc<dyn EvmNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> EvmAdapterBuilder {
        EvmAdapterBuilder {
            params,
            node,
            cursors,
            clock,
            config: EvmConfig::default(),
            history: None,
            pushes: None,
        }
    }

    /// Transactions holding a nonce slot, whether built but unsent or sent but not
    /// yet deep enough to forget. A replacement and its original count separately.
    #[cfg(feature = "test-util")]
    pub fn in_flight(&self) -> usize {
        self.ledger.lock().len()
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    fn ensure_readable(&self) -> Result<(), EvmError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(EvmError::UnreadableState);
        }
        Ok(())
    }

    /// Stored state that does not read back blocks every later operation, so
    /// nothing overwrites it.
    fn block_if_unreadable(&self, error: &EvmError) {
        if matches!(error, EvmError::UnreadableState) {
            self.is_blocked.store(true, Ordering::Release);
        }
    }

    /// Restores the stored sends of newly watched addresses, refusing the whole set
    /// when one of their scan cursors or sent sets does not read back.
    async fn restore(&self, watched: &[Watched]) -> Result<(), EvmError> {
        let _writing = self.writing.lock().await;
        self.ensure_readable()?;
        let current = self.watch_set();
        let missing: Vec<i64> = watched
            .iter()
            .filter(|entry| !current.contains(entry))
            .map(|entry| entry.address_id)
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let cursors = self.cursors.as_ref();
        sync::load_cursors(cursors, &missing)
            .await
            .inspect_err(|error| self.block_if_unreadable(error))?;
        let restored = load_pending(cursors, self.params.chain, watched, &missing)
            .await
            .inspect_err(|error| self.block_if_unreadable(error))?;
        self.ledger.lock().restore(restored);
        Ok(())
    }

    async fn persist_pending(&self, owner: Address) -> Result<(), EvmError> {
        self.ensure_readable()?;
        let watched = self.watch_set();
        store_pending(self.cursors.as_ref(), &self.ledger, &self.writing, &watched, owner).await
    }
}

fn txid_of(txid: &TxId) -> Result<B256, EvmError> {
    txid.value().parse::<B256>().map_err(|_| EvmError::NotATxid)
}

/// `build` is fallible so an unreadable token registry is reported up front
/// rather than on the first balance read.
#[derive(Debug)]
pub struct EvmAdapterBuilder {
    params: &'static EvmParams,
    node: Arc<dyn EvmNode>,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    config: EvmConfig,
    history: Option<Arc<dyn HistoryApi>>,
    pushes: Option<Arc<Pool<WsClient>>>,
}

impl EvmAdapterBuilder {
    #[cfg(feature = "test-util")]
    #[must_use]
    pub const fn config(mut self, config: EvmConfig) -> Self {
        self.config = config;
        self
    }

    /// Without a history service, sync walks `eth_getLogs` for token transfers
    /// and reads native activity off the balance.
    #[must_use]
    pub fn history(mut self, history: Arc<dyn HistoryApi>) -> Self {
        self.history = Some(history);
        self
    }

    /// Without a push pool the chain relies on its poll timer.
    #[must_use]
    pub fn pushes(mut self, pushes: Arc<Pool<WsClient>>) -> Self {
        self.pushes = Some(pushes);
        self
    }

    pub fn build(self) -> Result<EvmAdapter, ChainError> {
        let params = self.params;
        let tokens = TokenTable::for_params(params).map_err(ChainError::from)?;
        let carried = Carried::new(params, &self.config);
        Ok(EvmAdapter {
            params,
            config: self.config,
            client: EvmClient::new(self.node, params.chain),
            tokens,
            history: self.history,
            pushes: self.pushes,
            cursors: self.cursors,
            clock: self.clock,
            watch: RwLock::new(Arc::from([])),
            ledger: Mutex::new(Ledger::new()),
            pass: AsyncMutex::new(carried),
            building: AsyncMutex::new(()),
            writing: AsyncMutex::new(()),
            is_blocked: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl ChainAdapter for EvmAdapter {
    fn chain(&self) -> ChainId {
        self.params.chain
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        let _pass = self.pass.lock().await;
        let _building = self.building.lock().await;
        let resolved = watched::resolve(self.params.chain, addresses)?;
        self.restore(&resolved).await?;
        let senders: Vec<Address> = resolved.iter().map(|entry| entry.owner).collect();
        self.ledger.lock().retain_senders(&senders);
        *self.watch.write() = Arc::from(resolved);
        Ok(())
    }

    /// Single-flight per instance, so a second call waits for the running pass.
    ///
    /// The history steps, log windows and indexed reads, check the deadline and
    /// stop early with the progress they have. The tip, receipts and balances are
    /// always read. The whole pass also runs under a timeout [`Deadline::GRACE`]
    /// past the deadline, so a call that outlasts both is reported rather than
    /// waited on.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let mut carried = self.pass.lock().await;
        self.ensure_readable()?;
        let watched = self.watch_set();
        let cutoff = deadline.cutoff(self.clock.as_ref());
        let history = if carried.is_history_unserved { None } else { self.history.as_deref() };
        let inputs = PassInputs {
            params: self.params,
            config: &self.config,
            tokens: &self.tokens,
            client: &self.client,
            history,
            cursors: self.cursors.as_ref(),
            clock: self.clock.as_ref(),
            ledger: &self.ledger,
            writing: &self.writing,
            events,
            deadline,
        };
        let running = sync::run(&inputs, &watched, &mut carried);
        let Ok(progress) = timeout(cutoff, running).await else {
            return Err(ChainError::endpoints_down(
                self.params.chain,
                "a sync pass inside its deadline",
                Vec::new(),
            ));
        };
        Ok(progress.inspect_err(|error| self.block_if_unreadable(error))?)
    }

    /// Wakes sync on `newHeads` pushes without trusting their contents.
    ///
    /// Only the subscription goes through pool failover. The wait runs outside
    /// the pool and is floored by the chain's cadence. Without a push pool this
    /// never resolves.
    async fn wait_activity(&self) {
        let Some(pushes) = self.pushes.as_ref() else {
            pending::<()>().await;
            return;
        };
        let subscriptions = [WsSubscription::new("eth_subscribe", serde_json::json!(["newHeads"]))];
        loop {
            let registered = pushes
                .call("evm watch", classify_watch, |client| {
                    let subscriptions = subscriptions.clone();
                    async move {
                        client.register(&subscriptions).await?;
                        Ok(client)
                    }
                })
                .await;
            if let Ok(client) = registered
                && client.wait_notice(self.params.watch_floor).await.is_ok()
            {
                return;
            }
            sleep(WATCH_RETRY).await;
        }
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        Ok(self.resolve(&Draft::of(request)?).await?.quote()?)
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        Ok(self.prepare(&Draft::of(request)?).await?)
    }

    /// Makes no network call. This is the last point where the txid and the
    /// sender are both known, so the nonce slot learns its txid here.
    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let key = sole_key(keys)?;
        self.ensure_readable()?;
        let payload = send::payload_of(tx, self.params)?;
        if !self.watch_set().iter().any(|entry| entry.owner == payload.from()) {
            return Err(EvmError::UnwatchedSigner.into());
        }
        let signed = sign::sign(self.params, &payload, key)?;
        let txid = txid_of(&signed.txid)?;
        let held = Tracked::held(
            payload.intent(),
            payload.nonce(),
            payload.fee()?,
            self.clock.elapsed_millis(),
        );
        self.ledger.lock().record_signed(Tracked {
            calldata: payload.calldata(),
            state: State::Signed {
                envelope: Envelope { txid, raw: Bytes::copy_from_slice(signed.raw.as_bytes()) },
                is_sent: false,
            },
            ..held
        });
        Ok(signed)
    }

    /// One attempt against one endpoint.
    ///
    /// The watched set is persisted before the bytes go out, so a crash after
    /// sending still leaves a transaction this wallet knows to ask about, and
    /// bytes a pass can send again. When that write fails nothing is sent, and a
    /// first attempt gives its nonce slot back. A refusal of a first attempt that
    /// proves the transaction is in no mempool releases the slot, and anything
    /// ambiguous keeps it.
    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        self.ensure_readable()?;
        if tx.chain() != self.params.chain {
            return Err(EvmError::ForeignChain { chain: tx.chain() }.into());
        }
        let txid = txid_of(&tx.txid)?;
        let not_ours = || EvmError::NotOurs { txid: tx.txid.value().to_owned() };
        let (sender, was_sent) = {
            let mut ledger = self.ledger.lock();
            let entry = ledger.entry(txid).ok_or_else(not_ours)?;
            let (sender, was_sent) = (entry.from, entry.is_sent());
            ledger.mark_sent(txid);
            (sender, was_sent)
        };
        if let Err(error) = self.persist_pending(sender).await {
            // A retry of a send that already went out may sit in a mempool, so
            // only a first attempt lets go of its slot.
            if !was_sent {
                self.ledger.lock().release(txid);
            }
            return Err(error.into());
        }

        match send::deliver(&self.client, tx).await {
            Ok(id) => Ok(id),
            // A refusal of a retry says nothing about the attempt before it, which
            // may sit in a mempool or a block already. Geth answers a mined
            // transaction sent again with nonce too low.
            Err(EvmError::Rejected(rejection)) => {
                let released =
                    !was_sent && self.ledger.lock().release_refused(txid, rejection.retry);
                if released {
                    self.persist_pending(sender).await?;
                }
                Err(EvmError::Rejected(rejection).into())
            }
            Err(other) => Err(other.into()),
        }
    }

    fn fee_bump(&self) -> Option<&dyn FeeBump> {
        Some(self)
    }

    fn tx_cancel(&self) -> Option<&dyn TxCancel> {
        Some(self)
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for EvmAdapter {
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let payload = send::payload_of(tx, self.params)?;
        self.ledger.lock().release_held(payload.from(), payload.nonce());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<EvmAdapter>();
}
