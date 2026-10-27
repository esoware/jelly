use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Optimism,
    coin_name: "Ethereum",
    // Ethereum's coin type, as MetaMask and Trust Wallet derive, so a seed has the
    // same address on both networks.
    slip44: 60,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/60'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    // The sequencer's blocks stand unless it fails to post a batch to Ethereum.
    settled_confirmations: 1,
    poll_secs: 8,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::FeeCap),
    // About $2.10 at the October 2026 price of $2328. Gas costs about 0.001 gwei now, so
    // 100000 gas costs 0.0000001 ETH before the L1 data fee.
    high_fee: Amount::from_base_units(900_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://optimistic.etherscan.io/tx/{txid}"),
    explorer_address_url: Some("https://optimistic.etherscan.io/address/{address}"),
    transports: &[Transport::HttpJsonrpc, Transport::WsJsonrpc, Transport::HistoryApi],
    primary_transport: Transport::HttpJsonrpc,
    can_bump_fee: true,
    can_cancel: true,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const PARAMS: EvmParams = EvmParams {
    chain: SPEC.chain,
    chain_id: 10,
    request_timeout: Duration::from_secs(10),
    // Each issuer's own deployment: Tether's USDT0, Circle's USDC, and Sky's USDS. The
    // bridged USDC.e and the standard bridge's USDT are left out. LINK is Chainlink's,
    // WETH the OP Stack's predeploy, and OP is issued here.
    tokens: &[
        TokenId::LinkOptimism,
        TokenId::WethOptimism,
        TokenId::OpOptimism,
        TokenId::UsdtOptimism,
        TokenId::UsdcOptimism,
        TokenId::UsdsOptimism,
    ],
    token_standard: "ERC-20",
    block_millis: 2_000,
    // A pass per block is wasteful at this pace, so a burst of heads wakes one pass.
    watch_floor: Duration::from_secs(6),
    // A minute of blocks.
    fee_history_blocks: 30,
    // The tip op-geth suggests when blocks have room, 0.001 gwei. Senders here tip
    // about 20 wei, so every level's sampled tip rises to this floor.
    min_priority_fee_wei: 1_000_000,
    max_priority_fee_wei: None,
    // Twenty seconds of blocks, for a public node that trails the one that reported
    // the tip.
    confirmations_target: 10,
    // The OP Labs endpoint caps eth_getLogs at this width, and the other shipped
    // endpoint that serves old logs takes it.
    log_window_blocks: 10_000,
    // WETH logged its first transfers in block 2179, and OP and LINK moved long before
    // the Bedrock upgrade at block 105235063, so a floor at Bedrock would lose history.
    // The shipped endpoints that serve old logs reach back to genesis.
    backfill_floor: 0,
    has_native_transfer_logs: false,
    has_l1_data_fee: true,
    has_keyless_history: false,
};

pub const ENTRY: &dyn ChainEntry = &EvmEntry {
    spec: &SPEC,
    params: &PARAMS,
    codec: &EvmCodec::new(SPEC.chain),
    default_endpoints: include_str!("../endpoints.json"),
};

pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    jelly_evm::build(&SPEC, &PARAMS, network, request).await
}

pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    jelly_evm::probe(&PARAMS, network, endpoint).await
}
