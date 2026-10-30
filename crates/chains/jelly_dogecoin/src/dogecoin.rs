use std::sync::Arc;

use bitcoin::Weight;
use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, FeeRate, KeyKind, ScanStream, ScriptKind, Transport,
};
use jelly_net::{Endpoint, Network, ProbeReport};
use jelly_utxo::{
    Base58Versions, BumpMethod, DustRule, FeeMarket, Replacement, UtxoCodec, UtxoEntry, UtxoParams,
    WifFormat,
};

/// A hundred-millionth of a dogecoin is a koinu, and without segwit a rate per virtual
/// byte is a rate per byte.
const FEE_RATE_UNIT: &str = "koinu/B";

/// Without segwit, P2PKH is the only single-key script. The one fee level pays a fixed
/// rate, so there's no fee service to read.
pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Dogecoin,
    slip44: 3,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/3'/0'/0/{index}",
    script_kinds: &[ScriptKind::P2pkh],
    default_script: Some(ScriptKind::P2pkh),
    settled_confirmations: 6,
    poll_secs: 30,
    fee_rate_unit: Some(FEE_RATE_UNIT),
    fee_levels: &[FeeLevel::Normal],
    custom_fee: Some(CustomFee::Rate),
    // About $2.
    high_fee: Amount::from_base_units(2_500_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "dogecoin",
    explorer_tx_url: Some("https://dogechain.info/tx/{txid}"),
    explorer_address_url: Some("https://dogechain.info/address/{address}"),
    transports: &[Transport::ElectrumSsl],
    primary_transport: Transport::ElectrumSsl,
    can_bump_fee: true,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

/// P2PKH renders as `D`, and P2SH as `9` or `A`.
const CODEC: UtxoCodec = UtxoCodec {
    chain: SPEC.chain,
    segwit_hrp: None,
    base58: Base58Versions { p2pkh: 0x1e, p2sh: 0x16, superseded_p2sh: None },
};

/// Rates are per kwu, from Dogecoin Core 1.14.6's fee recommendation.
///
/// The recommended 0.01 DOGE/kB, which the reference wallet pays and miners include at by
/// default, is 1000 koinu per byte, or 250000 per kwu. Nodes relay down to a tenth of that,
/// and a replacement has to add a tenth of the relay floor.
pub const PARAMS: UtxoParams = UtxoParams {
    chain: SPEC.chain,
    codec: &CODEC,
    script_kinds: SPEC.script_kinds,
    fee_rate_unit: FEE_RATE_UNIT,
    genesis: [
        0x1a, 0x91, 0xe3, 0xda, 0xce, 0x36, 0xe2, 0xbe, 0x3b, 0xf0, 0x30, 0xa6, 0x56, 0x79, 0xfe,
        0x82, 0x1a, 0xa1, 0xd6, 0xef, 0x92, 0xe7, 0xc9, 0x90, 0x2e, 0xb3, 0x18, 0x18, 0x2c, 0x35,
        0x56, 0x91,
    ],
    checkpoint: None,
    // Merge-mined with Litecoin since block 371337.
    is_merge_mined: true,
    has_segwit: false,
    scan_stream: ScanStream::DogecoinElectrumStatus,
    // Dogecoin Core's 0.01 DOGE soft dust limit. An output below it costs that much
    // again in fee, and one below 0.001 DOGE isn't relayed at all, so the reference
    // wallet never writes one.
    dust: DustRule::Fixed(Amount::from_base_units(1_000_000)),
    min_relay_fee: FeeRate::from_sat_per_kwu(25_000),
    incremental_relay_fee: FeeRate::from_sat_per_kwu(2_500),
    // `MAX_MONEY`, ten billion DOGE, bounds one transaction's outputs. Dogecoin has
    // no supply cap.
    max_money: Amount::from_base_units(10_000_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    min_tx_fee: FeeRate::from_sat_per_kwu(250_000),
    // Server estimates swing by two orders of magnitude between neighboring targets,
    // and Dogecoin Core's wallet pays the recommended rate anyway.
    fee_market: FeeMarket::Flat,
    // Dogecoin Core 1.14 relays BIP-125 replacements by default.
    replacement: Replacement::ByDefault,
    bump_method: Some(BumpMethod::Replace),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    // `COINBASE_MATURITY` has been 240 since block 145000.
    coinbase_maturity: 240,
    pegout_maturity: 0,
    // Dogecoin's version bytes are its own, so every address form is unambiguous.
    address_notes: &[],
    address_refusals: &[],
    // Dogecoin Core's `-maxtxfee` default of 100 DOGE.
    fee_ceiling: Amount::from_base_units(10_000_000_000),
    // The one fee level pays the recommended rate, so that's what spending change
    // later costs.
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250_000),
};

pub const ENTRY: &dyn ChainEntry = &UtxoEntry {
    spec: &SPEC,
    params: &PARAMS,
    // No segwit, so Electrum never wrote a script type into this version byte.
    wif: WifFormat { version: 0x9e, has_script_offsets: false, name: "Dogecoin WIF key" },
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
