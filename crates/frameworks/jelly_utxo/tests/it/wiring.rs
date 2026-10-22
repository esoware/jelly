//! Each chain crate's build and probe over a real network layer, which builds pools
//! from the shipped endpoints and opens no socket until a request.

use std::sync::Arc;

use jelly_core::{BuildError, ChainId, EndpointOverride, Transport, test_util::build_request};
use jelly_net::{
    Endpoint, NetError, Network, ProbeFailure, ProbeReport, test_util::direct_network,
};

use crate::common::Fallible;

fn network() -> Result<Arc<Network>, NetError> {
    direct_network(&[
        jelly_bitcoin::ENTRY,
        jelly_litecoin::ENTRY,
        jelly_bitcoin_cash::ENTRY,
        jelly_dogecoin::ENTRY,
    ])
}

/// Disables every shipped endpoint of `chain` on `transport`.
fn disable(network: &Network, chain: ChainId, transport: Transport) -> Fallible<()> {
    let rows: Vec<(ChainId, EndpointOverride)> = network
        .registry()
        .enabled(chain, transport)?
        .iter()
        .map(|endpoint| {
            (chain, EndpointOverride { is_enabled: false, ..EndpointOverride::new(endpoint.url()) })
        })
        .collect();
    network.registry().load_overrides(rows);
    Ok(())
}

#[tokio::test]
async fn each_chain_builds_its_adapter_from_its_shipped_endpoints() {
    let network = network().unwrap();
    let built = [
        (
            jelly_bitcoin::build(&network, &build_request(&[ChainId::Bitcoin])).await,
            jelly_bitcoin::SPEC,
        ),
        (
            jelly_litecoin::build(&network, &build_request(&[ChainId::Litecoin])).await,
            jelly_litecoin::SPEC,
        ),
        (
            jelly_bitcoin_cash::build(&network, &build_request(&[ChainId::BitcoinCash])).await,
            jelly_bitcoin_cash::SPEC,
        ),
        (
            jelly_dogecoin::build(&network, &build_request(&[ChainId::Dogecoin])).await,
            jelly_dogecoin::SPEC,
        ),
    ];
    for (outcome, spec) in built {
        let adapter = outcome.unwrap();
        assert_eq!(adapter.chain(), spec.chain);
        assert_eq!(adapter.fee_bump().is_some(), spec.can_bump_fee, "{}", spec.chain);
    }
}

/// The fee service is optional, so an auctioned chain without one still builds and
/// falls back to the Electrum estimator.
#[tokio::test]
async fn a_chain_without_a_fee_service_still_builds() {
    let network = network().unwrap();
    disable(&network, ChainId::Bitcoin, Transport::FeeApi).unwrap();
    assert!(network.registry().enabled(ChainId::Bitcoin, Transport::FeeApi).unwrap().is_empty());
    let adapter =
        jelly_bitcoin::build(&network, &build_request(&[ChainId::Bitcoin])).await.unwrap();
    assert_eq!(adapter.chain(), ChainId::Bitcoin);
}

#[tokio::test]
async fn a_chain_without_an_electrum_endpoint_returns_no_endpoints_for_its_transport() {
    let network = network().unwrap();
    disable(&network, ChainId::Dogecoin, Transport::ElectrumSsl).unwrap();
    let refused = jelly_dogecoin::build(&network, &build_request(&[ChainId::Dogecoin])).await;
    assert!(
        matches!(
            refused,
            Err(BuildError::NoEndpoints {
                chain: ChainId::Dogecoin,
                transport: Transport::ElectrumSsl
            })
        ),
        "{:?}",
        refused.map(|adapter| adapter.chain())
    );
}

/// The registry probes a REST service itself, so a chain asked about one sends
/// nothing and gives the registry's own refusal.
#[tokio::test]
async fn a_probe_of_a_transport_the_chain_does_not_identify_asks_nothing() {
    let network = network().unwrap();
    let endpoint =
        Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, "https://fees.invalid").unwrap();
    assert_eq!(
        jelly_bitcoin::probe(&network, &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
