use jelly_chains::chains;
use jelly_core::{ChainId, EndpointOverride, Transport};
use jelly_net::{
    Endpoint, EndpointOrigin, EndpointRegistry, NetError, PlaintextPolicy, UrlRejection,
};

use crate::common::registry;

const CHAIN: ChainId = ChainId::Ethereum;
const MINE: &str = "https://my-node.example";

fn curated_url(registry: &EndpointRegistry, chain: ChainId) -> Result<String, NetError> {
    registry
        .defaults(chain)?
        .endpoints
        .first()
        .map(|entry| entry.url.clone())
        .ok_or(NetError::NoEndpoints { chain })
}

fn overrides(chain: ChainId, rows: Vec<EndpointOverride>) -> Vec<(ChainId, EndpointOverride)> {
    rows.into_iter().map(|row| (chain, row)).collect()
}

#[test]
fn with_no_overrides_the_curated_list_is_the_list() {
    let registry = registry();
    for chain in ChainId::ALL {
        let curated = &registry.defaults(chain).unwrap().endpoints;
        let configured = registry.configured(chain).unwrap();
        assert_eq!(configured.len(), curated.len(), "{chain:?}");
        for (row, entry) in configured.iter().zip(curated) {
            assert_eq!(row.endpoint.url(), entry.url);
            assert_eq!(row.endpoint.origin(), EndpointOrigin::Default);
            assert!(row.is_enabled);
        }
    }
}

#[test]
fn a_websocket_address_resolves_to_its_own_transport_beside_the_http_row() {
    let registry = registry();
    let curated = curated_url(&registry, CHAIN).unwrap();
    let ws_spelling = curated.replacen("https://", "wss://", 1);

    let resolved = registry.resolve(CHAIN, &ws_spelling, PlaintextPolicy::LoopbackOnly).unwrap();
    assert_eq!(resolved.transport(), Transport::WsJsonrpc);
    assert_eq!(resolved.url(), ws_spelling, "not folded into the https row");

    let plain = registry.resolve(CHAIN, &curated, PlaintextPolicy::LoopbackOnly).unwrap();
    assert_eq!(plain.transport(), Transport::HttpJsonrpc);

    // A stored websocket override row keeps its transport on the way back out.
    registry.load_overrides(overrides(CHAIN, vec![EndpointOverride::new(ws_spelling.clone())]));
    let listed = registry.configured(CHAIN).unwrap();
    let row = listed.iter().find(|row| row.endpoint.url() == ws_spelling).unwrap();
    assert_eq!(row.endpoint.transport(), Transport::WsJsonrpc);
}

#[test]
fn an_override_on_a_curated_url_adjusts_it_instead_of_duplicating_it() {
    let registry = registry();
    let url = curated_url(&registry, CHAIN).unwrap();
    let before = registry.configured(CHAIN).unwrap().len();

    registry.load_overrides(overrides(
        CHAIN,
        vec![EndpointOverride { is_enabled: false, ..EndpointOverride::new(url.clone()) }],
    ));

    let configured = registry.configured(CHAIN).unwrap();
    assert_eq!(configured.len(), before, "no second row for the same address");
    let row = configured.iter().find(|row| row.endpoint.url() == url).unwrap();
    assert!(!row.is_enabled);

    let walked = registry.enabled(CHAIN, Transport::HttpJsonrpc).unwrap();
    assert!(walked.iter().all(|endpoint| endpoint.url() != url), "a disabled row is not walked");
}

#[test]
fn pinning_a_curated_default_moves_it_to_the_front() {
    let registry = registry();
    let curated = &registry.defaults(CHAIN).unwrap().endpoints;
    let last = curated.last().map(|entry| entry.url.clone()).unwrap();

    registry.load_overrides(overrides(
        CHAIN,
        vec![EndpointOverride { is_pinned: true, ..EndpointOverride::new(last.clone()) }],
    ));

    let configured = registry.configured(CHAIN).unwrap();
    let first = configured.first().unwrap();
    assert_eq!(first.endpoint.url(), last);
    assert_eq!(first.endpoint.origin(), EndpointOrigin::Pinned);
}

