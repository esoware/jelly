use std::sync::Arc;

use bitcoin::Weight;
use jelly_core::{
    Amount, BuildError, BuildRequest, ChainAdapter, ChainEntry, ChainId, ChainSpec, CustomFee,
    FeeLevel, FeeRate, KeyKind, ScanStream, ScriptKind, Transport, TxWarningKind,
};
use jelly_net::{Endpoint, Network, ProbeReport};
use jelly_utxo::{
    AddressNote, AddressRefusal, Auction, Base58Versions, BumpMethod, DustRule, FeeMarket,
    KnownBlock, Replacement, UtxoCodec, UtxoEntry, UtxoParams, WifFormat,
};

/// The litoshi is abbreviated lit, as litecoinspace.org writes it.
const FEE_RATE_UNIT: &str = "lit/vB";

/// Litecoin has activated taproot, and its codec reads and pays a taproot address, but
/// no Litecoin wallet derives a taproot key, so it is no flavour to derive under.
pub const SPEC: ChainSpec = ChainSpec {
    chain: ChainId::Litecoin,
    slip44: 2,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/84'/2'/0'/0/{index}",
    script_kinds: &[ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh, ScriptKind::P2pkh],
    default_script: Some(ScriptKind::P2wpkh),
    settled_confirmations: 6,
    poll_secs: 30,
    fee_rate_unit: Some(FEE_RATE_UNIT),
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::Rate),
    // Under $1 at 2026 prices. A 141 vB payment costs 1410 lit at the wallet's minimum of
    // 10 lit/vB. Update it when the price moves a lot.
    high_fee: Amount::from_base_units(1_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "litecoin",
    explorer_tx_url: Some("https://litecoinblockexplorer.net/tx/{txid}"),
    explorer_address_url: Some("https://litecoinblockexplorer.net/address/{address}"),
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

/// P2PKH renders as `L` and P2SH as `M`.
///
/// The superseded P2SH version byte renders as `3`, and Litecoin Core still accepts it
/// when decoding. It is Bitcoin's current P2SH byte, which is what makes a `3...` address
/// ambiguous between the two chains.
const CODEC: UtxoCodec = UtxoCodec {
    chain: SPEC.chain,
    segwit_hrp: Some("ltc"),
    base58: Base58Versions { p2pkh: 0x30, p2sh: 0x32, superseded_p2sh: Some(0x05) },
};

/// Litecoin relay and consensus constants in weight units.
///
/// The 30000 lit/kvB dust relay fee, ten times Bitcoin's, is 7500 per kwu, and
/// the 1000 lit/kvB minimum and incremental relay fees are 250 per kwu.
///
/// The network's incremental relay fee raises the compiled replacement floor when
/// the node requires more.
pub const PARAMS: UtxoParams = UtxoParams {
    chain: SPEC.chain,
    codec: &CODEC,
    script_kinds: SPEC.script_kinds,
    fee_rate_unit: FEE_RATE_UNIT,
    // Mined 2011-10-07.
    genesis: [
        0x12, 0xa7, 0x65, 0xe3, 0x1f, 0xfd, 0x40, 0x59, 0xba, 0xda, 0x1e, 0x25, 0x19, 0x0f, 0x6e,
        0x98, 0xc9, 0x9d, 0x97, 0x14, 0xd3, 0x34, 0xef, 0xa4, 0x1a, 0x19, 0x5a, 0x7e, 0x7e, 0x04,
        0xbf, 0xe2,
    ],
    // The third halving's block, mined 2023-08-02, past every chain that kept
    // Litecoin's block zero: Feathercoin, which parts from it at block one, and
    // Litecoin Cash, which split at 1371111.
    checkpoint: Some(KnownBlock {
        height: 2_520_000,
        hash: [
            0x54, 0xcd, 0x0e, 0xf4, 0x8a, 0x97, 0x7c, 0x5b, 0xb9, 0xc8, 0x45, 0xe0, 0xc6, 0x7a,
            0x4e, 0x9d, 0x17, 0xa5, 0x89, 0x4b, 0x08, 0x4a, 0xea, 0x4b, 0xf7, 0xe5, 0x94, 0x12,
            0x2f, 0x5c, 0x0d, 0xc1,
        ],
    }),
    is_merge_mined: false,
    has_segwit: true,
    scan_stream: ScanStream::LitecoinElectrumStatus,
    dust: DustRule::RelayFee(FeeRate::from_sat_per_kwu(7_500)),
    min_relay_fee: FeeRate::from_sat_per_kwu(250),
    incremental_relay_fee: FeeRate::from_sat_per_kwu(250),
    max_money: Amount::from_base_units(84_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    // Litecoin Core's `DEFAULT_TRANSACTION_MINFEE` of 10000 lit/kvB, ten times its
    // relay floor.
    min_tx_fee: FeeRate::from_sat_per_kwu(2_500),
    fee_market: FeeMarket::Auction(Auction {
        // An hour, Litecoin Core's `DEFAULT_TX_CONFIRM_TARGET`, and two blocks, at
        // two and a half minutes a block.
        target_blocks: [24, 6, 2],
        block_vsize: 1_000_000,
        // Only the whole-number route, which every mempool.space deployment serves.
        api_route: "/v1/fees/recommended",
        api_fallback_route: None,
    }),
    // Litecoin Core refuses a replacement unless `-mempoolreplacement` is set,
    // which it is not by default, so a send is sped up by a child spending its
    // change, which every node relays.
    replacement: Replacement::WhenConfigured,
    bump_method: Some(BumpMethod::ChildPaysForParent),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    pegout_maturity: 6,
    // A `3...` address is Litecoin's superseded P2SH form and shares its encoding
    // with Bitcoin's, so the string alone cannot say which chain it belongs to,
    // and paying the wrong one is unrecoverable.
    address_notes: &[AddressNote {
        prefix: "3",
        kind: TxWarningKind::AmbiguousAddress,
        message: "This legacy 3... address is valid on both Litecoin and Bitcoin. Confirm the recipient expects Litecoin before sending.",
    }],
    // `MimbleWimble` Extension Blocks (LIP-0002 to LIP-0004, activated May 2022)
    // are a parallel transaction domain with confidential amounts. An MWEB
    // address is not an output script, and the extension block is invisible to
    // Electrum's scripthash index, so a scripthash wallet can neither pay nor
    // watch one.
    address_refusals: &[AddressRefusal {
        prefix: "ltcmweb",
        message: "Jelly cannot send to or scan Litecoin MWEB addresses. Use a regular Litecoin address.",
    }],
    // A 250 vB payment at Litecoin Core's 10 lit/vB wallet floor pays 2500 lit, so
    // this is nearly three orders of magnitude above anything normal.
    fee_ceiling: Amount::from_base_units(2_000_000),
    // The relay floor rather than Core's higher consolidation rate, so coin
    // selection does not overprice change.
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250),
};

pub const ENTRY: &dyn ChainEntry = &UtxoEntry {
    spec: &SPEC,
    params: &PARAMS,
    wif: WifFormat { version: 0xb0, has_script_offsets: true, name: "Litecoin WIF key" },
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
