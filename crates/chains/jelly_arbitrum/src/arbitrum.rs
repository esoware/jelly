use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, KeyKind, TokenId, Transport,
};
use jelly_evm::{EvmCodec, EvmEntry, EvmParams};
use jelly_net::{Endpoint, Network, ProbeReport};

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Arbitrum,
    coin_name: "Ethereum",
    coin_chain: ChainId::Ethereum,
    // Ethereum's coin type, as in MetaMask and Trust Wallet, so a seed has the same
    // address on both networks.
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
    // About $2.
    high_fee: Amount::from_base_units(900_000_000_000_000),
    can_pin_nonce: true,
    can_carry_data: true,
    uri_scheme: "ethereum",
    explorer_tx_url: Some("https://arbiscan.io/tx/{txid}"),
    explorer_address_url: Some("https://arbiscan.io/address/{address}"),
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
    chain_id: 42_161,
    request_timeout: Duration::from_secs(10),
    // Each issuer's own deployment: Ethena's ENA, Tether's USDT0 and XAUt0, Circle's
    // USDC, Paxos's PYUSD and USDG, and Sky's USDS. The bridged USDC.e is left out.
    // LINK and WETH are the canonical bridge's, and ARB is issued here.
    tokens: &[
        TokenId::LinkArbitrum,
        TokenId::WethArbitrum,
        TokenId::EnaArbitrum,
        TokenId::ArbArbitrum,
        TokenId::UsdtArbitrum,
        TokenId::UsdcArbitrum,
        TokenId::UsdsArbitrum,
        TokenId::UsdgArbitrum,
        TokenId::PyusdArbitrum,
        TokenId::XautArbitrum,
    ],
    token_standard: "ERC-20",
    block_millis: 250,
    // A pass per block would be wasteful at this pace.
    watch_floor: Duration::from_secs(6),
    // The sequencer ignores tips, so the history is only read for the next base fee.
    fee_history_blocks: 20,
    min_priority_fee_wei: 0,
    max_priority_fee_wei: None,
    // Five seconds, to cover a public node behind the one that reported the tip.
    confirmations_target: 20,
    // Under four hours. The shipped endpoints that serve old logs accept it, and the
    // window shrinks for a gateway that routes to a node with a narrower cap.
    log_window_blocks: 50_000,
    // WETH logged its first transfers in block 56, so the backfill runs back to genesis.
    backfill_floor: 0,
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
