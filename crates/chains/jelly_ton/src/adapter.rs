use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    Address, Amount, ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore,
    Deadline, EventSink, FeeQuote, KeyMaterial, PendingTracker, ScriptProbe, SignedTx, TxId,
    TxRequest, UnsignedTx, WatchedAddress, sole_key,
};
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::address::ton_destination;
use crate::client::TonClient;
use crate::config::CHAIN;
use crate::error::TonError;
use crate::jetton::{JETTONS, master};
use crate::node::TonNode;
use crate::payload::Payment;
use crate::preview;
use crate::send::{self, Inputs};
use crate::sign;
use crate::state::States;
use crate::sync;
use crate::watched::{self, Watched};

/// Gram and the registry jettons over one rotating pool of TON HTTP JSON-RPC endpoints.
#[derive(Debug)]
pub struct TonAdapter {
    client: TonClient,
    states: States,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    /// Serializes watch set changes, passes, builds, broadcasts, and discards, so `sync`
    /// is single-flight.
    gate: Mutex<()>,
}

impl TonAdapter {
    pub fn new(
        node: Arc<dyn TonNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client: TonClient::new(node),
            states: States::new(cursors),
            clock,
            watch: RwLock::new(Arc::from([])),
            gate: Mutex::new(()),
        }
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    async fn resolve(&self, request: &TxRequest) -> Result<(i64, Payment), TonError> {
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
impl ChainAdapter for TonAdapter {
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
            .map_err(|_| ChainError::endpoints_down(CHAIN, "TON sync", Vec::new()))?
            .map_err(ChainError::from)
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let (_, payment) = self.resolve(request).await?;
        Ok(FeeQuote {
            fee: Amount::from(payment.cost_nanogram()?),
            custom: None,
            max_amount: payment.is_max.then(|| payment.amount()),
            levels: Vec::new(),
        })
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let _guard = self.gate.lock().await;
        let (source_id, payment) = self.resolve(request).await?;
        let tx = UnsignedTx { preview: preview::of(&payment)?, payload: payment.encode()? };
        self.states.hold(source_id, payment);
        Ok(tx)
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let key = sole_key(keys)?;
        self.states.ensure_readable()?;
        let payment = Payment::decode(&tx.payload)?;
        payment.ensure_unexpired(self.clock.as_ref())?;
        let signed = sign::sign(&payment, &tx.preview, key)?;
        self.states.attach_signed(&payment, &signed)?;
        Ok(signed)
    }

    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        let _guard = self.gate.lock().await;
        let boc = self.states.mark_sent(tx, self.clock.as_ref()).await?;
        self.client.broadcast(&boc).await?;
        Ok(tx.txid.clone())
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }

    fn script_probe(&self) -> Option<&dyn ScriptProbe> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for TonAdapter {
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let _guard = self.gate.lock().await;
        let payment = Payment::decode(&tx.payload)?;
        self.states.release_unsent(&payment);
        Ok(())
    }
}

/// Takes no part in sync's state, so it does not wait for a pass to finish.
///
/// Every call is paced, so an address stops being asked about at the first sign of use:
/// its own account first, then for each registry jetton the wallet the master names for
/// it and that wallet's account. A wallet that only ever received jettons has no Gram
/// history of its own, and its deployed jetton wallet is what shows it in use.
#[async_trait]
impl ScriptProbe for TonAdapter {
    async fn used(&self, addresses: &[Address]) -> Result<Vec<Address>, ChainError> {
        let mut used = Vec::new();
        for address in addresses {
            let invalid = TonError::Invalid("The TON address is not valid.");
            if address.chain() != CHAIN {
                return Err(invalid.into());
            }
            let owner = ton_destination(address.value()).map_err(|_| invalid)?.address;
            let mut is_used = self.client.is_used(&owner).await?;
            for jetton in JETTONS {
                if is_used {
                    break;
                }
                let wallet =
                    self.client.jetton_wallet(&master(jetton.token)?, &owner, None).await?;
                is_used = self.client.is_used(&wallet).await?;
            }
            if is_used {
                used.push(address.clone());
            }
        }
        Ok(used)
    }
}
