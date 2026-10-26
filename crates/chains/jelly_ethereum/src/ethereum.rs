use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Ethereum,
    slip44: 60,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/60'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    settled_confirmations: 12,
    poll_secs: 12,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::FeeCap),
    // About $1.15 at the October 2026 price of $2328. At the 0.078 gwei gas costs now, a
    // plain transfer costs about 0.0000016 ETH and a token transfer about 0.000005 ETH.
    // Update it when the price moves a lot.
    high_fee: Amount::from_base_units(500_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://etherscan.io/tx/{txid}"),
    explorer_address_url: Some("https://etherscan.io/address/{address}"),
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
    chain_id: 1,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkErc20,
        TokenId::WbtcErc20,
        TokenId::WethErc20,
        TokenId::UniErc20,
        TokenId::ShibErc20,
        TokenId::EnaErc20,
        TokenId::AaveErc20,
        TokenId::OndoErc20,
        TokenId::PepeErc20,
        TokenId::ArbErc20,
        TokenId::UsdtErc20,
        TokenId::UsdcErc20,
        TokenId::UsdsErc20,
        TokenId::DaiErc20,
        TokenId::UsdgErc20,
        TokenId::PyusdErc20,
        TokenId::XautErc20,
        TokenId::PaxgErc20,
    ],
    token_standard: "ERC-20",
    block_millis: 12_000,
    // Under half the slot time, so every block still wakes a pass.
    watch_floor: Duration::from_secs(5),
    fee_history_blocks: 20,
    // Builders skip a transaction tipping under about 0.1 gwei.
    min_priority_fee_wei: 100_000_000,
    max_priority_fee_wei: None,
    // Two and a half minutes of slots, for a short reorg and a public node that
    // trails the one that reported the tip.
    confirmations_target: 12,
    log_window_blocks: 1_000,
    // The LINK deployment, the oldest supported contract.
    backfill_floor: 4_281_611,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
    has_keyless_history: true,
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
