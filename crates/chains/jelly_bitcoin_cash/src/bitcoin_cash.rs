use std::sync::Arc;

use bitcoin::Weight;
use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, FeeRate, KeyKind, ScanStream, ScriptKind, Transport, TxWarningKind,
};
use jelly_net::{Endpoint, Network, ProbeReport};
use jelly_utxo::{
    AddressNote, DustRule, FeeMarket, KnownBlock, Replacement, UtxoEntry, UtxoParams, WifFormat,
};

use crate::address::CashAddrCodec;

/// Without segwit a rate per virtual byte is a rate per byte.
const FEE_RATE_UNIT: &str = "sat/B";

/// Without segwit, P2PKH is the only single-key script. The one fee level pays a fixed
/// rate, so there's no fee service to read.
pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::BitcoinCash,
    slip44: 145,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/145'/0'/0/{index}",
    script_kinds: &[ScriptKind::P2pkh],
    default_script: Some(ScriptKind::P2pkh),
    settled_confirmations: 6,
    poll_secs: 30,
    fee_rate_unit: Some(FEE_RATE_UNIT),
    fee_levels: &[FeeLevel::Normal],
    custom_fee: Some(CustomFee::Rate),
    // About $2.
    high_fee: Amount::from_base_units(700_000),
    can_pin_nonce: false,
    can_carry_data: false,
    // The scheme is also the `CashAddr` prefix, which is part of the address checksum.
    uri_scheme: "bitcoincash",
    explorer_tx_url: Some("https://bchblockexplorer.com/tx/{txid}"),
    explorer_address_url: Some("https://bchblockexplorer.com/address/{address}"),
    transports: &[Transport::ElectrumSsl],
    primary_transport: Transport::ElectrumSsl,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

/// Rates are per kwu, from Bitcoin Cash Node.
///
/// The dust threshold is three times its 1000 sat/kB `DUST_RELAY_TX_FEE`, the same 3000 as
/// Bitcoin's and 546 sat for P2PKH, and the minimum relay fee and the wallet's own minimum
/// are both 1000 sat/kB, or 250 per kwu.
pub const PARAMS: UtxoParams = UtxoParams {
    chain: SPEC.chain,
    codec: &CashAddrCodec,
    script_kinds: SPEC.script_kinds,
    fee_rate_unit: FEE_RATE_UNIT,
    // Bitcoin's block zero, kept at the split.
    genesis: [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0xd6, 0x68, 0x9c, 0x08, 0x5a, 0xe1, 0x65, 0x83, 0x1e,
        0x93, 0x4f, 0xf7, 0x63, 0xae, 0x46, 0xa2, 0xa6, 0xc1, 0x72, 0xb3, 0xf1, 0xb6, 0x0a, 0x8c,
        0xe2, 0x6f,
    ],
    // Bitcoin Cash's own fourth halving (2024-04-04), after its splits from Bitcoin at
    // 478559, Bitcoin SV at 556767, and eCash at 661648.
    checkpoint: Some(KnownBlock {
        height: 840_000,
        hash: [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb3, 0xcf, 0xd7, 0x3d, 0xbd,
            0x87, 0xc5, 0xe6, 0xca, 0xe2, 0x6d, 0x89, 0xa5, 0x95, 0x6e, 0xe7, 0x81, 0x93, 0x73,
            0x3f, 0x61, 0x34, 0x0e,
        ],
    }),
    is_merge_mined: false,
    has_segwit: false,
    scan_stream: ScanStream::BitcoinCashElectrumStatus,
    dust: DustRule::RelayFee(FeeRate::from_sat_per_kwu(750)),
    min_relay_fee: FeeRate::from_sat_per_kwu(250),
    // Nothing replaces on Bitcoin Cash, so only a node's mempool limiting uses this.
    incremental_relay_fee: FeeRate::from_sat_per_kwu(250),
    max_money: Amount::from_base_units(21_000_000 * 100_000_000),
    // `MAX_STANDARD_TX_SIZE` of 100000 bytes.
    max_standard_weight: Weight::from_wu(400_000),
    min_tx_fee: FeeRate::from_sat_per_kwu(250),
    // Blocks hold 32 MB and more, and every wallet pays the 1 sat/B floor.
    fee_market: FeeMarket::Flat,
    // Bitcoin Cash dropped replacement when it split in 2017, and Electron Cash writes
    // the same non-signaling sequence.
    replacement: Replacement::Never,
    bump_method: None,
    // The fork id is zero, so the sighash type is 0x41.
    sighash_fork_id: Some(0),
    has_cash_tokens: true,
    // Like Electron Cash, a plain spend leaves SLP coins alone, so a wallet on the same
    // keys keeps its tokens.
    has_slp_tokens: true,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    pegout_maturity: 0,
    // A legacy address is the same string on Bitcoin, and paying the wrong chain can
    // be unrecoverable.
    address_notes: &[
        AddressNote {
            prefix: "1",
            kind: TxWarningKind::AmbiguousAddress,
            message: "This legacy 1... address is valid on both Bitcoin Cash and Bitcoin. Confirm the recipient expects Bitcoin Cash before sending.",
        },
        AddressNote {
            prefix: "3",
            kind: TxWarningKind::AmbiguousAddress,
            message: "This legacy 3... address is valid on both Bitcoin Cash and Bitcoin. Confirm the recipient expects Bitcoin Cash before sending.",
        },
    ],
    address_refusals: &[],
    // Nearly four orders of magnitude above a 226-byte payment at the 1 sat/B floor.
    fee_ceiling: Amount::from_base_units(2_000_000),
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250),
};

pub const ENTRY: &dyn ChainEntry = &UtxoEntry {
    spec: &SPEC,
    params: &PARAMS,
    // Bitcoin's version byte, which Electron Cash writes too, so a Bitcoin WIF imports
    // here and an export reads as one.
    wif: WifFormat { version: 0x80, has_script_offsets: true, name: "Bitcoin WIF key" },
    is_scheme_prefix: true,
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
