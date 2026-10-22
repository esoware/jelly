use std::sync::Arc;

use jelly_core::{
    BuildError, BuildRequest, ChainAdapter, ChainError, ChainSpec, KeyService, Transport,
};
use jelly_net::{Endpoint, HttpConnector, NetError, Network, ProbeReport, WsConnector};

use crate::adapter::EvmAdapter;
use crate::etherscan::EtherscanHistory;
use crate::node::PooledNode;
use crate::params::EvmParams;

/// The adapter for the network `params` names. Construction opens no sockets.
///
/// Without a history endpoint, and an Etherscan key unless the network's service is
/// keyless, history walks `eth_getLogs` instead.
pub async fn build(
    spec: &ChainSpec,
    params: &'static EvmParams,
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let chain = params.chain;
    let pool = network.primary_pool(spec, rpc_connector(network, params))?;
    let node = PooledNode::new(params, pool).map_err(ChainError::from)?;

    let mut builder = EvmAdapter::builder(
        params,
        Arc::new(node),
        Arc::clone(&request.cursors),
        Arc::clone(network.clock()),
    );
    if let Some(pushes) = network.ws_pool(chain, || ws_connector(network, params))? {
        builder = builder.pushes(pushes);
    }
    let builder = request.with_api_key(KeyService::Etherscan, |key| {
        let key = key.filter(|_| !params.has_keyless_history);
        if (key.is_some() || params.has_keyless_history)
            && let Some(history_pool) = network.rest_pool(chain, Transport::HistoryApi)?
        {
            let history = match key {
                Some(key) => {
                    EtherscanHistory::new(history_pool, key, params).map_err(ChainError::from)?
                }
                None => EtherscanHistory::keyless(history_pool, params),
            };
            builder = builder.history(Arc::new(history));
        }
        Ok::<_, BuildError>(builder)
    })?;
    Ok(Arc::new(builder.build()?))
}

/// Asks one endpoint which network it serves, through the connector its pool is
/// built with, so the endpoint editor and the pool's guard cannot disagree.
pub async fn probe(params: &EvmParams, network: &Network, endpoint: &Endpoint) -> ProbeReport {
    let clock = network.clock().as_ref();
    match endpoint.transport() {
        Transport::HttpJsonrpc => {
            jelly_net::probe(&rpc_connector(network, params), clock, endpoint).await
        }
        Transport::WsJsonrpc => match ws_connector(network, params) {
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

fn rpc_connector(network: &Network, params: &EvmParams) -> HttpConnector {
    HttpConnector::new(network.http(), Arc::new(*params))
}

fn ws_connector(network: &Network, params: &EvmParams) -> Result<WsConnector, NetError> {
    Ok(WsConnector::new(network.ws_dialler()?, Arc::clone(network.clock()), Arc::new(*params)))
}
