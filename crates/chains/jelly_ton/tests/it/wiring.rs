use jelly_core::{ChainId, Transport, test_util::build_request};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};
use jelly_ton::ENTRY;

#[tokio::test]
async fn build_pools_the_shipped_endpoints_without_opening_a_socket() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = jelly_ton::build(&network, &build_request(&[ChainId::Ton])).await.unwrap();
    assert_eq!(adapter.chain(), ChainId::Ton);
    assert!(adapter.pending_tracker().is_some());
}

#[tokio::test]
async fn a_probe_on_a_transport_this_chain_never_uses_asks_nothing() {
    let endpoint =
        Endpoint::parse(ChainId::Ton, Transport::TronHttp, "https://example.com").unwrap();
    assert_eq!(
        jelly_ton::probe(&direct_network(&[ENTRY]).unwrap(), &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
