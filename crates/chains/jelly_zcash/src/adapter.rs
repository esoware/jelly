use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline,
    EventSink, FeeLevel, FeeQuote, FeeSelection, KeyMaterial, LevelFee, PendingTracker, SendAmount,
    SignedTx, TxId, TxRequest, UnsignedTx, WatchedAddress,
};
use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::time::timeout;
use zcash_transparent::bundle::{OutPoint, TxOut};

use crate::client::ZcashClient;
use crate::config::CHAIN;
use crate::error::ZcashError;
use crate::node::ZcashNode;
use crate::payload::Payment;
use crate::preview;
use crate::send::{self, Inputs, Resolved};
use crate::sign;
use crate::state::States;
use crate::sync;
use crate::watched::{self, Watched};

/// Transparent payments from one or more key-hash addresses over one paced pool of
/// Blockbook endpoints.
#[derive(Debug)]
pub struct ZcashAdapter {
    client: ZcashClient,
    states: States,
    clock: Arc<dyn Clock>,
    watch: RwLock<Arc<[Watched]>>,
    /// Serializes watch set changes, passes, quotes, builds, broadcasts, and discards, so
    /// `sync` is single-flight.
    ///
    /// It also holds the outputs spent by history transactions not yet summarized, so a
    /// transaction with more inputs than one pass can fetch picks up where the last pass
    /// stopped. A complete pass empties it.
    gate: Mutex<BTreeMap<OutPoint, TxOut>>,
}

impl ZcashAdapter {
    pub fn new(
        node: Arc<dyn ZcashNode>,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client: ZcashClient::new(node),
            states: States::new(cursors),
            clock,
            watch: RwLock::new(Arc::from([])),
            gate: Mutex::default(),
        }
    }

    fn watch_set(&self) -> Arc<[Watched]> {
        Arc::clone(&self.watch.read())
    }

    async fn resolve(&self, request: &TxRequest) -> Result<Resolved, ZcashError> {
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
impl ChainAdapter for ZcashAdapter {
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

    /// The deadline covers the wait for the gate as well as the pass.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let running = async {
            let mut fetched_previous_outputs = self.gate.lock().await;
            let watched = self.watch_set();
            sync::run(
                &self.client,
                &self.states,
                &watched,
                &mut fetched_previous_outputs,
                self.clock.as_ref(),
                deadline,
                events,
            )
            .await
        };
        timeout(deadline.cutoff(self.clock.as_ref()), running)
            .await
            .map_err(|_| ChainError::endpoints_down(CHAIN, "Zcash sync", Vec::new()))?
            .map_err(ChainError::from)
    }

    /// A custom fee starts from what this payment pays, less any change too small to
    /// keep, so it reproduces the same transaction.
    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let _guard = self.gate.lock().await;
        let Resolved { payment, normal_fee_zat } = self.resolve(request).await?;
        let paid_zat = payment.fee_zat.saturating_sub(payment.dropped_change_zat);
        Ok(FeeQuote {
            fee: Amount::from(payment.fee_zat),
            custom: Some(FeeSelection::Total { fee: Amount::from(paid_zat) }),
            max_amount: (request.amount == SendAmount::Max)
                .then(|| Amount::from(payment.amount_zat)),
            levels: normal_fee_zat
                .map(|zat| LevelFee { level: FeeLevel::Normal, fee: Amount::from(zat) })
                .into_iter()
                .collect(),
        })
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let _guard = self.gate.lock().await;
        let Resolved { payment, .. } = self.resolve(request).await?;
        let tx = UnsignedTx { preview: preview::of(&payment), payload: payment.encode()? };
        self.states.hold(&payment);
        Ok(tx)
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        self.states.ensure_readable()?;
        let payment = Payment::decode(&tx.payload)?;
        let signed = sign::sign(&payment, &tx.preview, keys)?;
        self.states.attach_signed(&payment, &signed)?;
        Ok(signed)
    }

    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        let _guard = self.gate.lock().await;
        self.states.ensure_readable()?;
        if tx.chain() != CHAIN
            || !sign::decode(tx.raw.as_bytes())
                .is_ok_and(|decoded| decoded.txid().to_string() == tx.txid.value())
        {
            return Err(
                ZcashError::Internal("a signed Zcash transaction does not match its txid").into()
            );
        }
        let raw_hex = hex::encode(tx.raw.as_bytes());
        let payment = self.states.signed_payment(&raw_hex, tx.txid.value())?;
        let checked = async {
            sign::validate_signed(&payment, tx.raw.as_bytes()).map_err(|_| {
                ZcashError::Internal("a signed Zcash transaction does not match its payment")
            })?;
            let tip = self.client.fresh_tip(self.clock.as_ref()).await?;
            if tip.height >= payment.expiry_height || tip.branch != payment.branch() {
                return Err(ZcashError::Invalid("This Zcash payment has expired. Build it again."));
            }
            Ok(())
        };
        if let Err(error) = checked.await {
            // Nothing reached the network, and the engine never submits a refused payment
            // again, so holding its coins until expiry would only block the next payment.
            self.states.release_unsent(&payment);
            return Err(error.into());
        }
        self.states.mark_sent(&payment).await?;
        self.client.broadcast(&raw_hex, tx.txid.value()).await?;
        Ok(tx.txid.clone())
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }
}

#[async_trait]
impl PendingTracker for ZcashAdapter {
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        let _guard = self.gate.lock().await;
        let payment = Payment::decode(&tx.payload)?;
        self.states.release_unsent(&payment);
        Ok(())
    }
}
