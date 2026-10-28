//! Which endpoints the probe identifies and which it only reaches, through the
//! connectors the pools are built with.
//!
//! Electrum and websocket endpoints need TLS or a websocket handshake, so their checks are
//! tested through the same connectors in `jelly_electrum` and the chain crates instead.

use jelly_chains::probe;
use jelly_core::{ChainId, Transport};
use jelly_net::{Endpoint, ProbeFailure, ProbeReport};

use crate::common::{direct_network, serve};

const BSC_CHAIN_ID_REPLY: &str = r#"{"jsonrpc":"2.0","id":1,"result":"0x38"}"#;

/// The probe asks with the connector the Ethereum pool is built with, so a node that
/// answers for BNB Smart Chain is reported as the wrong network.
#[tokio::test]
async fn an_ethereum_node_is_held_to_ethereums_chain_id() {
    let (url, server) = serve(vec![("200 OK", BSC_CHAIN_ID_REPLY)]).await.unwrap();
    let network = direct_network().unwrap();
    let endpoint = Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, &url).unwrap();

    assert_eq!(probe(&network, &endpoint).await, ProbeReport::WrongNetwork);
    let requests = server.await.unwrap().unwrap();
    let request = requests.first().unwrap();
    assert!(request.starts_with("POST / HTTP/1.1\r\n"), "{request}");
    assert!(request.contains(r#""method":"eth_chainId""#), "{request}");
}

#[tokio::test]
async fn a_bsc_node_answering_for_bsc_is_verified() {
    let (url, server) = serve(vec![("200 OK", BSC_CHAIN_ID_REPLY)]).await.unwrap();
    let network = direct_network().unwrap();
    let endpoint = Endpoint::parse(ChainId::Bsc, Transport::HttpJsonrpc, &url).unwrap();

    assert_eq!(probe(&network, &endpoint).await, ProbeReport::Verified { latency_ms: 0 });
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn xrp_nodes_must_report_mainnet_and_cannot_omit_the_network_id() {
    let cases = [
        (r#"{"result":{"info":{"network_id":0}}}"#, ProbeReport::Verified { latency_ms: 0 }),
        (r#"{"result":{"info":{"network_id":1}}}"#, ProbeReport::WrongNetwork),
        (
            r#"{"result":{"info":{}}}"#,
            ProbeReport::Unreachable { failure: ProbeFailure::BadAnswer },
        ),
    ];
    for (body, expected) in cases {
        let (url, server) = serve(vec![("200 OK", body)]).await.unwrap();
        let network = direct_network().unwrap();
        let endpoint = Endpoint::parse(ChainId::Xrp, Transport::HttpJsonrpc, &url).unwrap();
        assert_eq!(probe(&network, &endpoint).await, expected);
        let requests = server.await.unwrap().unwrap();
        assert!(requests.first().unwrap().contains(r#""method":"server_info""#));
    }
}

#[tokio::test]
async fn ton_nodes_must_match_both_mainnet_genesis_hashes() {
    let cases = [
        (
            r#"{"ok":true,"result":{"init":{"root_hash":"F6OpKZKqvqeFp6CQmFomXNMfMj2EnaUSOXN+Mh+wVWk=","file_hash":"XplPz01CXAps5qeSWUtxcyBfdAo5zVb1N979KLSKD24="}}}"#,
            ProbeReport::Verified { latency_ms: 0 },
        ),
        (
            r#"{"result":{"init":{"root_hash":"F6OpKZKqvqeFp6CQmFomXNMfMj2EnaUSOXN+Mh+wVWk=","file_hash":"wrong"}}}"#,
            ProbeReport::WrongNetwork,
        ),
        (
            r#"{"result":{"init":{}}}"#,
            ProbeReport::Unreachable { failure: ProbeFailure::BadAnswer },
        ),
    ];
    for (body, expected) in cases {
        let (url, server) = serve(vec![("200 OK", body)]).await.unwrap();
        let network = direct_network().unwrap();
        let endpoint = Endpoint::parse(ChainId::Ton, Transport::HttpJsonrpc, &url).unwrap();
        assert_eq!(probe(&network, &endpoint).await, expected);
        let requests = server.await.unwrap().unwrap();
        assert!(requests.first().unwrap().contains(r#""method":"getMasterchainInfo""#));
    }
}

#[tokio::test]
async fn zcash_blockbook_endpoints_must_match_mainnet_and_its_genesis() {
    let mainnet = r#"{"blockbook":{"coin":"Zcash"},"backend":{"chain":"main"}}"#;
    let genesis =
        r#"{"blockHash":"00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"}"#;
    for (status, block, expected) in [
        (mainnet, genesis, ProbeReport::Verified { latency_ms: 0 }),
        (mainnet, r#"{"blockHash":"wrong"}"#, ProbeReport::WrongNetwork),
        (
            r#"{"blockbook":{"coin":"Zcash"},"backend":{"chain":"test"}}"#,
            genesis,
            ProbeReport::WrongNetwork,
        ),
    ] {
        let (url, server) = serve(vec![("200 OK", status), ("200 OK", block)]).await.unwrap();
        let network = direct_network().unwrap();
        let endpoint = Endpoint::parse(ChainId::Zcash, Transport::Blockbook, &url).unwrap();
        assert_eq!(probe(&network, &endpoint).await, expected);
        let requests = server.await.unwrap().unwrap();
        assert!(requests.first().unwrap().starts_with("GET /api/v2 HTTP/1.1"));
        assert!(requests.last().unwrap().starts_with("GET /api/v2/block-index/0 HTTP/1.1"));
    }
}

#[tokio::test]
async fn a_tron_node_is_held_to_mainnets_block_zero() {
    let foreign_block_zero =
        r#"{"blockID":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
    let (url, server) = serve(vec![("200 OK", foreign_block_zero)]).await.unwrap();
    let network = direct_network().unwrap();
    let endpoint = Endpoint::parse(ChainId::Tron, Transport::TronHttp, &url).unwrap();

    assert_eq!(probe(&network, &endpoint).await, ProbeReport::WrongNetwork);
    let requests = server.await.unwrap().unwrap();
    let request = requests.first().unwrap();
    assert!(request.starts_with("POST /wallet/getblockbynum HTTP/1.1\r\n"), "{request}");
}

/// The Monero connector negotiates with the daemon first, and only then asks
/// which network it runs.
#[tokio::test]
async fn a_monero_daemon_is_held_to_mainnet() {
    let stagenet =
        r#"{"jsonrpc":"2.0","id":1,"result":{"status":"OK","nettype":"stagenet","height":1}}"#;
    let (url, server) = serve(vec![("200 OK", "[]"), ("200 OK", stagenet)]).await.unwrap();
    let network = direct_network().unwrap();
    let endpoint = Endpoint::parse(ChainId::Monero, Transport::MoneroRpc, &url).unwrap();

    assert_eq!(probe(&network, &endpoint).await, ProbeReport::WrongNetwork);
    let requests = server.await.unwrap().unwrap();
    assert!(requests.last().unwrap().contains(r#""method":"get_info""#), "{requests:?}");
}

/// A REST service's pool sends nothing to identify it, so the probe only asks
/// whether it answers.
#[tokio::test]
async fn a_rest_service_is_only_asked_whether_it_answers() {
    let network = direct_network().unwrap();
    for (chain, transport) in
        [(ChainId::Bitcoin, Transport::FeeApi), (ChainId::Ethereum, Transport::HistoryApi)]
    {
        let (url, server) = serve(vec![("404 Not Found", "")]).await.unwrap();
        let endpoint = Endpoint::parse(chain, transport, &url).unwrap();

        assert_eq!(
            probe(&network, &endpoint).await,
            ProbeReport::Reachable { latency_ms: 0 },
            "{transport}"
        );
        let requests = server.await.unwrap().unwrap();
        let request = requests.first().unwrap();
        assert!(request.starts_with("GET / HTTP/1.1\r\n"), "{transport}: {request}");
    }
}

/// The report carries only the cause, so no parser's words can reach the interface,
/// and every way of answering badly is the same cause.
#[tokio::test]
async fn a_node_that_answers_badly_is_reported_as_a_bad_answer() {
    let network = direct_network().unwrap();
    let cases = [
        (ChainId::Ethereum, Transport::HttpJsonrpc, vec![("200 OK", "this is not json")]),
        (
            ChainId::Ethereum,
            Transport::HttpJsonrpc,
            vec![("200 OK", r#"{"jsonrpc":"2.0","id":1,"result":7}"#)],
        ),
        (ChainId::Tron, Transport::TronHttp, vec![("200 OK", "this is not json")]),
        (ChainId::Monero, Transport::MoneroRpc, vec![("200 OK", "this is not json")]),
        (
            ChainId::Monero,
            Transport::MoneroRpc,
            vec![("200 OK", "[]"), ("200 OK", r#"{"jsonrpc":"2.0","id":1,"result":"#)],
        ),
    ];
    for (index, (chain, transport, replies)) in cases.into_iter().enumerate() {
        let (url, server) = serve(replies).await.unwrap();
        let endpoint = Endpoint::parse(chain, transport, &url).unwrap();

        assert_eq!(
            probe(&network, &endpoint).await,
            ProbeReport::Unreachable { failure: ProbeFailure::BadAnswer },
            "case {index}: {chain}"
        );
        server.await.unwrap().unwrap();
    }
}

/// The registry never resolves such an endpoint, so one built by hand gets the
/// registry's own refusal.
#[tokio::test]
async fn a_transport_no_pool_on_the_chain_uses_is_refused_without_a_dial() {
    let network = direct_network().unwrap();
    let endpoint =
        Endpoint::parse(ChainId::Bitcoin, Transport::WsJsonrpc, "ws://127.0.0.1:9").unwrap();

    assert_eq!(
        probe(&network, &endpoint).await,
        ProbeReport::Unreachable { failure: ProbeFailure::NotProbed }
    );
}
