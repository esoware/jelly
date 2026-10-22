use jelly_core::{ChainId, Transport, test_util::build_request};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};
use jelly_solana::{ENTRY, build, probe};

#[tokio::test]
async fn a_build_over_the_shipped_endpoints_opens_no_socket_and_yields_a_solana_adapter() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &build_request(&[ChainId::Solana])).await.unwrap();
    assert_eq!(adapter.chain(), ChainId::Solana);
}

#[tokio::test]
async fn a_probe_on_a_transport_solana_does_not_use_asks_nothing() {
    let network = direct_network(&[ENTRY]).unwrap();
    for transport in [Transport::TronHttp, Transport::FeeApi, Transport::HistoryApi] {
        let endpoint = Endpoint::parse(ChainId::Solana, transport, "https://node.invalid").unwrap();
        assert_eq!(
            probe(&network, &endpoint).await,
            ProbeReport::Unreachable { failure: ProbeFailure::NotProbed },
            "{transport}"
        );
    }
}
