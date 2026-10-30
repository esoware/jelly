use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Polygon,
    // Ethereum's coin type, not Polygon's own 966, as in MetaMask and Trust Wallet, so a
    // seed has the same address on both networks.
    slip44: 60,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/60'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    // Milestones finalize a block within about five seconds.
    settled_confirmations: 5,
    poll_secs: 8,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::FeeCap),
    // About $2.
    high_fee: Amount::from_base_units(22_000_000_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://polygonscan.com/tx/{txid}"),
    explorer_address_url: Some("https://polygonscan.com/address/{address}"),
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
    chain_id: 137,
    request_timeout: Duration::from_secs(10),
    // Each issuer's own deployment: Tether's USDT0 and XAUt0, Circle's USDC, and
    // Paxos's PYUSD. Bridged copies, such as USDC.e and the PoS DAI, are left out.
    // LINK and ether are the PoS bridge's copies, which exchanges and wallets use, not
    // the ERC-677 LINK that Chainlink's PegSwap issues to pay its oracles.
    tokens: &[
        TokenId::LinkPolygon,
        TokenId::WethPolygon,
        TokenId::UsdtPolygon,
        TokenId::UsdcPolygon,
        TokenId::PyusdPolygon,
        TokenId::XautPolygon,
    ],
    token_standard: "ERC-20",
    // One block every 1.5 seconds since the second stage of PIP-86.
    block_millis: 1_500,
    // A pass per block would be wasteful at this pace.
    watch_floor: Duration::from_secs(6),
    // A minute of blocks.
    fee_history_blocks: 40,
    // Bor refuses a tip under 25 gwei on mainnet.
    min_priority_fee_wei: 25_000_000_000,
    max_priority_fee_wei: None,
    // About 24 seconds, past milestone finality, to cover a public node behind the one
    // that reported the tip.
    confirmations_target: 16,
    // Every shipped endpoint that serves logs takes a window this wide.
    log_window_blocks: 10_000,
    // Native transfers are logged from the first blocks, so the backfill runs back to
    // genesis.
    backfill_floor: 0,
    has_native_transfer_logs: true,
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