#[test]
fn a_user_endpoint_sorts_ahead_of_the_curated_defaults() {
    let registry = registry();
    registry.load_overrides(overrides(CHAIN, vec![EndpointOverride::new(MINE)]));

    let configured = registry.configured(CHAIN).unwrap();
    let first = configured.first().unwrap();
    assert_eq!(first.endpoint.url(), MINE);
    assert_eq!(first.endpoint.origin(), EndpointOrigin::User);
    assert_eq!(first.endpoint.transport(), chains().spec(CHAIN).primary_transport);
}

#[test]
fn a_pool_is_given_only_the_enabled_endpoints_of_its_own_transport() {
    let registry = registry();
    let jsonrpc = registry.enabled(CHAIN, Transport::HttpJsonrpc).unwrap();
    let sockets = registry.enabled(CHAIN, Transport::WsJsonrpc).unwrap();
    let history = registry.enabled(CHAIN, Transport::HistoryApi).unwrap();

    assert!(!jsonrpc.is_empty());
    assert!(!sockets.is_empty());
    assert!(!history.is_empty());
    assert!(jsonrpc.iter().all(|endpoint| endpoint.transport() == Transport::HttpJsonrpc));
    assert!(sockets.iter().all(|endpoint| endpoint.transport() == Transport::WsJsonrpc));
    assert!(history.iter().all(|endpoint| endpoint.transport() == Transport::HistoryApi));
    assert_eq!(
        registry.configured(CHAIN).unwrap().len(),
        jsonrpc.len().saturating_add(sockets.len()).saturating_add(history.len())
    );

    // Configured order, which the pool falls back on to break ties in health.
    registry.load_overrides(overrides(
        CHAIN,
        vec![EndpointOverride { is_pinned: true, ..EndpointOverride::new(MINE) }],
    ));
    let walked = registry.enabled(CHAIN, Transport::HttpJsonrpc).unwrap();
    assert_eq!(walked.first().map(Endpoint::url), Some(MINE));
}

#[test]
fn a_spelling_of_a_configured_endpoint_resolves_to_that_endpoint() {
    let registry = registry();
    let url = curated_url(&registry, ChainId::Bitcoin).unwrap();

    let resolved = registry
        .resolve(ChainId::Bitcoin, &url.to_uppercase(), PlaintextPolicy::LoopbackOnly)
        .unwrap();

    assert_eq!(resolved.url(), url);
    assert_eq!(resolved.transport(), Transport::ElectrumSsl);
    assert_eq!(resolved.origin(), EndpointOrigin::Default);
}

#[test]
fn an_unknown_address_resolves_as_a_canonical_user_endpoint_on_the_primary_transport() {
    let registry = registry();
    let resolved =
        registry.resolve(CHAIN, "HTTPS://My-Node.example/", PlaintextPolicy::LoopbackOnly).unwrap();

    assert_eq!(resolved.url(), MINE);
    assert_eq!(resolved.origin(), EndpointOrigin::User);
    assert_eq!(resolved.transport(), chains().spec(CHAIN).primary_transport);
}

#[test]
fn plaintext_outside_loopback_resolves_only_when_allowed() {
    let registry = registry();
    let refused = registry
        .resolve(CHAIN, "http://not-loopback.example", PlaintextPolicy::LoopbackOnly)
        .expect_err("plaintext outside loopback");
    assert!(matches!(refused, NetError::InvalidUrl { .. }));

    registry.resolve(CHAIN, "http://not-loopback.example", PlaintextPolicy::Allowed).unwrap();
}

#[test]
fn find_names_only_configured_endpoints_disabled_ones_included() {
    let registry = registry();
    let curated = curated_url(&registry, CHAIN).unwrap();
    registry.load_overrides(overrides(
        CHAIN,
        vec![EndpointOverride { is_enabled: false, ..EndpointOverride::new(MINE) }],
    ));

    let found = registry.find(CHAIN, &curated.to_uppercase()).unwrap();
    assert_eq!(found.map(|endpoint| endpoint.url().to_owned()), Some(curated));

    let disabled = registry.find(CHAIN, MINE).unwrap();
    assert_eq!(disabled.map(|endpoint| endpoint.origin()), Some(EndpointOrigin::User));

    for stranger in ["https://stranger.example", "http://stranger.example", "not a url"] {
        let missing = registry.find(CHAIN, stranger).unwrap();
        assert!(missing.is_none(), "{stranger} is not configured");
    }
}

