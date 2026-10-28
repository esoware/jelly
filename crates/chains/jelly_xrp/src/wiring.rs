use std::sync::Arc;

use jelly_core::{BuildError, BuildRequest, ChainAdapter, ChainError, Transport};
use jelly_net::{Endpoint, HttpConnector, Network, ProbeReport};

use crate::adapter::XrpAdapter;
use crate::entry::SPEC;
use crate::identity::XrpIdentity;
use crate::node::PooledNode;

pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let pool = network.primary_pool(&SPEC, rpc(network))?;
    let node = PooledNode::new(pool).map_err(ChainError::from)?;
    let adapter =
        XrpAdapter::new(Arc::new(node), Arc::clone(&request.cursors), Arc::clone(network.clock()));
    Ok(Arc::new(adapter))
}

/// Asks one endpoint which network it serves, through the connector its pool uses.
///
/// An endpoint on any other transport is reported as unused. The registry refuses one, so
/// only an endpoint built outside the registry gets that answer.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    if endpoint.transport() == Transport::HttpJsonrpc {
        jelly_net::probe(&rpc(network), network.clock().as_ref(), endpoint).await
    } else {
        ProbeReport::unused_transport(endpoint.label())
    }
}

fn rpc(network: &Network) -> HttpConnector {
    HttpConnector::new(network.http(), Arc::new(XrpIdentity))
}
