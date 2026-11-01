use jelly_core::{BuildError, ChainId, EndpointOverride, Transport, test_util::build_request};
use jelly_dogecoin::{ENTRY, SPEC, build};
use jelly_net::test_util::direct_network;

#[tokio::test]
async fn the_shipped_endpoints_build_an_adapter_with_the_specs_fee_bump() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &build_request(&[ChainId::Dogecoin])).await.unwrap();
    assert_eq!(adapter.chain(), SPEC.chain);
    assert_eq!(adapter.fee_bump().is_some(), SPEC.can_bump_fee);
}

#[tokio::test]
async fn a_chain_without_an_electrum_endpoint_returns_no_endpoints_for_its_transport() {
    let network = direct_network(&[ENTRY]).unwrap();
    let registry = network.registry();
    let servers = registry.enabled(ChainId::Dogecoin, Transport::ElectrumSsl).unwrap();
    registry.load_overrides(servers.iter().map(|endpoint| {
        let row = EndpointOverride { is_enabled: false, ..EndpointOverride::new(endpoint.url()) };
        (ChainId::Dogecoin, row)
    }));
    let refused = build(&network, &build_request(&[ChainId::Dogecoin])).await;
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
