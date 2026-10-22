use std::collections::BTreeSet;
use std::time::Duration;

use jelly_core::{ChainId, KeyKind, TokenId, Transport};
use jelly_evm::test_util;
use jelly_net::DefaultEndpoints;
use jelly_polygon::{ENTRY, PARAMS, SPEC};

#[test]
fn the_spec_declares_polygon_as_the_rest_of_the_wallet_sees_it() {
    assert_eq!(SPEC.chain, ChainId::Polygon);
    assert_eq!(SPEC.slip44, 60, "Ethereum's coin type, so a seed has one address on both");
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(SPEC.default_path, "m/44'/60'/0'/0/{index}");
    assert!(SPEC.script_kinds.is_empty());
    assert_eq!(SPEC.default_script, None);
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), None);
    assert_eq!(SPEC.settled_confirmations, 5);
    assert_eq!(SPEC.fee_rate_unit, None, "gas is not priced by size");
    assert_eq!(SPEC.uri_scheme, "ethereum", "EIP-681 names every EVM network ethereum");
    assert_eq!(SPEC.explorer_tx_url, Some("https://polygonscan.com/tx/{txid}"));
    assert_eq!(SPEC.explorer_address_url, Some("https://polygonscan.com/address/{address}"));
    assert_eq!(
        SPEC.transports,
        [Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi]
    );
    assert_eq!(SPEC.primary_transport, Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://polygon-bor-rpc.publicnode.com"), Transport::WsJsonrpc);
    assert_eq!(
        (SPEC.can_bump_fee, SPEC.can_cancel, SPEC.has_subaddresses, SPEC.needs_account_key),
        (true, true, false, false),
        "a fee bump, a cancel, no subaddresses, and a scan from public data"
    );
    assert_eq!(*ENTRY.spec(), SPEC);
}

#[test]
fn the_params_carry_the_eip_155_chain_id_and_polygons_figures() {
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.chain_id, 137, "0x89, as the endpoint file says");
    assert_eq!(PARAMS.symbol(), "POL", "the coin MATIC became, never the string MATIC");
    assert_eq!(PARAMS.token_standard, "ERC-20");
    assert_eq!(PARAMS.block_millis, 1_500);
    assert_eq!(PARAMS.confirmations_target, 16);
    assert_eq!(PARAMS.indexer_lag_blocks(), 80);
    assert_eq!(PARAMS.watch_floor, Duration::from_secs(6));
    assert_eq!(PARAMS.request_timeout, Duration::from_secs(10));
    const { assert!(PARAMS.has_native_transfer_logs, "Bor logs every native transfer") };

    assert_eq!(PARAMS.fee_history_blocks, 40);
    assert_eq!(PARAMS.min_priority_fee_wei, 25_000_000_000, "Bor's 25 gwei floor");
    assert_eq!(PARAMS.backfill_floor, 0, "native history reaches genesis");
    assert_eq!(PARAMS.log_window_blocks, 10_000);
}

/// Only each issuer's own deployment of a real world asset, never a bridged copy,
/// and only coins the wallet already carries on another network.
#[test]
fn the_tokens_are_coins_carried_elsewhere_with_no_bridged_real_world_asset() {
    assert_eq!(
        PARAMS.tokens,
        [
            TokenId::LinkPolygon,
            TokenId::WethPolygon,
            TokenId::UsdtPolygon,
            TokenId::UsdcPolygon,
            TokenId::PyusdPolygon,
            TokenId::XautPolygon
        ]
    );
    for token in PARAMS.tokens {
        assert_eq!(token.chain(), ChainId::Polygon, "{token:?}");
        assert!(
            TokenId::ALL
                .iter()
                .any(|other| other.chain() != ChainId::Polygon && other.symbol() == token.symbol()),
            "{token:?} is carried on another network too"
        );
    }
    let bridged = [
        "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174",
        "0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063",
        "0x553d3D295e0f695B9228246232eDF400ed3560B5",
    ];
    for token in PARAMS.tokens {
        assert!(!bridged.contains(&token.contract()), "{token:?} is a bridged copy");
    }
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

/// The framework's own unit tests run on a copy of these params.
#[test]
fn the_params_match_the_frameworks_test_copy() {
    assert_eq!(PARAMS, test_util::POLYGON);
}
