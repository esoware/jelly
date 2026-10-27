use jelly_core::{ChainId, Transport};
use jelly_net::{Endpoint, Network, ProbeReport, probe_reachable};

/// Asks one endpoint which network it serves, through the connector its chain's
/// pool is built with, so the endpoint editor and the pool's guard cannot
/// disagree.
///
/// A REST service is only asked whether it answers, because its pool sends
/// nothing to identify it.
pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    if matches!(endpoint.transport(), Transport::FeeApi | Transport::HistoryApi) {
        return probe_reachable(&network.http(), network.clock().as_ref(), endpoint).await;
    }
    match endpoint.chain() {
        ChainId::Bitcoin => jelly_bitcoin::probe(network, endpoint).await,
        ChainId::Litecoin => jelly_litecoin::probe(network, endpoint).await,
        ChainId::Dogecoin => jelly_dogecoin::probe(network, endpoint).await,
        ChainId::BitcoinCash => jelly_bitcoin_cash::probe(network, endpoint).await,
        ChainId::Zcash => jelly_zcash::probe(network, endpoint).await,
        ChainId::Ethereum => jelly_ethereum::probe(network, endpoint).await,
        ChainId::Arbitrum => jelly_arbitrum::probe(network, endpoint).await,
        ChainId::Base => jelly_base::probe(network, endpoint).await,
        ChainId::Optimism => jelly_optimism::probe(network, endpoint).await,
        ChainId::Bsc => jelly_bsc::probe(network, endpoint).await,
        ChainId::Polygon => jelly_polygon::probe(network, endpoint).await,
        ChainId::Avalanche => jelly_avalanche::probe(network, endpoint).await,
        ChainId::Solana => jelly_solana::probe(network, endpoint).await,
        ChainId::Tron => jelly_tron::probe(network, endpoint).await,
        ChainId::Xrp => jelly_xrp::probe(network, endpoint).await,
        ChainId::Ton => jelly_ton::probe(network, endpoint).await,
        ChainId::Monero => jelly_monero::probe(network, endpoint).await,
    }
}
