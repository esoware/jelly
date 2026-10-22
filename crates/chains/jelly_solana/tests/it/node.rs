use std::sync::Arc;

use jelly_core::{ChainId, Clock, Transport, test_util::FakeClock};
use jelly_net::{
    Endpoint, HttpClient, HttpConnector, HttpEndpoint, NetError, NetRuntime, NoIdentity,
    PacerPolicy, Pool,
};
use jelly_solana::{PooledNode, SolanaError, SolanaNode as _};

use crate::common::Fallible;

/// A real HTTP pool for construction checks. Building a pool opens no socket, so
/// nothing is ever identified.
fn pool(chain: ChainId, urls: &[&str]) -> Fallible<Arc<Pool<HttpEndpoint>>> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let http = HttpClient::new(None)?;
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let connector = HttpConnector::new(http, Arc::new(NoIdentity));
    let mut endpoints = Vec::with_capacity(urls.len());
    for url in urls {
        endpoints.push(Endpoint::parse(chain, Transport::HttpJsonrpc, url)?);
    }
    Ok(Arc::new(runtime.pool(chain, Transport::HttpJsonrpc, Arc::new(connector), endpoints)))
}

#[test]
fn a_pool_on_another_chain_is_refused_at_construction() {
    let error = PooledNode::new(pool(ChainId::Ethereum, &["https://node.invalid"]).unwrap())
        .expect_err("another chain");
    assert!(matches!(error, SolanaError::ForeignChain { chain: ChainId::Ethereum }), "{error:?}");
}

#[test]
fn a_pool_with_no_endpoints_is_refused_at_construction() {
    let error = PooledNode::new(pool(ChainId::Solana, &[]).unwrap()).expect_err("no endpoints");
    assert!(
        matches!(error, SolanaError::Net(NetError::NoEndpoints { chain: ChainId::Solana })),
        "{error:?}"
    );
}

#[test]
fn a_pool_on_solana_builds_a_node_that_starts_out_batching() {
    let node = PooledNode::new(pool(ChainId::Solana, &["https://node.invalid"]).unwrap()).unwrap();
    assert!(node.is_batching());
    assert!(format!("{node:?}").contains("Solana"), "{node:?}");
}

#[tokio::test]
async fn an_empty_batch_is_refused_before_any_endpoint_is_asked() {
    let node = PooledNode::new(pool(ChainId::Solana, &["https://node.invalid"]).unwrap()).unwrap();
    let error = node.batch(&[]).await.expect_err("nothing to send");
    assert!(matches!(error, SolanaError::EmptyBatch), "{error:?}");
}
