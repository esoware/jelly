use std::sync::Arc;

use jelly_core::{
    BuildError, BuildRequest, ChainAdapter, ChainError, KeyService, SecretText, Transport,
};
use jelly_net::{Endpoint, HttpConnector, Network, ProbeReport};

use crate::adapter::TronAdapter;
use crate::entry::SPEC;
use crate::identity::TronIdentity;
use crate::node::PooledNode;

/// Builds the adapter over the endpoint pool. The request's `TronGrid` key, if any,
/// is held only inside the node, and construction opens no sockets.
pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let key = request
        .with_api_key(KeyService::TronGrid, |key| key.map(|key| SecretText::new(key.to_owned())));
    let pool = network.primary_pool(&SPEC, connector(network))?;
    let node = PooledNode::new(pool, key).map_err(ChainError::from)?;

    let adapter =
        TronAdapter::new(Arc::new(node), Arc::clone(&request.cursors), Arc::clone(network.clock()));
    Ok(Arc::new(adapter))
}

/// Asks one endpoint which network it serves, through the connector its pool is built
/// with, so the endpoint editor and the pool's guard can't disagree.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    if endpoint.transport() != Transport::TronHttp {
        return ProbeReport::unused_transport(endpoint.label());
    }
    jelly_net::probe(&connector(network), network.clock().as_ref(), endpoint).await
}

fn connector(network: &Network) -> HttpConnector {
    HttpConnector::new(network.http(), Arc::new(TronIdentity))
}
