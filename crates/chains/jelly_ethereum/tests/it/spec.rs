use std::collections::BTreeSet;
use std::time::Duration;

use jelly_core::{ChainId, KeyKind, Transport};
use jelly_ethereum::{ENTRY, PARAMS, SPEC};
use jelly_evm::test_util;
use jelly_net::DefaultEndpoints;

#[test]
fn the_spec_declares_ethereum_as_the_rest_of_the_wallet_sees_it() {
    assert_eq!(SPEC.chain, ChainId::Ethereum);
    assert_eq!(SPEC.slip44, 60);
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(SPEC.default_path, "m/44'/60'/0'/0/{index}");
    assert!(SPEC.script_kinds.is_empty());
    assert_eq!(SPEC.default_script, None);
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), None);
    assert_eq!(SPEC.settled_confirmations, 12);
    assert_eq!(SPEC.fee_rate_unit, None, "gas is not priced by size");
    assert_eq!(SPEC.uri_scheme, "ethereum");
    assert_eq!(SPEC.explorer_tx_url, Some("https://etherscan.io/tx/{txid}"));
    assert_eq!(SPEC.explorer_address_url, Some("https://etherscan.io/address/{address}"));
    assert_eq!(
        SPEC.transports,
        [Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi]
    );
    assert_eq!(SPEC.primary_transport, Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://ethereum-rpc.publicnode.com"), Transport::WsJsonrpc);
    assert_eq!(
        (SPEC.can_bump_fee, SPEC.can_cancel, SPEC.has_subaddresses, SPEC.needs_account_key),
        (true, true, false, false),
        "a fee bump, a cancel, no subaddresses, and a scan from public data"
    );
    assert_eq!(*ENTRY.spec(), SPEC);
}

#[test]
fn the_params_carry_the_eip_155_chain_id_and_ethereums_figures() {
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.chain_id, 1);
    assert_eq!(PARAMS.symbol(), "ETH");
    assert_eq!(PARAMS.token_standard, "ERC-20");
    assert_eq!(PARAMS.block_millis, 12_000);
    assert_eq!(PARAMS.confirmations_target, 12);
    assert_eq!(PARAMS.indexer_lag_blocks(), 10);
    assert_eq!(PARAMS.watch_floor, Duration::from_secs(5));
    assert_eq!(PARAMS.request_timeout, Duration::from_secs(10));
    assert_eq!(PARAMS.tokens.len(), 18);
    for token in PARAMS.tokens {
        assert_eq!(token.chain(), ChainId::Ethereum, "{token:?}");
    }
    const { assert!(PARAMS.has_keyless_history, "Routescan answers without a key") };

    assert_eq!(PARAMS.fee_history_blocks, 20);
    assert_eq!(PARAMS.min_priority_fee_wei, 100_000_000);
    assert_eq!(PARAMS.backfill_floor, 4_281_611);
    assert_eq!(PARAMS.log_window_blocks, 1_000);
}

#[test]
fn the_shipped_endpoints_use_exactly_the_specs_transports() {
    let file: DefaultEndpoints = serde_json::from_str(ENTRY.default_endpoints()).unwrap();
    assert_eq!(file.chain, SPEC.chain);
    let used: BTreeSet<Transport> =
        file.endpoints.iter().map(|endpoint| endpoint.transport).collect();
    let declared: BTreeSet<Transport> = SPEC.transports.iter().copied().collect();
    assert_eq!(used, declared);
}

/// A keyless service is sent no key, so a saved Etherscan key must never be
/// what history depends on, and must never reach another service.
#[test]
fn every_shipped_history_service_answers_without_a_key() {
    let file: DefaultEndpoints = serde_json::from_str(ENTRY.default_endpoints()).unwrap();
    let services: Vec<&str> = file
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.transport == Transport::HistoryApi)
        .map(|endpoint| endpoint.url.as_str())
        .collect();
    assert_eq!(services, ["https://api.routescan.io/v2/network/mainnet/evm/1/etherscan/api"]);
}

/// The framework's own unit tests run on a copy of these params.
#[test]
fn the_params_match_the_frameworks_test_copy() {
    assert_eq!(PARAMS, test_util::ETHEREUM);
}
