use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline,
    EventSink, FeeLevel, FeeQuote, FeeSelection, KeyMaterial, LevelFee, PendingTracker, SendAmount,
    SignedTx, TxId, TxRequest, UnsignedTx, WatchedAddress, sole_key,
};
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::client::{Submission, XrpClient};
use crate::config::CHAIN;
use crate::error::XrpError;
use crate::node::XrpNode;
use crate::payload::{Payment, SendKind};
use crate::preview;
use crate::send::{self, Inputs, Resolved};
use crate::sign;
use crate::state::States;
use crate::sync;
use crate::watched::{self, Watched};

/// Native XRP payments and account deletions over one rotating pool of rippled
/// JSON-RPC endpoints.
#[derive(Debug)]
pub struct XrpAdapter {
    client: XrpClient,
    states: States,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    /// Serializes watch set changes, passes, builds, broadcasts, and discards, so `sync`
    /// is single-flight.
    gate: Mutex<()>,
}

impl XrpAdapter {
    pub fn new(
        node: Arc<dyn XrpNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client: XrpClient::new(node),
            states: States::new(cursors),
            clock,
            watch: RwLock::new(Arc::from([])),
            gate: Mutex::new(()),
        }
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    async fn resolve(&self, request: &TxRequest) -> Result<Resolved, XrpError> {
        let watched = self.watch_set();
        let inputs = Inputs {
            client: &self.client,
            states: &self.states,
            clock: self.clock.as_ref(),
            watched: &watched,
        };
        send::resolve(&inputs, request).await
    }
}

#[async_trait]
impl ChainAdapter for XrpAdapter {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        let _guard = self.gate.lock().await;
        let resolved = watched::resolve(addresses)?;
        let address_ids: Vec<i64> = resolved.iter().map(|entry| entry.address_id).collect();
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
        self.states.load(&address_ids, &admitted).await?;
        *self.watch.write() = Arc::from(resolved);
        Ok(())
    }

    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let _guard = self.gate.lock().await;
        let watched = self.watch_set();
        let running =
            sync::run(&self.client, &self.states, &watched, self.clock.as_ref(), deadline, events);
        timeout(deadline.cutoff(self.clock.as_ref()), running)
            .await
            .map_err(|_| ChainError::endpoints_down(CHAIN, "XRP sync", Vec::new()))?
            .map_err(ChainError::from)
    }

    /// A deletion pays the owner reserve, so it names no custom fee to start from.
    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let Resolved { payment, normal_fee_drops, .. } = self.resolve(request).await?;
        let fee = Amount::from(payment.fee_drops);
        Ok(FeeQuote {
            fee,
            custom: matches!(payment.kind, SendKind::Payment)
                .then_some(FeeSelection::Total { fee }),
            max_amount: (request.amount == SendAmount::Max)
                .then(|| Amount::from(payment.amount_drops)),
            levels: normal_fee_drops
                .map(|drops| LevelFee { level: FeeLevel::Normal, fee: Amount::from(drops) })
                .into_iter()
                .collect(),
        })
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let _guard = self.gate.lock().await;
        let Resolved { source_id, payment, .. } = self.resolve(request).await?;
        let tx = UnsignedTx { preview: preview::of(&payment), payload: payment.encode()? };
        self.states.hold(source_id, payment);
        Ok(tx)
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let key = sole_key(keys)?;
        self.states.ensure_readable()?;
        let (payment, signed) = sign::sign(tx, key)?;
        self.states.attach_signed(&payment, &signed)?;
        Ok(signed)
    }

    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        let _guard = self.gate.lock().await;
        let blob = self.states.mark_sent(tx).await?;
        match self.client.submit(&blob).await? {
            Submission::Accepted => Ok(tx.txid.clone()),
            Submission::Unresolved => {
                Err(ChainError::rpc("XRP submission is unresolved; the payment remains tracked"))
            }
        }
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for XrpAdapter {
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let _guard = self.gate.lock().await;
        let payment = Payment::decode(&tx.payload)?;
        self.states.release_unsent(&payment);
        Ok(())
    }
}
