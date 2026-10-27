use std::collections::BTreeSet;
use std::time::Duration;

use jelly_core::{ChainId, KeyKind, TokenId, Transport};
use jelly_evm::test_util;
use jelly_net::DefaultEndpoints;
use jelly_optimism::{ENTRY, PARAMS, SPEC};

#[test]
fn the_spec_declares_op_mainnet_as_the_rest_of_the_wallet_sees_it() {
    assert_eq!(SPEC.chain, ChainId::Optimism);
    assert_eq!(SPEC.coin_name, "Ethereum", "gas is paid in ether");
    assert_eq!(SPEC.slip44, 60, "Ethereum's coin type, so a seed has one address on both");
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(SPEC.default_path, "m/44'/60'/0'/0/{index}");
    assert!(SPEC.script_kinds.is_empty());
    assert_eq!(SPEC.default_script, None);
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), None);
    assert_eq!(SPEC.settled_confirmations, 1);
    assert_eq!(SPEC.fee_rate_unit, None, "gas is not priced by size");
    assert_eq!(SPEC.uri_scheme, "ethereum", "EIP-681 names every EVM network ethereum");
    assert_eq!(SPEC.explorer_tx_url, Some("https://optimistic.etherscan.io/tx/{txid}"));
    assert_eq!(
        SPEC.explorer_address_url,
        Some("https://optimistic.etherscan.io/address/{address}")
    );
    assert_eq!(
        SPEC.transports,
        [Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi]
    );
    assert_eq!(SPEC.primary_transport, Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://optimism-rpc.publicnode.com"), Transport::WsJsonrpc);
    assert_eq!(
        (SPEC.can_bump_fee, SPEC.can_cancel, SPEC.has_subaddresses, SPEC.needs_account_key),
        (true, true, false, false),
        "a fee bump, a cancel, no subaddresses, and a scan from public data"
    );
    assert_eq!(*ENTRY.spec(), SPEC);
}

#[test]
fn the_params_carry_the_eip_155_chain_id_and_op_mainnets_figures() {
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.chain_id, 10, "0xa, as the endpoint file says");
    assert_eq!(PARAMS.symbol(), "ETH", "gas is paid in ether");
    assert_eq!(PARAMS.token_standard, "ERC-20");
    assert_eq!(PARAMS.block_millis, 2_000);
    assert_eq!(PARAMS.confirmations_target, 10);
    assert_eq!(PARAMS.indexer_lag_blocks(), 60);
    assert_eq!(PARAMS.watch_floor, Duration::from_secs(6));
    assert_eq!(PARAMS.request_timeout, Duration::from_secs(10));
    const { assert!(!PARAMS.has_native_transfer_logs, "ether moves without a log") };
    const { assert!(PARAMS.has_l1_data_fee, "the OP Stack charges L1 data apart from gas") };
    const { assert!(!PARAMS.has_keyless_history, "Etherscan serves OP Mainnet with a key") };

    assert_eq!(PARAMS.fee_history_blocks, 30);
    assert_eq!(PARAMS.min_priority_fee_wei, 1_000_000, "op-geth's suggested 0.001 gwei");
    assert_eq!(PARAMS.backfill_floor, 0, "WETH's first transfers sit near genesis");
    assert_eq!(PARAMS.log_window_blocks, 10_000, "the OP Labs endpoint's cap");
}

/// Only each issuer's own deployment of a real world asset, never a bridged copy, and
/// otherwise coins the wallet carries on another network or OP itself.
#[test]
fn the_tokens_are_issuer_deployments_with_no_bridged_real_world_asset() {
    assert_eq!(
        PARAMS.tokens,
        [
            TokenId::LinkOptimism,
            TokenId::WethOptimism,
            TokenId::OpOptimism,
            TokenId::UsdtOptimism,
            TokenId::UsdcOptimism,
            TokenId::UsdsOptimism,
        ]
    );
    for token in PARAMS.tokens {
        assert_eq!(token.chain(), ChainId::Optimism, "{token:?}");
    }
    let bridged = [
        "0x7F5c764cBc14f9669B88837ca1490cCa17c31607",
        "0x94b008aA00579c1307B0EF2c499aD98a8ce58e58",
    ];
    for token in PARAMS.tokens {
        assert!(!bridged.contains(&token.contract()), "{token:?} is the bridged USDC.e or USDT");
    }
    assert_eq!(TokenId::OpOptimism.contract(), "0x4200000000000000000000000000000000000042");
    assert_eq!(TokenId::WethOptimism.contract(), "0x4200000000000000000000000000000000000006");
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
    assert_eq!(PARAMS, test_util::OPTIMISM);
}
