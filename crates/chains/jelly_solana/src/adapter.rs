use std::fmt;
use std::future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{
    ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline, EventSink,
    FeeQuote, KeyMaterial, PendingTracker, SignedTx, TxId, TxRequest, UnsignedTx, WatchedAddress,
    sole_key,
};
use jelly_net::{NetError, Outcome, Pool, WsClient, WsSubscription};
use parking_lot::RwLock;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{sleep, timeout};

use crate::broadcast::{self, Attempt, Delivery};
use crate::client::{self, SolanaClient};
use crate::config::{CHAIN, SolanaConfig};
use crate::error::SolanaError;
use crate::fee::UnitCache;
use crate::node::SolanaNode;
use crate::payload::SolanaPayload;
use crate::pending::Tracked;
use crate::rent::RentCache;
use crate::send::{self, Inputs, Plan};
use crate::sign;
use crate::state::States;
use crate::sync::{self, PassInputs};
use crate::watched::{self, Watched};

const TARGET: &str = "jelly::solana::adapter";

/// Short enough to re-register a recovered endpoint within one poll interval, long
/// enough not to hammer a dead one.
const WATCH_RETRY: Duration = Duration::from_secs(5);

/// The least time between push-driven wakes, so a burst of pushes becomes one pass.
const WATCH_FLOOR: Duration = Duration::from_secs(2);

/// One endpoint's subscription failure says nothing about the next.
const fn classify_watch(_: &NetError) -> Outcome {
    Outcome::Rotate
}

/// Solana mainnet-beta over one rotating pool of JSON-RPC endpoints.
pub struct SolanaAdapter {
    config: SolanaConfig,
    client: SolanaClient,
    states: States,
    pushes: Option<Arc<Pool<WsClient>>>,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    rent: RentCache,
    units: UnitCache,
    /// Makes `sync` and `set_watch_set` single-flight.
    pass: AsyncMutex<()>,
}

impl fmt::Debug for SolanaAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SolanaAdapter")
            .field("watched", &self.watch.read().len())
            .field("in_flight", &self.states.pending_count())
            .finish_non_exhaustive()
    }
}

