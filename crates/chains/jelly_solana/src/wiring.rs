use std::sync::Arc;

use jelly_core::{BuildError, BuildRequest, ChainAdapter, ChainError, Transport};
use jelly_net::{Endpoint, HttpConnector, NetError, Network, ProbeReport, WsConnector};

use crate::adapter::SolanaAdapter;
use crate::entry::SPEC;
use crate::identity::SolanaIdentity;
use crate::node::PooledNode;

/// The adapter over the HTTP pool, woken by websocket pushes when a websocket endpoint
/// is configured. Construction opens no sockets.
pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let pool = network.primary_pool(&SPEC, rpc_connector(network))?;
    let node = PooledNode::new(pool).map_err(ChainError::from)?;

    let mut builder = SolanaAdapter::builder(
        Arc::new(node),
        Arc::clone(&request.cursors),
        Arc::clone(network.clock()),
    );
    if let Some(pushes) = network.ws_pool(SPEC.chain, || ws_connector(network))? {
        builder = builder.pushes(pushes);
    }
    Ok(Arc::new(builder.build()))
}

/// Asks one endpoint which network it serves, through the connector its pool is built
/// with, so the endpoint editor and the pool's guard cannot disagree.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    let clock = network.clock().as_ref();
    match endpoint.transport() {
        Transport::HttpJsonrpc => jelly_net::probe(&rpc_connector(network), clock, endpoint).await,
        Transport::WsJsonrpc => match ws_connector(network) {
            Ok(connector) => jelly_net::probe(&connector, clock, endpoint).await,
            Err(error) => ProbeReport::from_failure(endpoint.label(), &error),
        },
        Transport::ElectrumSsl
        | Transport::MoneroRpc
        | Transport::TronHttp
        | Transport::Blockbook
        | Transport::FeeApi
        | Transport::HistoryApi => ProbeReport::unused_transport(endpoint.label()),
    }
}

fn rpc_connector(network: &Network) -> HttpConnector {
    HttpConnector::new(network.http(), Arc::new(SolanaIdentity))
}

fn ws_connector(network: &Network) -> Result<WsConnector, NetError> {
    Ok(WsConnector::new(
        network.ws_dialler()?,
        Arc::clone(network.clock()),
        Arc::new(SolanaIdentity),
    ))
}
