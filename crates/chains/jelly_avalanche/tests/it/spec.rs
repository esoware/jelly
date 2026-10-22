use std::collections::BTreeSet;
use std::time::Duration;

use jelly_avalanche::{ENTRY, PARAMS, SPEC};
use jelly_core::{ChainId, KeyKind, TokenId, Transport};
use jelly_evm::test_util;
use jelly_net::DefaultEndpoints;

#[test]
fn the_spec_declares_the_c_chain_as_the_rest_of_the_wallet_sees_it() {
    assert_eq!(SPEC.chain, ChainId::Avalanche);
    assert_eq!(SPEC.slip44, 60, "Ethereum's coin type, so a seed has one address on both");
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(SPEC.default_path, "m/44'/60'/0'/0/{index}");
    assert!(SPEC.script_kinds.is_empty());
    assert_eq!(SPEC.default_script, None);
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), None);
    assert_eq!(SPEC.settled_confirmations, 1);
    assert_eq!(SPEC.fee_rate_unit, None, "gas is not priced by size");
    assert_eq!(SPEC.uri_scheme, "ethereum", "EIP-681 names every EVM network ethereum");
    assert_eq!(SPEC.explorer_tx_url, Some("https://snowtrace.io/tx/{txid}"));
    assert_eq!(SPEC.explorer_address_url, Some("https://snowtrace.io/address/{address}"));
    assert_eq!(
        SPEC.transports,
        [Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi]
    );
    assert_eq!(SPEC.primary_transport, Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://api.avax.network/ext/bc/C/ws"), Transport::WsJsonrpc);
    assert_eq!(
        (SPEC.can_bump_fee, SPEC.can_cancel, SPEC.has_subaddresses, SPEC.needs_account_key),
        (true, true, false, false),
        "a fee bump, a cancel, no subaddresses, and a scan from public data"
    );
    assert_eq!(*ENTRY.spec(), SPEC);
}

#[test]
fn the_params_carry_the_eip_155_chain_id_and_the_c_chains_figures() {
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.chain_id, 43_114, "0xa86a, as the endpoint file says");
    assert_eq!(PARAMS.symbol(), "AVAX");
    assert_eq!(PARAMS.token_standard, "ERC-20");
    assert_eq!(PARAMS.block_millis, 1_000);
    assert_eq!(PARAMS.confirmations_target, 10);
    assert_eq!(PARAMS.indexer_lag_blocks(), 120);
    assert_eq!(PARAMS.watch_floor, Duration::from_secs(6));
    assert_eq!(PARAMS.request_timeout, Duration::from_secs(10));
    const { assert!(!PARAMS.has_native_transfer_logs, "AVAX moves without a log") };
    const { assert!(!PARAMS.has_l1_data_fee, "every cost is gas") };
    const { assert!(PARAMS.has_keyless_history, "Routescan answers without a key") };

    assert_eq!(PARAMS.fee_history_blocks, 60);
    assert_eq!(PARAMS.min_priority_fee_wei, 0, "a tip of nothing is included");
    assert_eq!(PARAMS.max_priority_fee_wei, Some(150), "the node's own oracle suggests no more");
    assert_eq!(PARAMS.backfill_floor, 2_749_890, "the LINK.e deployment");
    assert_eq!(PARAMS.log_window_blocks, 10_000);
}

/// Only each issuer's own deployment of a real world asset, never a bridged copy,
/// and only coins the wallet already carries on another network.
#[test]
fn the_tokens_are_coins_carried_elsewhere_with_no_bridged_real_world_asset() {
    assert_eq!(
        PARAMS.tokens,
        [
            TokenId::LinkAvalanche,
            TokenId::WethAvalanche,
            TokenId::UsdtAvalanche,
            TokenId::UsdcAvalanche,
            TokenId::UsdsAvalanche,
            TokenId::XautAvalanche
        ]
    );
    for token in PARAMS.tokens {
        assert_eq!(token.chain(), ChainId::Avalanche, "{token:?}");
        assert!(
            TokenId::ALL.iter().any(
                |other| other.chain() != ChainId::Avalanche && other.symbol() == token.symbol()
            ),
            "{token:?} is carried on another network too"
        );
    }
    let bridged_or_wrapped = [
        "0xc7198437980c041c805A1EDcbA50c1Ce5db95118",
        "0xA7D7079b0FEaD91F3e65f86E8915Cb59c1a4C664",
        "0xd586E7F844cEa2F87f50152665BCbc2C279D8d70",
        "0x142cdc44890978B506e745bB3Bd11607B7f7faEf",
    ];
    for token in PARAMS.tokens {
        assert!(!bridged_or_wrapped.contains(&token.contract()), "{token:?} is not the issuer's");
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

/// Etherscan serves the C-Chain to paid plans only, so a saved key must never be
/// what history depends on, and must never be sent to another service.
#[test]
fn every_shipped_history_service_answers_without_a_key() {
    let file: DefaultEndpoints = serde_json::from_str(ENTRY.default_endpoints()).unwrap();
    let services: Vec<&str> = file
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.transport == Transport::HistoryApi)
        .map(|endpoint| endpoint.url.as_str())
        .collect();
    assert_eq!(
        services,
        [
            "https://api.routescan.io/v2/network/mainnet/evm/43114/etherscan/api",
            "https://api.snowtrace.io/api"
        ]
    );
}

/// The framework's own unit tests run on a copy of these params.
#[test]
fn the_params_match_the_frameworks_test_copy() {
    assert_eq!(PARAMS, test_util::AVALANCHE);
}
