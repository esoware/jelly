//! The port an adapter talks to, and the shipped node behind it.
//!
//! The port is typed in `monero-wallet`'s own vocabulary, so there is no untyped
//! call layer to decode.

use std::fmt;
use std::ops::RangeInclusive;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_net::{NetError, Outcome, Pool};
use monero_interface::ScannableBlock;
use monero_wallet::ed25519::Point;
use monero_wallet::transaction::Transaction;

use crate::broadcast::BroadcastReply;
use crate::daemon::DaemonEndpoint;
use crate::decoys::Distribution;
use crate::error::MoneroError;
use crate::fee::FeeLadder;
use crate::params::{CHAIN, check_chain};

/// An object-safe node interface, so adapter tests can run offline. The
/// library's `Provides*` traits return `impl Trait` and cannot be trait objects.
///
/// Ring selection stays on one endpoint, because combining nodes' ring answers
/// could reveal the real input. A transaction's bytes and every question about it go
/// to that endpoint too, and to another only once it cannot be reached.
#[async_trait]
pub trait MoneroNode: Send + Sync + fmt::Debug {
    async fn tip_height(&self) -> Result<u64, MoneroError>;

    /// A contiguous range of blocks, validated to build on each other and to
    /// carry the transactions their headers name.
    async fn blocks(&self, range: RangeInclusive<u64>) -> Result<Vec<ScannableBlock>, MoneroError>;

    async fn fee_ladder(&self) -> Result<FeeLadder, MoneroError>;

    /// The cumulative `RingCT` output distribution up to and including `height`.
    async fn distribution(&self, height: u64) -> Result<Distribution, MoneroError>;

    /// Ring members by their index on the blockchain, from the endpoint named,
    /// so one build's whole ring comes from one node.
    async fn ring_members(
        &self,
        endpoint: &str,
        indexes: &[u64],
    ) -> Result<Vec<Option<[Point; 2]>>, MoneroError>;

    /// Broadcasts through `endpoint` when one is named, and through one endpoint of
    /// the pool for `None`.
    ///
    /// Exactly one attempt, because a transport failure leaves the outcome
    /// unknown, and unknown is not failed. The caller re-checks by txid and
    /// resends the identical bytes.
    async fn broadcast(
        &self,
        endpoint: Option<&str>,
        transaction: &Transaction,
    ) -> Result<BroadcastReply, MoneroError>;

    /// Whether `endpoint`, or for `None` whichever node of the pool answers, holds
    /// this transaction, in a block or in its pool.
    ///
    /// This is the only way to tell a send still in flight from one every mempool
    /// has forgotten, because a pooled transaction is in no block and cannot be
    /// found by scanning.
    async fn is_known(&self, endpoint: Option<&str>, txid: [u8; 32]) -> Result<bool, MoneroError>;
}

/// The shipping node, monerod over the shared client and failover pool. A named
/// endpoint is reached through the pool's own connection to it.
pub(crate) struct PooledNode {
    pool: Arc<Pool<DaemonEndpoint>>,
}

impl fmt::Debug for PooledNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PooledNode").field("endpoints", &self.pool.endpoints().len()).finish()
    }
}

impl PooledNode {
    /// A pool on another chain or with no endpoints is a configuration mistake, so
    /// it fails here rather than at runtime.
    pub(crate) fn new(pool: Arc<Pool<DaemonEndpoint>>) -> Result<Self, MoneroError> {
        check_chain(pool.chain())?;
        if pool.endpoints().is_empty() {
            return Err(MoneroError::Net(NetError::NoEndpoints { chain: CHAIN }));
        }
        Ok(Self { pool })
    }
}

#[async_trait]
impl MoneroNode for PooledNode {
    async fn tip_height(&self) -> Result<u64, MoneroError> {
        let height = self
            .pool
            .call("monero.tip_height", classify, async |node: Arc<DaemonEndpoint>| {
                node.latest_block_number().await
            })
            .await?;
        u64::try_from(height)
            .map_err(|_| MoneroError::Internal { context: "a block height does not fit in u64" })
    }

    async fn blocks(&self, range: RangeInclusive<u64>) -> Result<Vec<ScannableBlock>, MoneroError> {
        let from = height_index(*range.start())?;
        let to = height_index(*range.end())?;
        Ok(self
            .pool
            .call("monero.blocks", classify, async |node: Arc<DaemonEndpoint>| {
                node.scannable_blocks(from..=to).await
            })
            .await?)
    }

    async fn fee_ladder(&self) -> Result<FeeLadder, MoneroError> {
        Ok(self
            .pool
            .call("monero.fee_ladder", classify, async |node: Arc<DaemonEndpoint>| {
                node.fee_ladder().await
            })
            .await?)
    }

    async fn distribution(&self, height: u64) -> Result<Distribution, MoneroError> {
        let index = height_index(height)?;
        Ok(self
            .pool
            .call("monero.distribution", classify, async |node: Arc<DaemonEndpoint>| {
                let cumulative_outputs = node.output_distribution(index).await?;
                Ok(Distribution::new(cumulative_outputs, height, node.url()))
            })
            .await?)
    }

    async fn ring_members(
        &self,
        endpoint: &str,
        indexes: &[u64],
    ) -> Result<Vec<Option<[Point; 2]>>, MoneroError> {
        Ok(self
            .pool
            .call_at(
                endpoint,
                "monero.ring_members",
                classify,
                async |node: Arc<DaemonEndpoint>| node.ring_members(indexes).await,
            )
            .await?)
    }

