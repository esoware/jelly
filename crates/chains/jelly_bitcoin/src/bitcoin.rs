use std::sync::Arc;

use bitcoin::Weight;
use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, FeeRate, KeyKind, ScanStream, ScriptKind, Transport,
};
use jelly_net::{Endpoint, Network, ProbeReport};
use jelly_utxo::{
    Auction, Base58Versions, BumpMethod, DustRule, FeeMarket, KnownBlock, Replacement, UtxoCodec,
    UtxoEntry, UtxoParams, WifFormat,
};

const FEE_RATE_UNIT: &str = "sat/vB";

pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Bitcoin,
    slip44: 0,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/84'/0'/0'/0/{index}",
    script_kinds: &[
        ScriptKind::P2tr,
        ScriptKind::P2wpkh,
        ScriptKind::P2shP2wpkh,
        ScriptKind::P2pkh,
    ],
    default_script: Some(ScriptKind::P2wpkh),
    settled_confirmations: 6,
    poll_secs: 30,
    fee_rate_unit: Some(FEE_RATE_UNIT),
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::Rate),
    // A payment spending five inputs is about 410 vB, which cost about 0.004 BTC when
    // blocks needed more than 1000 sat/vB around the April 2024 halving. Half the fee a
    // build refuses.
    high_fee: Amount::from_base_units(1_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "bitcoin",
    explorer_tx_url: Some("https://mempool.space/tx/{txid}"),
    explorer_address_url: Some("https://mempool.space/address/{address}"),
    transports: &[Transport::ElectrumSsl, Transport::FeeApi],
    primary_transport: Transport::ElectrumSsl,
    can_bump_fee: true,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

const CODEC: UtxoCodec = UtxoCodec {
    chain: SPEC.chain,
    segwit_hrp: Some("bc"),
    base58: Base58Versions { p2pkh: 0x00, p2sh: 0x05, superseded_p2sh: None },
};

/// Bitcoin relay and consensus constants in weight units.
///
/// Core's per-kvB fees divide by four, so the dust relay fee is 750 sat/kwu and
/// the minimum and incremental relay fees, 100 sat/kvB since Core 29.1, are 25
/// sat/kwu.
///
/// The minimum and incremental relay fees are only floors. A server's reported
/// relay floor raises the minimum, and the network's incremental relay fee raises
/// the replacement floor, so a node that still runs the older 1000 sat/kvB defaults
/// is never offered less than it takes.
pub const PARAMS: UtxoParams = UtxoParams {
    chain: SPEC.chain,
    codec: &CODEC,
    script_kinds: SPEC.script_kinds,
    fee_rate_unit: FEE_RATE_UNIT,
    genesis: [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0xd6, 0x68, 0x9c, 0x08, 0x5a, 0xe1, 0x65, 0x83, 0x1e,
        0x93, 0x4f, 0xf7, 0x63, 0xae, 0x46, 0xa2, 0xa6, 0xc1, 0x72, 0xb3, 0xf1, 0xb6, 0x0a, 0x8c,
        0xe2, 0x6f,
    ],
    // The fourth halving's block, mined 2024-04-20, past every split from Bitcoin
    // that left servers behind: Bitcoin Cash at 478559, Bitcoin Gold at 491407,
    // and every chain that split from Bitcoin Cash since.
    checkpoint: Some(KnownBlock {
        height: 840_000,
        hash: [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x20, 0x28, 0x3a, 0x03,
            0x27, 0x48, 0xce, 0xf8, 0x22, 0x78, 0x73, 0xff, 0x48, 0x72, 0x68, 0x9b, 0xf2, 0x3f,
            0x1c, 0xda, 0x83, 0xa5,
        ],
    }),
    is_merge_mined: false,
    has_segwit: true,
    scan_stream: ScanStream::BitcoinElectrumStatus,
    dust: DustRule::RelayFee(FeeRate::from_sat_per_kwu(750)),
    min_relay_fee: FeeRate::from_sat_per_kwu(25),
    incremental_relay_fee: FeeRate::from_sat_per_kwu(25),
    max_money: Amount::from_base_units(21_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    // Core's `DEFAULT_TRANSACTION_MINFEE` of 1000 sat/kvB.
    min_tx_fee: FeeRate::from_sat_per_kwu(250),
    fee_market: FeeMarket::Auction(Auction {
        // An hour, half an hour, and the next block, the horizons of `hourFee`,
        // `halfHourFee`, and `fastestFee`. Core answers a one-block target with its
        // two-block estimate.
        target_blocks: [6, 3, 1],
        block_vsize: 1_000_000,
        // The rates to a thousandth of a sat/vB, which Sparrow and Trezor Suite
        // read, so a level between two whole numbers is not rounded to one. No
        // level goes below `min_tx_fee`, but a custom rate is judged slow against
        // the exact reading. Older deployments serve only the whole-number route.
        api_route: "/v1/fees/precise",
        api_fallback_route: Some("/v1/fees/recommended"),
    }),
    // Signalling is what makes a payment replaceable on a node that honours it, and
    // Core 28 replaces regardless, so a final sequence would only cost the user the
    // ability to unstick their own payment.
    replacement: Replacement::ByDefault,
    bump_method: Some(BumpMethod::Replace),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    // Bitcoin has no extension block, so no output is a peg-out.
    pegout_maturity: 0,
    // Every Bitcoin address form is unambiguous, so there is nothing to warn a
    // sender about.
    address_notes: &[],
    address_refusals: &[],
    fee_ceiling: Amount::from_base_units(2_000_000),
    // Core's `consolidatefeerate` of 10 sat/vB.
    long_term_fee_rate: FeeRate::from_sat_per_kwu(2_500),
};

pub const ENTRY: &dyn ChainEntry = &UtxoEntry {
    spec: &SPEC,
    params: &PARAMS,
    // Bitcoin Cash and Zcash kept this version byte, so a Bitcoin WIF also imports
    // into both.
    wif: WifFormat { version: 0x80, has_script_offsets: true, name: "Bitcoin WIF key" },
    is_scheme_prefix: false,
    default_endpoints: include_str!("../endpoints.json"),
};

pub async fn build(
    network: &Network,
    request: &BuildRequest,
) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    jelly_utxo::build(network, request, &PARAMS, &SPEC).await
}

pub async fn probe(network: &Network, endpoint: &Endpoint) -> ProbeReport {
    jelly_utxo::probe(network, endpoint, &PARAMS).await
}