#[test]
fn a_malformed_override_does_not_hide_valid_or_disabled_endpoints() {
    let registry = registry();
    let curated = curated_url(&registry, CHAIN).unwrap();
    registry.load_overrides(overrides(
        CHAIN,
        vec![
            EndpointOverride::new("https://"),
            EndpointOverride::new(MINE),
            EndpointOverride { is_enabled: false, ..EndpointOverride::new(curated.clone()) },
        ],
    ));
    let rows = registry.configured(CHAIN).unwrap();
    assert!(rows.iter().any(|row| row.endpoint.url() == MINE && row.is_enabled));
    assert!(rows.iter().any(|row| row.endpoint.url() == curated && !row.is_enabled));
    let enabled = registry.enabled(CHAIN, Transport::HttpJsonrpc).unwrap();
    assert!(enabled.iter().any(|endpoint| endpoint.url() == MINE));
    assert!(enabled.iter().all(|endpoint| endpoint.url() != curated));
    assert!(registry.find(CHAIN, MINE).unwrap().is_some());
}

#[test]
fn load_overrides_replaces_every_chain_at_once() {
    let registry = registry();
    let electrum = "ssl://x.example:50002";
    registry.load_overrides(overrides(ChainId::Bitcoin, vec![EndpointOverride::new(electrum)]));
    let listed = |chain: ChainId, url: &str| {
        registry.configured(chain).unwrap().iter().any(|row| row.endpoint.url() == url)
    };
    assert!(listed(ChainId::Bitcoin, electrum));

    registry.load_overrides(overrides(CHAIN, vec![EndpointOverride::new(MINE)]));
    assert!(!listed(ChainId::Bitcoin, electrum));
    assert!(listed(CHAIN, MINE));
}

#[test]
fn an_address_on_a_transport_the_chain_never_uses_is_refused() {
    let registry = registry();
    for chain in [ChainId::Bitcoin, ChainId::Litecoin, ChainId::Tron, ChainId::Monero] {
        let refused = registry
            .resolve(chain, "wss://socket.example", PlaintextPolicy::LoopbackOnly)
            .expect_err("no pool on this chain opens a socket");
        assert!(
            matches!(refused, NetError::InvalidUrl { reason: UrlRejection::UnusedTransport }),
            "{chain:?}: {refused:?}"
        );
    }
    for chain in [ChainId::Ethereum, ChainId::Bsc, ChainId::Solana] {
        let resolved =
            registry.resolve(chain, "wss://socket.example", PlaintextPolicy::LoopbackOnly).unwrap();
        assert_eq!(resolved.transport(), Transport::WsJsonrpc, "{chain:?}");
    }
}

/// A stored row the registry can't use never fails the load. It's skipped, and the rest of
/// the chain's rows still load.
#[test]
fn a_stored_row_on_a_transport_the_chain_never_uses_is_skipped_when_overrides_load() {
    let registry = registry();
    let electrum = "ssl://x.example:50002";
    let socket = "wss://socket.example";
    registry.load_overrides([
        (ChainId::Bitcoin, EndpointOverride::new(socket)),
        (ChainId::Bitcoin, EndpointOverride::new(electrum)),
        (CHAIN, EndpointOverride::new(socket)),
    ]);

    let configured = registry.configured(ChainId::Bitcoin).unwrap();
    assert!(configured.iter().any(|row| row.endpoint.url() == electrum));
    assert!(configured.iter().all(|row| row.endpoint.transport() != Transport::WsJsonrpc));
    let ethereum = registry.configured(CHAIN).unwrap();
    assert!(ethereum.iter().any(|row| row.endpoint.url() == socket), "ethereum does open sockets");
}
