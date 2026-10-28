use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bitcoin::{ScriptBuf, Transaction, Txid, block};
use jelly_core::EndpointLabel;
use jelly_net::Identity;
use tokio::sync::Mutex as AsyncMutex;

use crate::chain::ElectrumChain;
use crate::connection::Connection;
use crate::connector::SocketSource;
use crate::error::ElectrumError;
use crate::fee::{EstimationMode, FeeReading, HistogramBucket, MempoolInfo};
use crate::history::HistoryEntry;
use crate::tx::ServedTx;
use crate::unspent::Unspent;
#[cfg(feature = "test-util")]
use crate::version::ProtocolVersion;

const TARGET: &str = "jelly::electrum::client";

/// One Electrum endpoint that reconnects after socket loss.
///
/// Every socket negotiates a protocol version and proves its chain before it serves a
/// request. The first socket waits for its first use to prove it, so the pool's identity
/// check can report another chain as a mismatch. Reads retry once on a socket that died
/// without answering. A broadcast never retries.
pub struct ElectrumClient {
    sockets: Arc<dyn SocketSource>,
    chain: ElectrumChain,
    connection: AsyncMutex<Option<Arc<Connection>>>,
    sockets_opened: AtomicU64,
}

impl fmt::Debug for ElectrumClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElectrumClient")
            .field("endpoint", self.endpoint())
            .field("sockets_opened", &self.sockets_opened.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ElectrumClient {
    /// Negotiates on the first socket, so a client that exists has reached its server.
    pub(crate) async fn open(
        sockets: Arc<dyn SocketSource>,
        chain: ElectrumChain,
    ) -> Result<Self, ElectrumError> {
        let connection = Connection::open(sockets.as_ref(), chain).await?;
        Ok(Self {
            sockets,
            chain,
            connection: AsyncMutex::new(Some(Arc::new(connection))),
            sockets_opened: AtomicU64::new(1),
        })
    }

    pub fn endpoint(&self) -> &EndpointLabel {
        self.sockets.endpoint()
    }

    /// What the cached socket negotiated, or `None` while no socket is cached.
    #[cfg(feature = "test-util")]
    pub async fn protocol(&self) -> Option<ProtocolVersion> {
        self.connection.lock().await.as_ref().map(|connection| connection.protocol())
    }

    /// The software string the server gave in the cached socket's handshake, or `None`
    /// while no socket is cached.
    #[cfg(feature = "test-util")]
    pub async fn software(&self) -> Option<String> {
        self.connection.lock().await.as_ref().map(|connection| connection.software().to_owned())
    }

    /// Opening a client doesn't prove its chain, and a wrong block here is an
    /// [`Identity::Mismatch`] rather than an error, so a probe reports a server on another
    /// chain as the wrong network instead of unreachable.
    pub async fn verify_identity(&self) -> Result<Identity, ElectrumError> {
        match self.connection().await {
            Ok(connection) => Ok(Identity::confirmed(format!(
                "{} on genesis {}",
                connection.software(),
                self.chain.genesis
            ))),
            Err(ElectrumError::WrongChain { expected, found, .. }) => {
                Ok(Identity::Mismatch { expected: expected.to_string(), found: found.to_string() })
            }
            Err(other) => Err(other),
        }
    }

    /// The tip height from `blockchain.headers.subscribe`. A malformed header fails the
    /// call.
    pub async fn tip_height(&self) -> Result<u32, ElectrumError> {
        self.read(async |connection| connection.tip_height().await).await
    }

    pub async fn block_headers(
        &self,
        heights: &[u32],
    ) -> Result<Vec<block::Header>, ElectrumError> {
        self.read(async |connection| connection.block_headers(heights).await).await
    }

    pub async fn histories(
        &self,
        scripts: &[ScriptBuf],
    ) -> Result<Vec<Vec<HistoryEntry>>, ElectrumError> {
        self.read(async |connection| connection.histories(scripts).await).await
    }

    pub async fn unspents(
        &self,
        scripts: &[ScriptBuf],
    ) -> Result<Vec<Vec<Unspent>>, ElectrumError> {
        self.read(async |connection| connection.unspents(scripts).await).await
    }

    pub async fn txs(&self, txids: &[Txid]) -> Result<Vec<ServedTx>, ElectrumError> {
        self.read(async |connection| connection.txs(txids).await).await
    }

    /// Sends the mode only to servers on protocol 1.6 or later.
    pub async fn estimate_fee(
        &self,
        target_blocks: u16,
        mode: EstimationMode,
    ) -> Result<FeeReading, ElectrumError> {
        self.read(async |connection| connection.estimate_fee(target_blocks, mode).await).await
    }

    pub async fn fee_histogram(&self) -> Result<Vec<HistogramBucket>, ElectrumError> {
        self.read(async |connection| connection.fee_histogram().await).await
    }

    pub async fn mempool_info(&self) -> Result<MempoolInfo, ElectrumError> {
        self.read(async |connection| connection.mempool_info().await).await
    }

    /// Registers tip and script subscriptions, then waits for a push. A fresh registration
    /// returns at once, so a sync covers the time spent disconnected.
    ///
    /// Socket errors go back to the caller for backoff. If the server refuses the script
    /// subscription, this falls back to tip notifications. Canceling the wait keeps the
    /// registrations and any unread pushes for the next call.
    pub async fn watch(&self, scripts: &[ScriptBuf]) -> Result<(), ElectrumError> {
        self.connection().await?.watch(scripts).await
    }

    /// Sends a signed transaction to this server at most once.
    ///
    /// A dead socket is replaced before the send, but nothing after it is retried. A
    /// transport failure leaves the outcome unknown, so the caller checks by txid before
    /// re-sending the same bytes.
    pub async fn broadcast(&self, tx: &Transaction) -> Result<Txid, ElectrumError> {
        self.connection().await?.broadcast(tx).await
    }

    /// Holds the lock across the dial and the proof, so concurrent callers share one
    /// reconnect.
    async fn connection(&self) -> Result<Arc<Connection>, ElectrumError> {
        let mut guard = self.connection.lock().await;
        if let Some(existing) = guard.as_ref()
            && existing.is_live()
        {
            let existing = Arc::clone(existing);
            if let Err(error) = existing.prove().await {
                *guard = None;
                return Err(error);
            }
            return Ok(existing);
        }
        *guard = None;
        let fresh = Arc::new(self.dial().await?);
        *guard = Some(Arc::clone(&fresh));
        Ok(fresh)
    }

    /// A fresh negotiated socket, proved before it is cached.
    async fn dial(&self) -> Result<Connection, ElectrumError> {
        let connection = Connection::open(self.sockets.as_ref(), self.chain).await?;
        self.sockets_opened.fetch_add(1, Ordering::Relaxed);

        connection.prove().await?;

        tracing::debug!(
            target: TARGET,
            endpoint = %self.endpoint(),
            protocol = %connection.protocol(),
            sockets = self.sockets_opened.load(Ordering::Relaxed),
            "electrum socket opened"
        );
        Ok(connection)
    }

    /// Runs an idempotent call, retrying once on a fresh socket if this one was dead.
    ///
    /// [`ElectrumError::Closed`] means nothing was answered, so the retry is safe. A
    /// timeout, server error, or malformed line is returned as is.
    async fn read<T, F, Fut>(&self, call: F) -> Result<T, ElectrumError>
    where
        F: Fn(Arc<Connection>) -> Fut + Send,
        Fut: Future<Output = Result<T, ElectrumError>> + Send,
    {
        let connection = self.connection().await?;
        let outcome = call(Arc::clone(&connection)).await;
        if matches!(outcome, Err(ElectrumError::Closed { .. })) {
            self.discard(&connection).await;
            let fresh = self.connection().await?;
            return call(fresh).await;
        }
        outcome
    }

    /// Drops `stale` from the cache, unless a reconnect has already replaced it.
    async fn discard(&self, stale: &Arc<Connection>) {
        let mut guard = self.connection.lock().await;
        if guard.as_ref().is_some_and(|held| Arc::ptr_eq(held, stale)) {
            *guard = None;
        }
    }
}

/// The pool shares each client across tasks, so losing `Send + Sync` should fail here,
/// not at a distant spawn site.
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<ElectrumClient>();
