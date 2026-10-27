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

/// Dogecoin has no segregated witness, so P2PKH is its only single-key form and there
/// is nothing to choose. Its one fee level is a fixed rate, so there is no fee service
/// to read.
pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Dogecoin,
    coin_name: "Dogecoin",
    coin_chain: ChainId::Dogecoin,
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
    // About $1.93 at the October 2026 price of $0.077. A 226-byte payment costs about
    // 0.0023 DOGE at the recommended 0.01 DOGE/kB.
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

/// P2PKH renders as `D`, and P2SH as `9` or `A`. Dogecoin never activated segregated
/// witness, so the two base58 forms are its only addresses.
const CODEC: UtxoCodec = UtxoCodec {
    chain: SPEC.chain,
    segwit_hrp: None,
    base58: Base58Versions { p2pkh: 0x1e, p2sh: 0x16, superseded_p2sh: None },
};

/// Dogecoin relay and consensus constants in weight units, from Dogecoin Core's
/// fee recommendation as of 1.14.6.
///
/// Dogecoin has no segregated witness. Its recommended 0.01 DOGE/kB, which the
/// reference wallet pays and miners include at by default, is 1000 koinu per byte,
/// or 250000 per kwu. Nodes relay down to a tenth of that, and a replacement has to
/// add a tenth of the relay floor.
pub const PARAMS: UtxoParams = UtxoParams {
    chain: SPEC.chain,
    codec: &CODEC,
    script_kinds: SPEC.script_kinds,
    fee_rate_unit: FEE_RATE_UNIT,
    // Mined 2013-12-06.
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
    // The soft dust limit of 0.01 DOGE. A node takes an output below it only with
    // that much again in fee, and refuses one below 0.001 DOGE outright, so the
    // reference wallet never writes one, and neither does this build.
    dust: DustRule::Fixed(Amount::from_base_units(1_000_000)),
    min_relay_fee: FeeRate::from_sat_per_kwu(25_000),
    incremental_relay_fee: FeeRate::from_sat_per_kwu(2_500),
    // `MAX_MONEY`, ten billion DOGE, bounds one transaction's outputs. Dogecoin has
    // no supply cap.
    max_money: Amount::from_base_units(10_000_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    min_tx_fee: FeeRate::from_sat_per_kwu(250_000),
    // The servers' estimators swing by two orders of magnitude between neighbouring
    // targets, while Dogecoin Core's wallet pays the recommended rate.
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
    // Every automatic spend pays the recommended rate, so that is what spending
    // change later costs.
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250_000),
};

pub const ENTRY: &dyn ChainEntry = &UtxoEntry {
    spec: &SPEC,
    params: &PARAMS,
    // Dogecoin has no segwit, so no Electrum release wrote a script type into its
    // version byte, and the byte names a key and nothing more.
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
