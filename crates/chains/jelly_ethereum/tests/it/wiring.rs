use std::collections::BTreeMap;

use jelly_core::{
    BuildRequest, ChainId, KeyService, SecretText, Transport, test_util::build_request,
};
use jelly_ethereum::{ENTRY, build, probe};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};

fn request(api_key: &str) -> BuildRequest {
    BuildRequest {
        api_keys: BTreeMap::from([(KeyService::Etherscan, SecretText::new(api_key.to_owned()))]),
        ..build_request(&[ChainId::Ethereum])
    }
}

#[tokio::test]
async fn the_shipped_endpoints_build_an_adapter_with_indexed_history_under_a_blank_key() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &request("")).await.unwrap();
    assert_eq!(adapter.chain(), ChainId::Ethereum);
    assert!(adapter.fee_bump().is_some());
    let rendered = format!("{adapter:?}");
    assert!(
        rendered.contains("indexed_history: true"),
        "the shipped service answers without a key: {rendered}"
    );
}

#[tokio::test]
async fn an_etherscan_key_keeps_the_keyless_indexed_history() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &request(" a-key ")).await.unwrap();
    let rendered = format!("{adapter:?}");
    assert!(rendered.contains("indexed_history: true"), "{rendered}");
    assert!(!rendered.contains("a-key"), "the key is never printed: {rendered}");
}

#[tokio::test]
async fn a_probe_of_a_transport_the_chain_does_not_identify_asks_nothing() {
    let network = direct_network(&[ENTRY]).unwrap();
    let endpoint = Endpoint::parse(
        ChainId::Ethereum,
        Transport::HistoryApi,
        "https://api.etherscan.io/v2/api",
    )
    .unwrap();
    assert_eq!(
        probe(&network, &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
