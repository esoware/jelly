use std::sync::Arc;

use jelly_core::{BuildError, BuildRequest, ChainAdapter, ChainSpec, Transport};
use jelly_electrum::ElectrumConnector;
use jelly_net::{Endpoint, NetError, Network, ProbeReport};

use crate::adapter::UtxoAdapter;
use crate::fee_api::{FeeApi, MempoolFeeApi};
use crate::params::{FeeMarket, UtxoParams};

/// The connector a chain's Electrum pool and its endpoint probe share, over the pinned
/// dialler and the chain guard.
///
/// Block zero, and the checkpoint when the table names one, are hashed and
/// compared against the table's, which keeps one chain's server out of another's
/// pool. The chains share the protocol, so a wrong-network server could otherwise
/// report an empty wallet. The pool runs the guard before an endpoint's first
/// request, the endpoint probe runs it on demand, and the client re-runs it on
/// every later socket.
fn electrum(network: &Network, params: &UtxoParams) -> Result<ElectrumConnector, NetError> {
    Ok(ElectrumConnector::new(network.electrum_dialler(params.chain)?, params.electrum_chain()))
}

/// The chain's adapter over its Electrum pool. Construction opens no sockets.
///
/// An auctioned chain without a fee service falls back to the Electrum server's
/// estimator. A flat chain reads no market, so it has no fee service to build.
pub async fn build(
    network: &Network,
    request: &BuildRequest,
    params: &UtxoParams,
    spec: &ChainSpec,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let pool = network.primary_pool(spec, electrum(network, params)?)?;
    let fee_api: Option<Arc<dyn FeeApi>> = if let FeeMarket::Auction(auction) = params.fee_market
        && let Some(fee_pool) = network.rest_pool(spec.chain, Transport::FeeApi)?
    {
        Some(Arc::new(MempoolFeeApi::new(fee_pool, &auction)))
    } else {
        None
    };
    let cursors = Arc::clone(&request.cursors);
    let adapter = UtxoAdapter::new(params, pool, cursors, Arc::clone(network.clock()), fee_api)?;
    Ok(Arc::new(adapter))
}

/// Asks one Electrum endpoint which network it serves, through the connector the
/// chain's pool is built with, so the endpoint editor and the pool's guard cannot
/// disagree.
///
/// The registry asks a REST service whether it answers before it dispatches here, so an
/// endpoint on any other transport was built outside the registry and gets the
/// registry's own refusal.
pub async fn probe(network: &Network, endpoint: &Endpoint, params: &UtxoParams) -> ProbeReport {
    if endpoint.transport() != Transport::ElectrumSsl {
        return ProbeReport::unused_transport(endpoint.label());
    }
    match electrum(network, params) {
        Ok(connector) => jelly_net::probe(&connector, network.clock().as_ref(), endpoint).await,
        Err(error) => ProbeReport::from_failure(endpoint.label(), &error),
    }
}
