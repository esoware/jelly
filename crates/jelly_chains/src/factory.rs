use std::fmt;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use jelly_core::{
    AdapterBuilder, AdapterSet, BuildError, BuildRequest, ChainAdapter, ChainId, ChainOutcome,
    SkipReason,
};
use jelly_net::Network;

const TARGET: &str = "jelly::chains::factory";

/// Builds an adapter or a typed skip for every requested chain. Construction
/// opens no sockets.
///
/// The shell calls `attach` only once the engine runs, because the network layer
/// verifies TLS against the engine's pins. Until then builds skip every chain, and a
/// later rebuild retries them. Nothing detaches it, which is safe because the network
/// it holds reaches the engine only through the pin store, and that does not keep the
/// database open.
#[derive(Default)]
pub struct AdapterFactory {
    network: OnceLock<Arc<Network>>,
}

impl fmt::Debug for AdapterFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdapterFactory")
            .field("is_attached", &self.network.get().is_some())
            .finish_non_exhaustive()
    }
}

impl AdapterFactory {
    pub const fn new() -> Self {
        Self { network: OnceLock::new() }
    }

    pub fn attach(&self, network: Arc<Network>) {
        if self.network.set(network).is_err() {
            tracing::warn!(
                target: TARGET,
                "the adapter factory is already attached, so the new network was ignored"
            );
        }
    }
}

#[async_trait]
impl AdapterBuilder for AdapterFactory {
    async fn build(&self, request: &BuildRequest) -> AdapterSet {
        let Some(network) = self.network.get() else {
            tracing::error!(
                target: TARGET,
                "no chain could be built, because the adapter factory has no network yet"
            );
            return every_chain_failed(request);
        };
        let network = match network.with_proxy(request.proxy.clone()) {
            Ok(network) => network,
            Err(error) => {
                tracing::error!(
                    target: TARGET,
                    %error,
                    "no chain could be built, because the egress route could not be built"
                );
                return every_chain_failed(request);
            }
        };

        let mut outcomes = Vec::with_capacity(request.chains.len());
        for &chain in &request.chains {
            outcomes.push((chain, outcome(chain, &network, request).await));
        }
        outcomes.into_iter().collect()
    }
}

fn every_chain_failed(request: &BuildRequest) -> AdapterSet {
    request
        .chains
        .iter()
        .map(|&chain| (chain, ChainOutcome::Skipped(SkipReason::BuildFailed)))
        .collect()
}

/// A skip is logged and reported rather than propagated, so one chain that
/// cannot be built does not cost the rest.
///
/// Only a failed build is a warning. The other reasons are expected states
/// that would otherwise warn at every rebuild.
async fn outcome(chain: ChainId, network: &Network, request: &BuildRequest) -> ChainOutcome {
    let error = match build(chain, network, request).await {
        Ok(adapter) => return ChainOutcome::Built(adapter),
        Err(error) => error,
    };
    let reason = error.skip_reason();
    match reason {
        SkipReason::BuildFailed => tracing::warn!(
            target: TARGET,
            chain = chain.as_str(),
            reason = reason.as_str(),
            %error,
            "this chain has no adapter"
        ),
        SkipReason::NoEndpoints | SkipReason::NoKeys => tracing::info!(
            target: TARGET,
            chain = chain.as_str(),
            reason = reason.as_str(),
            %error,
            "this chain has no adapter"
        ),
    }
    ChainOutcome::Skipped(reason)
}

/// The one place a chain's builder is named.
async fn build(
    chain: ChainId,
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    match chain {
        ChainId::Bitcoin => jelly_bitcoin::build(network, request).await,
        ChainId::Litecoin => jelly_litecoin::build(network, request).await,
        ChainId::Dogecoin => jelly_dogecoin::build(network, request).await,
        ChainId::BitcoinCash => jelly_bitcoin_cash::build(network, request).await,
        ChainId::Zcash => jelly_zcash::build(network, request).await,
        ChainId::Ethereum => jelly_ethereum::build(network, request).await,
        ChainId::Arbitrum => jelly_arbitrum::build(network, request).await,
        ChainId::Base => jelly_base::build(network, request).await,
        ChainId::Optimism => jelly_optimism::build(network, request).await,
        ChainId::Bsc => jelly_bsc::build(network, request).await,
        ChainId::Polygon => jelly_polygon::build(network, request).await,
        ChainId::Avalanche => jelly_avalanche::build(network, request).await,
        ChainId::Solana => jelly_solana::build(network, request).await,
        ChainId::Tron => jelly_tron::build(network, request).await,
        ChainId::Xrp => jelly_xrp::build(network, request).await,
        ChainId::Ton => jelly_ton::build(network, request).await,
        ChainId::Monero => jelly_monero::build(network, request).await,
    }
}
