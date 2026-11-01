use jelly_bitcoin::{ENTRY, SPEC, build, probe};
use jelly_core::{ChainId, EndpointOverride, Transport, test_util::build_request};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};

#[tokio::test]
async fn the_shipped_endpoints_build_an_adapter_with_the_specs_fee_bump() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &build_request(&[ChainId::Bitcoin])).await.unwrap();
    assert_eq!(adapter.chain(), SPEC.chain);
    assert_eq!(adapter.fee_bump().is_some(), SPEC.can_bump_fee);
}

/// The fee service is optional, so an auctioned chain without one still builds and
/// falls back to the Electrum estimator.
#[tokio::test]
async fn a_chain_without_a_fee_service_still_builds() {
    let network = direct_network(&[ENTRY]).unwrap();
    let registry = network.registry();
    let fee_services = registry.enabled(ChainId::Bitcoin, Transport::FeeApi).unwrap();
    registry.load_overrides(fee_services.iter().map(|endpoint| {
        let row = EndpointOverride { is_enabled: false, ..EndpointOverride::new(endpoint.url()) };
        (ChainId::Bitcoin, row)
    }));
    assert!(registry.enabled(ChainId::Bitcoin, Transport::FeeApi).unwrap().is_empty());
    let adapter = build(&network, &build_request(&[ChainId::Bitcoin])).await.unwrap();
    assert_eq!(adapter.chain(), ChainId::Bitcoin);
}

/// The registry probes a REST service itself, so a chain asked about one sends
/// nothing and gives the registry's own refusal.
#[tokio::test]
async fn a_probe_of_a_transport_the_chain_does_not_identify_asks_nothing() {
    let network = direct_network(&[ENTRY]).unwrap();
    let endpoint =
        Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, "https://fees.invalid").unwrap();
    assert_eq!(
        probe(&network, &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
