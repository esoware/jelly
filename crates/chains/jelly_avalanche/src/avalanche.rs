use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Avalanche,
    // Ethereum's coin type rather than Avalanche's own 9000, which only the X-Chain
    // and P-Chain use. Core, MetaMask and Trust Wallet derive the C-Chain here, so a
    // seed has the same address as on Ethereum.
    slip44: 60,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/60'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    // An accepted block never reorganizes, and a node has a receipt only once the
    // block has run, so the outcome it reports is the one that stands.
    settled_confirmations: 1,
    poll_secs: 8,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::FeeCap),
    // A token transfer to a new holder, about 65000 gas, at about 15000 nAVAX, three times
    // the 5000 nAVAX of the December 2023 inscription surge.
    high_fee: Amount::from_base_units(1_000_000_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://snowtrace.io/tx/{txid}"),
    explorer_address_url: Some("https://snowtrace.io/address/{address}"),
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
    chain_id: 43_114,
    request_timeout: Duration::from_secs(10),
    // Each issuer's own deployment: Tether's USDt and XAUt0, Circle's USDC, and
    // Sky's USDS. The bridged USDT.e, USDC.e and DAI.e are left out, and so is
    // PYUSD0, which LayerZero rather than Paxos issues. LINK and WETH have no
    // issuer's deployment here, so they are the Avalanche Bridge's LINK.e and
    // WETH.e, which exchanges and wallets use.
    tokens: &[
        TokenId::LinkAvalanche,
        TokenId::WethAvalanche,
        TokenId::UsdtAvalanche,
        TokenId::UsdcAvalanche,
        TokenId::UsdsAvalanche,
        TokenId::XautAvalanche,
    ],
    token_standard: "ERC-20",
    // About one block a second since Granite made the minimum block delay dynamic.
    block_millis: 1_000,
    // A pass per block is wasteful at this pace, so a burst of heads wakes one pass.
    watch_floor: Duration::from_secs(6),
    // A minute of blocks.
    fee_history_blocks: 60,
    // Nodes take a transaction that tips nothing.
    min_priority_fee_wei: 0,
    // The most the node's own fee oracle suggests. Tips are burned and only order
    // a block, and blocks are rarely full, so the few senders who tip gwei would
    // otherwise nearly double every level's fee.
    max_priority_fee_wei: Some(150),
    // Execution settles at least five seconds after a block runs, so this covers
    // settlement and a public node that trails the one that reported the tip.
    confirmations_target: 10,
    // Every shipped endpoint that serves old logs takes a window this wide.
    log_window_blocks: 10_000,
    // The LINK.e deployment, the oldest supported contract.
    backfill_floor: 2_749_890,
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
