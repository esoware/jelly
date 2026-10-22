use jelly_core::{Transport, test_util::build_request};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport, test_util::direct_network};
use jelly_zcash::{ENTRY, build, probe};

use crate::common::CHAIN;

#[tokio::test]
async fn build_pools_the_shipped_endpoints_without_opening_a_socket() {
    let network = direct_network(&[ENTRY]).unwrap();
    let adapter = build(&network, &build_request(&[CHAIN])).await.unwrap();
    assert_eq!(adapter.chain(), CHAIN);
}

#[tokio::test]
async fn a_probe_on_a_transport_this_chain_never_uses_asks_nothing() {
    let endpoint = Endpoint::parse(CHAIN, Transport::HttpJsonrpc, "https://node.invalid").unwrap();
    assert_eq!(
        probe(&direct_network(&[ENTRY]).unwrap(), &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