    async fn broadcast(
        &self,
        endpoint: Option<&str>,
        transaction: &Transaction,
    ) -> Result<BroadcastReply, MoneroError> {
        let send = async |node: Arc<DaemonEndpoint>| broadcast_through(&node, transaction).await;
        answered(match endpoint {
            Some(url) => self.pool.call_at(url, "monero.broadcast", classify_broadcast, send).await,
            None => self.pool.broadcast_once("monero.broadcast", classify_broadcast, send).await,
        })
    }

    async fn is_known(&self, endpoint: Option<&str>, txid: [u8; 32]) -> Result<bool, MoneroError> {
        let ask = async |node: Arc<DaemonEndpoint>| node.is_known(txid).await;
        Ok(match endpoint {
            Some(url) => self.pool.call_at(url, "monero.is_known", classify, ask).await?,
            None => self.pool.call("monero.is_known", classify, ask).await?,
        })
    }
}

/// Broadcasts through `endpoint`, the node that served the ring.
///
/// Another node of the pool gets the bytes only when that one cannot be reached at
/// all. Any answer from the ring node, a refusal included, is the answer.
pub(crate) async fn broadcast_through_ring(
    node: &dyn MoneroNode,
    endpoint: &str,
    transaction: &Transaction,
) -> Result<BroadcastReply, MoneroError> {
    match node.broadcast(Some(endpoint), transaction).await {
        Err(error) if error.is_unreachable() => node.broadcast(None, transaction).await,
        answered => answered,
    }
}

/// A refusal travels as an error so the pool's classifier can stop the walk on
/// it, and is unwrapped by [`answered`].
async fn broadcast_through(
    node: &DaemonEndpoint,
    transaction: &Transaction,
) -> Result<BroadcastReply, NetError> {
    match node.send_raw_transaction(transaction).await? {
        BroadcastReply::Accepted => Ok(BroadcastReply::Accepted),
        BroadcastReply::Refused(rejection) => Err(MoneroError::Rejected(rejection).into()),
    }
}

/// Unwraps the refusal a broadcast carried as an error back into an answer.
fn answered(result: Result<BroadcastReply, NetError>) -> Result<BroadcastReply, MoneroError> {
    result.or_else(|error| {
        let error = MoneroError::from(error);
        if let MoneroError::Rejected(rejection) = error {
            Ok(BroadcastReply::Refused(rejection))
        } else {
            Err(error)
        }
    })
}

fn height_index(height: u64) -> Result<usize, MoneroError> {
    usize::try_from(height).map_err(|_| MoneroError::HeightOutOfRange { height })
}

/// The pool classifier for every Monero read.
///
/// A malformed request gets the same answer everywhere, so rotating would hide
/// the cause, and anything else is worth another endpoint.
const fn classify(error: &NetError) -> Outcome {
    if let NetError::Rpc { code, .. } = *error {
        jelly_net::json_rpc_outcome(code)
    } else {
        Outcome::Rotate
    }
}

/// The pool classifier for a broadcast.
///
/// A classified refusal is the node's answer, not its failure, so accepting it
/// keeps the typed [`jelly_core::Rejection`] instead of burying it inside `AllFailed`.
fn classify_broadcast(error: &NetError) -> Outcome {
    if matches!(error.source_of::<MoneroError>(), Some(MoneroError::Rejected(_))) {
        Outcome::Accept
    } else {
        Outcome::Rotate
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, Clock, Transport, test_util::FakeClock};
    use jelly_net::{Connect, Endpoint, Identity, NetRuntime};

    use super::*;
    use crate::broadcast::BroadcastRefusal;
    use crate::fixtures::label;

    /// A connector that never dials, because these checks are about construction.
    #[derive(Debug)]
    struct NeverDials;

    #[async_trait]
    impl Connect<DaemonEndpoint> for NeverDials {
        async fn connect(&self, _endpoint: &Endpoint) -> Result<Arc<DaemonEndpoint>, NetError> {
            Err(NetError::NoEndpoints { chain: CHAIN })
        }

        async fn identify(
            &self,
            _endpoint: &Endpoint,
            _connection: &DaemonEndpoint,
        ) -> Result<Identity, NetError> {
            Ok(Identity::Unchecked)
        }
    }

    fn pool(chain: ChainId, urls: &[&str]) -> Arc<Pool<DaemonEndpoint>> {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
        let runtime = NetRuntime::new(clock, &[]);
        let endpoints = urls
            .iter()
            .filter_map(|url| Endpoint::parse(chain, Transport::MoneroRpc, url).ok())
            .collect();
        Arc::new(runtime.pool(chain, Transport::MoneroRpc, Arc::new(NeverDials), endpoints))
    }

    #[test]
    fn a_pool_for_another_chain_or_with_no_endpoints_is_refused_at_construction() {
        let wrong = PooledNode::new(pool(ChainId::Ethereum, &["https://node.invalid"]));
        assert!(matches!(wrong, Err(MoneroError::ForeignChain { chain: ChainId::Ethereum })));
        let empty = PooledNode::new(pool(CHAIN, &[]));
        assert!(matches!(empty, Err(MoneroError::Net(NetError::NoEndpoints { .. }))));
        PooledNode::new(pool(CHAIN, &["https://node.invalid:18081"])).expect("it builds");
    }

    #[test]
    fn a_refusal_is_accepted_by_the_broadcast_classifier_and_rotated_by_the_read_one() {
        let rejected =
            MoneroError::Rejected(BroadcastRefusal::FeeTooLow.into_rejection("fee too low"));
        let carried = NetError::from(rejected);
        assert_eq!(classify_broadcast(&carried), Outcome::Accept);
        assert_eq!(classify(&carried), Outcome::Rotate);

        let answered = MoneroError::answer(&label(), "the node quoted no fee");
        let carried = NetError::from(answered);
        assert_eq!(classify_broadcast(&carried), Outcome::Rotate);
        assert!(matches!(MoneroError::from(carried), MoneroError::Answer { .. }));
    }
}