impl SolanaAdapter {
    pub fn builder(
        node: Arc<dyn SolanaNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> SolanaAdapterBuilder {
        SolanaAdapterBuilder { node, cursors, clock, config: SolanaConfig::default(), pushes: None }
    }

    /// How many sends this adapter is still following.
    #[cfg(feature = "test-util")]
    pub fn in_flight(&self) -> usize {
        self.states.pending_count()
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    async fn resolve(&self, request: &TxRequest) -> Result<Plan, SolanaError> {
        let watched = self.watch_set();
        let inputs = Inputs {
            client: &self.client,
            config: &self.config,
            states: &self.states,
            watched: &watched,
            rent: &self.rent,
            units: &self.units,
        };
        send::resolve(&inputs, request).await
    }
}

#[derive(Debug)]
pub struct SolanaAdapterBuilder {
    node: Arc<dyn SolanaNode>,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    config: SolanaConfig,
    pushes: Option<Arc<Pool<WsClient>>>,
}

impl SolanaAdapterBuilder {
    #[cfg(feature = "test-util")]
    #[must_use]
    pub const fn config(mut self, config: SolanaConfig) -> Self {
        self.config = config;
        self
    }

    /// Without a push pool the chain relies on its poll timer.
    #[must_use]
    pub fn pushes(mut self, pushes: Arc<Pool<WsClient>>) -> Self {
        self.pushes = Some(pushes);
        self
    }

    pub fn build(self) -> SolanaAdapter {
        let client = SolanaClient::new(self.node, &self.config);
        SolanaAdapter {
            config: self.config,
            client,
            states: States::new(self.cursors),
            pushes: self.pushes,
            clock: self.clock,
            watch: RwLock::new(Arc::from([])),
            rent: RentCache::default(),
            units: UnitCache::default(),
            pass: AsyncMutex::new(()),
        }
    }
}

#[async_trait]
impl ChainAdapter for SolanaAdapter {
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

    /// A second call waits for the running pass.
    ///
    /// The tip and balances are always read. Follow-up and the history walk are skipped
    /// or cut short once the deadline passes, and a pass still running
    /// [`Deadline::GRACE`] past the deadline is abandoned and reported as a failure.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let _pass = self.pass.lock().await;
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
        let running = sync::run(&inputs, &watched, deadline, events);
        let Ok(progress) = timeout(cutoff, running).await else {
            return Err(ChainError::endpoints_down(
                CHAIN,
                "a sync pass inside its deadline",
                Vec::new(),
            ));
        };
        Ok(progress?)
    }

    /// Wakes sync on any push without reading what it says, and never resolves without
    /// a push pool.
    ///
    /// `accountSubscribe` on the owner and on each associated token account covers every
    /// lamport and token balance change. Public endpoints never answer `logsSubscribe`,
    /// so registering it would time out everywhere.
    async fn wait_activity(&self) {
        let Some(pushes) = self.pushes.as_ref() else {
            future::pending::<()>().await;
            return;
        };
        loop {
            let subscriptions: Vec<WsSubscription> = self
                .watch_set()
                .iter()
                .flat_map(Watched::accounts)
                .map(|(_, account)| client::account_subscription(&account))
                .collect();
            let registered = pushes
                .call("solana watch", classify_watch, |client| {
                    let subscriptions = subscriptions.clone();
                    async move {
                        client.register(&subscriptions).await?;
                        Ok(client)
                    }
                })
                .await;
            if let Ok(client) = registered
                && client.wait_notice(WATCH_FLOOR).await.is_ok()
            {
                return;
            }
            sleep(WATCH_RETRY).await;
        }
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let plan = self.resolve(request).await?;
        Ok(plan.quote()?)
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let plan = self.resolve(request).await?;
        Ok(send::build(&plan)?)
    }

    /// Makes no network call. Following starts here, the last point where the signature
    /// and the payload are both in hand.
    ///
    /// Nothing is persisted until broadcast, and an entry that's never broadcast ages
    /// out with its blockhash.
    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let key = sole_key(keys)?;
        let payload = SolanaPayload::decode(tx.payload.as_bytes())?;
        let signed = sign::sign(&payload, key)?;
        self.states.ensure_readable()?;
        let payer = payload.payer();
        let address_id = self
            .watch_set()
            .iter()
            .find(|entry| entry.owner == payer)
            .map(|entry| entry.address_id)
            .ok_or(SolanaError::PayerNotWatched)?;
        self.states.track(Tracked {
            signature: signed.txid.value().to_owned(),
            address_id,
            payer: payer.to_string(),
            asset: payload.asset(),
            amount: payload.amount(),
            recipient: payload.recipient().to_owned(),
            expiry_height: payload.expiry_height(),
            raw: BASE64.encode(signed.raw.as_bytes()),
            is_sent: false,
        });
        Ok(signed)
    }

    /// One attempt against one endpoint.
    ///
    /// The send is persisted before the bytes leave, so a crash mid-send still leaves a
    /// transaction to ask about, and a failed write stops the send. Sync passes leave it
    /// alone until this attempt answers. A refusal drops the send only if no other
    /// attempt could have forwarded the bytes.
    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        let chain = tx.chain();
        if chain != CHAIN {
            return Err(SolanaError::ForeignChain { chain }.into());
        }
        let signature = tx.txid.value();
        let first_attempt = self.states.first_attempt(signature);
        if self.states.mark_sent(signature).await?.is_none() {
            return Err(SolanaError::Untracked.into());
        }
        match broadcast::deliver(&self.client, tx.raw.as_bytes(), signature, Attempt::First).await {
            Ok(delivery) => {
                if delivery == Delivery::AlreadyLanded {
                    tracing::info!(
                        target: TARGET,
                        "the network already had this transaction, so the send is done"
                    );
                }
                Ok(tx.txid.clone())
            }
            Err(refused @ SolanaError::Rejected(_)) => {
                if let Some(address_id) = self.states.forget_refused(&first_attempt) {
                    self.states.persist_sends(address_id).await?;
                }
                Err(refused.into())
            }
            Err(other) => Err(other.into()),
        }
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for SolanaAdapter {
    /// Solana has no nonce or output to release, so this only stops following an unsent
    /// build. It's matched by message, because the caller has no signature.
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let payload = SolanaPayload::decode(tx.payload.as_bytes())?;
        self.states.forget_unsent(|held| held.is_signed_message(payload.message_bytes()));
        Ok(())
    }
}
