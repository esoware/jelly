use std::collections::BTreeSet;
use std::time::Duration;

use jelly_bsc::{ENTRY, PARAMS, SPEC};
use jelly_core::{ChainId, KeyKind, TokenId, Transport};
use jelly_evm::test_util;
use jelly_net::DefaultEndpoints;

#[test]
fn the_spec_declares_bnb_smart_chain_as_the_rest_of_the_wallet_sees_it() {
    assert_eq!(SPEC.chain, ChainId::Bsc);
    assert_eq!(SPEC.slip44, 60, "Ethereum's coin type, so a seed has one address on both");
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(SPEC.default_path, "m/44'/60'/0'/0/{index}");
    assert!(SPEC.script_kinds.is_empty());
    assert_eq!(SPEC.default_script, None);
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), None);
    assert_eq!(SPEC.settled_confirmations, 3);
    assert_eq!(SPEC.fee_rate_unit, None, "gas is not priced by size");
    assert_eq!(SPEC.uri_scheme, "ethereum", "EIP-681 names every EVM network ethereum");
    assert_eq!(SPEC.explorer_tx_url, Some("https://bscscan.com/tx/{txid}"));
    assert_eq!(SPEC.explorer_address_url, Some("https://bscscan.com/address/{address}"));
    assert_eq!(
        SPEC.transports,
        [Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi]
    );
    assert_eq!(SPEC.primary_transport, Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://bsc-rpc.publicnode.com"), Transport::WsJsonrpc);
    assert_eq!(
        (SPEC.can_bump_fee, SPEC.can_cancel, SPEC.has_subaddresses, SPEC.needs_account_key),
        (true, true, false, false),
        "a fee bump, a cancel, no subaddresses, and a scan from public data"
    );
    assert_eq!(*ENTRY.spec(), SPEC);
}

#[test]
fn the_params_carry_the_eip_155_chain_id_and_bnb_smart_chains_figures() {
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.chain_id, 56, "0x38, as the endpoint file says");
    assert_eq!(PARAMS.symbol(), "BNB", "the ticker, never the string BSC");
    assert_eq!(PARAMS.token_standard, "BEP-20");
    assert_eq!(PARAMS.block_millis, 450);
    assert_eq!(PARAMS.confirmations_target, 15);
    assert_eq!(PARAMS.watch_floor, Duration::from_secs(6));
    assert_eq!(PARAMS.request_timeout, Duration::from_secs(10));
    assert_eq!(
        PARAMS.tokens,
        [
            TokenId::LinkBep20,
            TokenId::WethBep20,
            TokenId::EnaBep20,
            TokenId::UsdtBep20,
            TokenId::UsdcBep20,
            TokenId::XautBep20
        ]
    );
    for token in PARAMS.tokens {
        assert_eq!(token.chain(), ChainId::Bsc, "{token:?}");
    }

    assert_eq!(PARAMS.fee_history_blocks, 100);
    assert_eq!(PARAMS.min_priority_fee_wei, 50_000_000);
    assert_eq!(PARAMS.backfill_floor, 176_416);
    assert_eq!(PARAMS.log_window_blocks, 1_000);
}

/// Fermi has produced a block every 0.45 seconds since 14 January 2026, after
/// Maxwell's 0.75 seconds.
#[test]
fn the_indexer_lag_lasts_two_minutes_of_bsc_blocks() {
    const BSC_BLOCK_MILLIS: u64 = 450;
    let seconds = |blocks: u64| {
        blocks.saturating_mul(BSC_BLOCK_MILLIS).checked_div(1_000).unwrap_or_default()
    };
    let lag_secs = seconds(PARAMS.indexer_lag_blocks());
    assert!(
        lag_secs >= 120,
        "an indexer is promised two minutes of lag on every network, but on BNB Smart Chain \
         the count lasts {lag_secs} seconds"
    );
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
    assert_eq!(PARAMS, test_util::BSC);
}
