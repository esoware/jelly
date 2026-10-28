use std::sync::Arc;

use jelly_core::{BuildError, BuildRequest, ChainAdapter, ChainError};
use jelly_net::{Endpoint, Network, ProbeReport, TimeoutPolicy};

use crate::account::MoneroAccount;
use crate::adapter::MoneroAdapter;
use crate::daemon::MoneroConnector;
use crate::entry::SPEC;
use crate::keys::MoneroKeys;
use crate::node::PooledNode;
use crate::params::CHAIN;

/// Async because building restores the persisted scan state.
pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let material = request.account_key(CHAIN).ok_or(BuildError::NoKeys { chain: CHAIN })?;
    let keys = MoneroKeys::from_key_material(material)
        .map_err(|error| ChainError::internal("the Monero account key is unusable", error))?;
    let account = MoneroAccount::try_from(keys).map_err(ChainError::from)?;

    let pool = network.primary_pool(&SPEC, daemon_rpc(network))?;
    let node = PooledNode::new(pool).map_err(ChainError::from)?;

    let adapter = MoneroAdapter::builder(
        Arc::new(node),
        account,
        Arc::clone(&request.cursors),
        Arc::clone(network.clock()),
        request.restore_height(CHAIN),
    )
    .build()
    .await?;
    Ok(Arc::new(adapter))
}

/// Asks one endpoint which network it serves, through the connector the pool is built
/// with, so the endpoint editor and the pool's guard can't disagree.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    if endpoint.transport() == SPEC.primary_transport {
        jelly_net::probe(&daemon_rpc(network), network.clock().as_ref(), endpoint).await
    } else {
        ProbeReport::unused_transport(endpoint.label())
    }
}

/// The connector's request budget is the pool's per-attempt timeout, so both bound a
/// request the same way.
fn daemon_rpc(network: &Network) -> MoneroConnector {
    MoneroConnector::new(
        network.http(),
        TimeoutPolicy::for_transport(SPEC.primary_transport).per_attempt,
    )
}
