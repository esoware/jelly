use std::sync::Arc;

use jelly_core::{BuildError, BuildRequest, ChainAdapter, ChainError};
use jelly_net::{Endpoint, HttpConnector, Network, ProbeReport};

use crate::adapter::ZcashAdapter;
use crate::entry::SPEC;
use crate::identity::ZcashIdentity;
use crate::node::PooledNode;

pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let pool = network.primary_pool(&SPEC, blockbook(network))?;
    let node = PooledNode::new(pool).map_err(ChainError::from)?;
    let adapter = ZcashAdapter::new(
        Arc::new(node),
        Arc::clone(&request.cursors),
        Arc::clone(network.clock()),
    );
    Ok(Arc::new(adapter))
}

/// Asks one endpoint which network it serves, through the connector the pool is built
/// with, so the endpoint editor and the pool's guard can't disagree.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    if endpoint.transport() == SPEC.primary_transport {
        jelly_net::probe(&blockbook(network), network.clock().as_ref(), endpoint).await
    } else {
        ProbeReport::unused_transport(endpoint.label())
    }
}

fn blockbook(network: &Network) -> HttpConnector {
    HttpConnector::new(network.http(), Arc::new(ZcashIdentity))
}
