//! Every read the adapter makes, and the one write, over the failover pool.
//! Nothing above this module knows an Electrum method name or wire shape.
//!
//! The pool tags every walk it logs or reports with its own chain, which the
//! adapter's build holds equal to the table's, so the operation names here do
//! not repeat it.

use std::collections::BTreeSet;
use std::sync::Arc;

use bitcoin::{Transaction, Txid};
use jelly_core::{EventSink, FeeRate};
use jelly_electrum::{
    ElectrumClient, EstimationMode, HistogramBucket, MempoolInfo, classify_broadcast,
};
use jelly_net::{NetError, Pool};

use crate::error::{UtxoError, classify};
use crate::fee_api::FeeApi;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::sync::{PassOutcome, WatchedScript, sync_pass};

/// The Electrum side walks the failover pool once per call. The fee service is
/// optional, and its absence is not a failure.
///
/// A read rotates on anything attributable to one server and stops only on a
/// refusal a second host would repeat, or on a method the negotiated protocol
/// lacks, which was never sent.
#[derive(Debug)]
pub(crate) struct UtxoClient {
    pool: Arc<Pool<ElectrumClient>>,
    fee_api: Option<Arc<dyn FeeApi>>,
}

impl UtxoClient {
    pub(crate) const fn new(
        pool: Arc<Pool<ElectrumClient>>,
        fee_api: Option<Arc<dyn FeeApi>>,
    ) -> Self {
        Self { pool, fee_api }
    }

    pub(crate) fn fee_api(&self) -> Option<&dyn FeeApi> {
        self.fee_api.as_deref()
    }

    pub(crate) async fn tip_height(&self) -> Result<u32, UtxoError> {
        self.call("tip", async |client| Ok(client.tip_height().await?)).await
    }

    /// The relay floors protocol 1.6 reports. A server that negotiated below 1.6
    /// refuses the method before anything is sent, and the walk stops there rather
    /// than penalising every server, so `bid` then takes the floor from the fee
    /// service.
    pub(crate) async fn mempool_info(&self) -> Result<MempoolInfo, UtxoError> {
        self.call("mempool info", async |client| Ok(client.mempool_info().await?)).await
    }

    /// The Electrum client drops the mode for servers below protocol 1.6.
    /// `None` when the server has no estimate for `target_blocks`.
    pub(crate) async fn estimate_fee(
        &self,
        target_blocks: u16,
        mode: EstimationMode,
    ) -> Result<Option<FeeRate>, UtxoError> {
        let reading = self
            .call("fee estimate", async |client| {
                Ok(client.estimate_fee(target_blocks, mode).await?)
            })
            .await?;
        Ok(reading.rate())
    }

    pub(crate) async fn fee_histogram(&self) -> Result<Vec<HistogramBucket>, UtxoError> {
        self.call("fee histogram", async |client| Ok(client.fee_histogram().await?)).await
    }

    /// The transaction `txid` and the outputs it spends.
    ///
    /// Both fetches happen in one walk, so the transaction and its funding come
    /// from the same server, and a replacement is priced from the transactions
    /// themselves rather than a claim.
    pub(crate) async fn original(&self, txid: Txid) -> Result<Original, UtxoError> {
        self.call("original transaction", async |client| {
            let mut fetched = client.txs(&[txid]).await?;
            let Some(served) = fetched.pop() else {
                return Err(UtxoError::from_server(
                    client.endpoint(),
                    UtxoError::UnknownTx { txid },
                ));
            };
            let tx = served.tx;
            let parents: BTreeSet<Txid> =
                tx.input.iter().map(|txin| txin.previous_output.txid).collect();
            let funding: Vec<Transaction> = client
                .txs(&parents.into_iter().collect::<Vec<Txid>>())
                .await?
                .into_iter()
                .map(|parent| parent.tx)
                .collect();
            Original::new(tx, &funding)
                .map_err(|error| UtxoError::from_server(client.endpoint(), error))
        })
        .await
    }

    /// One endpoint, one attempt.
    ///
    /// A transport failure leaves the outcome unknown, so the caller re-checks by
    /// transaction id before any re-send rather than replaying the bytes around a
    /// rotation.
    pub(crate) async fn broadcast(&self, tx: &Transaction) -> Result<Txid, UtxoError> {
        self.pool
            .broadcast_once("broadcast", classify_broadcast, async |client: Arc<ElectrumClient>| {
                client.broadcast(tx).await.map_err(NetError::from)
            })
            .await
            .map_err(UtxoError::from_net)
    }

    /// The pooled client the next read would use, connected and proved.
    ///
    /// The walk only acquires the connection. The subscription wait happens
    /// outside the pool, because waiting for the next block must not be bounded
    /// by a per-attempt budget or read as a slow endpoint.
    pub(crate) async fn watch_client(&self) -> Result<Arc<ElectrumClient>, UtxoError> {
        self.call("watch", async |client| Ok(client)).await
    }

    /// One Electrum sync pass against one server.
    pub(crate) async fn pass(
        &self,
        params: &UtxoParams,
        watched: &[WatchedScript<'_>],
        events: &dyn EventSink,
    ) -> Result<PassOutcome, UtxoError> {
        self.call("sync", async |client| {
            sync_pass(params, &client, watched, events)
                .await
                .map_err(|error| UtxoError::from_server(client.endpoint(), error))
        })
        .await
    }

    /// Walks the pool until one endpoint answers, carrying the typed failure
    /// through and handing it back intact.
    async fn call<T, F, Fut>(&self, operation: &'static str, attempt: F) -> Result<T, UtxoError>
    where
        T: Send,
        F: Fn(Arc<ElectrumClient>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, UtxoError>> + Send,
    {
        let mapped = |client: Arc<ElectrumClient>| {
            let running = attempt(client);
            async move { running.await.map_err(NetError::from) }
        };
        self.pool.call(operation, classify, mapped).await.map_err(UtxoError::from_net)
    }
}
