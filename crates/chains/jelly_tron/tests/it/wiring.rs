use jelly_core::{
    BuildRequest, ChainId, KeyService, SecretText, Transport, test_util::build_request,
};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};
use jelly_tron::{ENTRY, build, probe};

fn request(api_key: Option<&str>) -> BuildRequest {
    BuildRequest {
        api_keys: api_key
            .map(|key| (KeyService::TronGrid, SecretText::new(key.to_owned())))
            .into_iter()
            .collect(),
        ..build_request(&[ChainId::Tron])
    }
}

#[tokio::test]
async fn a_build_over_the_shipped_endpoints_opens_no_socket_and_yields_a_tron_adapter() {
    let network = direct_network(&[ENTRY]).unwrap();
    for api_key in [None, Some(""), Some("trongrid-key")] {
        let adapter = build(&network, &request(api_key)).await.unwrap();
        assert_eq!(adapter.chain(), ChainId::Tron, "{api_key:?}");
    }
}

#[tokio::test]
async fn a_probe_on_a_transport_tron_does_not_use_asks_nothing() {
    let network = direct_network(&[ENTRY]).unwrap();
    for transport in [Transport::HttpJsonrpc, Transport::FeeApi, Transport::HistoryApi] {
        let endpoint = Endpoint::parse(ChainId::Tron, transport, "https://node.invalid").unwrap();
        assert_eq!(
            probe(&network, &endpoint).await,
            ProbeReport::Unreachable { failure: ProbeFailure::NotProbed },
            "{transport}"
        );
    }
}
