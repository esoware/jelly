use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Bsc,
    coin_name: "BNB",
    coin_chain: ChainId::Bsc,
    // Ethereum's coin type rather than BNB's own, so a seed has the same address on
    // both networks.
    slip44: 60,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/60'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    // Fast finality finalizes a block once the two after it carry the validators'
    // votes, about a second and a half.
    settled_confirmations: 3,
    poll_secs: 8,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::GasPrice),
    // About $1.99 at the October 2026 price of $737. At the 0.05 gwei gas costs now, a plain
    // transfer costs about 0.000001 BNB and a token transfer about 0.0000033 BNB.
    high_fee: Amount::from_base_units(2_700_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://bscscan.com/tx/{txid}"),
    explorer_address_url: Some("https://bscscan.com/address/{address}"),
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
    chain_id: 56,
    request_timeout: Duration::from_secs(10),
    // Binance's own copies of LINK and ether, which its withdrawals send and the
    // network's exchanges trade, rather than the ERC-677 LINK Chainlink's PegSwap
    // issues for paying its oracles. ENA is Ethena's own deployment.
    tokens: &[
        TokenId::LinkBep20,
        TokenId::WethBep20,
        TokenId::EnaBep20,
        TokenId::UsdtBep20,
        TokenId::UsdcBep20,
        TokenId::XautBep20,
    ],
    token_standard: "BEP-20",
    // One block every 0.45 seconds since the Fermi hard fork of 14 January 2026.
    block_millis: 450,
    // A pass per block on a sub-second chain is wasteful, so a burst of heads wakes
    // one pass.
    watch_floor: Duration::from_secs(6),
    // More blocks than Ethereum samples, because they come many times as often.
    fee_history_blocks: 100,
    // The base fee is near zero, so the tip is what gets a transaction included.
    min_priority_fee_wei: 50_000_000,
    max_priority_fee_wei: None,
    // Past finality, to leave room for a public node that trails the one that
    // reported the tip.
    confirmations_target: 15,
    log_window_blocks: 1_000,
    // The USDT deployment, the oldest supported contract.
    backfill_floor: 176_416,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
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
