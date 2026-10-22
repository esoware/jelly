//! Deterministic fixtures for the shared UTXO engine. Only `[dev-dependencies]`
//! enable the `test-util` feature.
//!
//! Every key comes from one seed byte, so a test names a seed and gets the same
//! address, script, and outpoint on every run.
//!
//! A test outside this crate reads a payload, the outpoint claims, or a stored
//! cursor through the operations here.
//!
//! The four tables here are copies of the chain crates' own, for this crate's unit
//! tests, which cannot name a chain crate. Each chain crate's tests assert that its
//! table matches its copy. Bitcoin Cash's copy reads only the legacy address forms,
//! because `CashAddr` lives in its chain crate.

use bitcoin::absolute::LockTime;
use bitcoin::consensus::serialize;
use bitcoin::hashes::Hash as _;
use bitcoin::secp256k1::{Keypair, SecretKey};
use bitcoin::transaction::Version;
use bitcoin::{OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Weight, Witness};
use jelly_core::{
    Amount, ChainEntry, ChainId, FeeRate, KeyError, KeyKind, KeyMaterial, ScanStream, ScriptKind,
    TxWarningKind, test_util::key_material_of,
};
use jelly_electrum::{
    Method,
    test_util::{Answer, FakeElectrum, script_hash},
};
use jelly_keypair::{parse_path, purpose_of, signing_context, test_util::key_at};
use serde::Deserialize;

use crate::adapter::UtxoAdapter;
use crate::address::UtxoCodec;
use crate::cursor::SyncCursor;
use crate::error::UtxoError;
use crate::legacy::Base58Versions;
use crate::params::{
    AddressNote, AddressRefusal, Auction, BumpMethod, DustRule, FeeMarket, KnownBlock, Replacement,
    UtxoParams,
};
use crate::payload::UtxoPayload;
use crate::script::SpendableScript;

#[derive(Debug, Deserialize)]
pub struct Vectors {
    pub derived: Vec<Derived>,
    pub raw: Vec<Raw>,
    pub scripts: Vec<ScriptRow>,
    pub valid: Vec<String>,
    pub invalid: Vec<Invalid>,
    pub uris: Uris,
}

#[derive(Debug, Deserialize)]
pub struct Derived {
    pub path: String,
    pub script: Option<ScriptKind>,
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct Raw {
    pub secret: String,
    pub script: Option<ScriptKind>,
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct ScriptRow {
    pub address: String,
    pub script_kind: Option<ScriptKind>,
    pub script_pubkey: String,
}

#[derive(Debug, Deserialize)]
pub struct Invalid {
    pub address: String,
    pub why: String,
}

#[derive(Debug, Deserialize)]
pub struct Uris {
    pub accepted: Vec<AcceptedUri>,
    pub unsupported: Vec<UnsupportedUri>,
}

#[derive(Debug, Deserialize)]
pub struct AcceptedUri {
    pub uri: String,
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct UnsupportedUri {
    pub uri: String,
    pub why: String,
}

pub fn address_at(
    entry: &dyn ChainEntry,
    text: &str,
    requested: Option<ScriptKind>,
) -> Result<String, KeyError> {
    let script = entry.spec().resolve_script(purpose_of(&parse_path(text)?), requested)?;
    entry.address(&key_at(text)?, script)
}

#[derive(Debug, Clone)]
pub struct TestKey {
    secret: [u8; 32],
    pubkey: [u8; 33],
    address: String,
    script: SpendableScript,
}

impl TestKey {
    pub const fn pubkey(&self) -> &[u8; 33] {
        &self.pubkey
    }

    pub const fn address(&self) -> &str {
        self.address.as_str()
    }

    pub fn material(&self) -> KeyMaterial {
        key_material_of(KeyKind::Secp256k1, self.secret.to_vec())
    }

    pub fn script_pubkey(&self) -> ScriptBuf {
        self.script.script_buf().clone()
    }
}

#[cfg(test)]
impl TestKey {
    pub(crate) const fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    pub(crate) const fn script(&self) -> &SpendableScript {
        &self.script
    }
}

pub fn test_key(params: &UtxoParams, kind: ScriptKind, seed: u8) -> Result<TestKey, UtxoError> {
    let mut secret = [1u8; 32];
    if let Some(slot) = secret.last_mut() {
        *slot = seed;
    }
    let secret_key = SecretKey::from_slice(&secret)
        .map_err(|_| UtxoError::signing("the fixture seed is not a scalar"))?;
    let keypair = Keypair::from_secret_key(signing_context(), &secret_key);
    let pubkey = keypair.public_key().serialize();
    let address = params.key_address(kind, &pubkey)?;
    let script = params.spendable_script(&address)?;
    Ok(TestKey { secret, pubkey, address, script })
}

/// A transaction paying `outputs`, with one dummy input so it has an identity.
pub fn tx_paying(outputs: &[(ScriptBuf, u64)]) -> Transaction {
    tx_spending_each(&[OutPoint { txid: fixture_txid(outputs.len()), vout: 0 }], outputs)
}

pub fn tx_spending(funding: &Transaction, vout: u32, outputs: &[(ScriptBuf, u64)]) -> Transaction {
    tx_spending_each(&[OutPoint { txid: funding.compute_txid(), vout }], outputs)
}

/// A transaction spending every outpoint of `spent`, in order, into `outputs`.
pub fn tx_spending_each(spent: &[OutPoint], outputs: &[(ScriptBuf, u64)]) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: spent
            .iter()
            .map(|outpoint| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: outputs
            .iter()
            .map(|(script_pubkey, value)| TxOut {
                value: bitcoin::Amount::from_sat(*value),
                script_pubkey: script_pubkey.clone(),
            })
            .collect(),
    }
}

pub(crate) fn fixture_txid(seed: usize) -> Txid {
    let mut bytes = [0u8; 32];
    if let Some(slot) = bytes.first_mut() {
        *slot = u8::try_from(seed.checked_rem(251).unwrap_or(0)).unwrap_or(0).max(1);
    }
    if let Some(slot) = bytes.get_mut(1) {
        *slot = 0xab;
    }
    Txid::from_byte_array(bytes)
}

/// One `blockchain.scripthash.listunspent` entry: the funding txid, the output index,
/// the value, and the height, zero while the funding transaction is unconfirmed.
pub type ListedUnspent = (Txid, u32, u64, u32);

/// A fake Electrum server scripted with the answers one sync pass needs, as raw
/// JSON so a test pins exactly what a server said.
#[derive(Debug, Default)]
pub struct FakeChain {
    pub server: FakeElectrum,
}

impl FakeChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tip(&self, height: u32, unix_seconds: u32) {
        self.server.always(
            Method::HeadersSubscribe,
            Answer::Result(format!(
                r#"{{"height":{height},"hex":"{}"}}"#,
                hex::encode(header_bytes(unix_seconds))
            )),
        );
    }

    /// Every height gets the same header, so a test must not ask for adjacent
    /// heights or the client's linkage check trips.
    pub fn header(&self, unix_seconds: u32) {
        self.server.always(
            Method::BlockHeader,
            Answer::Result(format!(r#""{}""#, hex::encode(header_bytes(unix_seconds)))),
        );
    }

    /// One history, the answer for every script a request names.
    pub fn history(&self, entries: &[(Txid, i32)]) {
        self.server.always(Method::GetHistory, Answer::Result(history_json(entries)));
    }

    /// Each script's history, looked up by the script hash a request names, so an
    /// answer does not depend on what else its batch asked for.
    pub fn histories(&self, by_script: &[(ScriptBuf, Vec<(Txid, i32)>)]) {
        let answers = by_script
            .iter()
            .map(|(script, entries)| (script_hash(script), history_json(entries)))
            .collect();
        self.server.always(Method::GetHistory, Answer::Keyed(answers));
    }

    /// One unspent set, the answer for every script a request names.
    pub fn unspent(&self, entries: &[ListedUnspent]) {
        self.server.always(Method::ListUnspent, Answer::Result(unspent_json(entries)));
    }

    /// Each script's unspent set, looked up by the script hash a request names.
    pub fn unspents(&self, by_script: &[(ScriptBuf, Vec<ListedUnspent>)]) {
        let answers = by_script
            .iter()
            .map(|(script, entries)| (script_hash(script), unspent_json(entries)))
            .collect();
        self.server.always(Method::ListUnspent, Answer::Keyed(answers));
    }

    /// Answers every `blockchain.transaction.get` for one of `txs` by its txid, in
    /// any batch, the way a real server serves them.
    pub fn transactions(&self, txs: &[Transaction]) {
        let by_txid = txs.iter().map(|tx| (tx.compute_txid().to_string(), tx_json(tx))).collect();
        self.server.always(Method::GetTransaction, Answer::Keyed(by_txid));
    }

    /// Answers only the next `blockchain.transaction.get` batch, one transaction
    /// per element in request order.
    ///
    /// A sync pass fetches the history's transactions and then their parents, and
    /// a test scripts each round.
    pub fn queue_transactions(&self, txs: &[Transaction]) {
        self.server.on(Method::GetTransaction, Answer::Each(txs.iter().map(tx_json).collect()));
    }
}

fn history_json(entries: &[(Txid, i32)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(txid, height)| format!(r#"{{"tx_hash":"{txid}","height":{height}}}"#))
        .collect();
    format!("[{}]", items.join(","))
}

fn unspent_json(entries: &[ListedUnspent]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(txid, vout, value, height)| {
            format!(r#"{{"tx_hash":"{txid}","tx_pos":{vout},"value":{value},"height":{height}}}"#)
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// `tx` as the JSON string a server returns it in.
fn tx_json(tx: &Transaction) -> String {
    format!(r#""{}""#, hex::encode(serialize(tx)))
}

/// An 80-byte header whose only meaningful field is `time`, the one a sync pass
/// reads.
fn header_bytes(unix_seconds: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(80);
    header.extend_from_slice(&2u32.to_le_bytes());
    header.extend_from_slice(&[0u8; 32]);
    header.extend_from_slice(&[0u8; 32]);
    header.extend_from_slice(&unix_seconds.to_le_bytes());
    header.extend_from_slice(&[0xff, 0xff, 0x00, 0x1d]);
    header.extend_from_slice(&0u32.to_le_bytes());
    header
}

/// What a build decided, read back off the payload of the unsigned transaction
/// it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPayload {
    pub tx: Transaction,
    pub sources: Vec<String>,
    pub to: String,
    pub fee: Amount,
    /// The coin selection strategy's wire spelling, such as `drain`.
    pub strategy: &'static str,
    /// Leftover handed to the miner rather than paid back as change.
    pub donated: Amount,
    /// The transaction a replacement replaces.
    pub replaces: Option<Txid>,
    /// The transaction a child pays for.
    pub pays_for: Option<Txid>,
    pub input_kinds: Vec<ScriptKind>,
}

pub fn decode_payload(bytes: &[u8]) -> Result<DecodedPayload, UtxoError> {
    let payload = UtxoPayload::decode(bytes)?;
    let intent = payload.intent();
    Ok(DecodedPayload {
        tx: payload.unsigned_tx().clone(),
        sources: intent.sources.clone(),
        to: intent.to.clone(),
        fee: payload.fee(),
        strategy: intent.strategy.as_str(),
        donated: intent.donated,
        replaces: intent.replaces.map(|replaced| replaced.txid),
        pays_for: intent.pays_for,
        input_kinds: payload.inputs().iter().map(|input| input.script.kind().into()).collect(),
    })
}

/// Every outpoint `adapter` holds for a transaction awaiting broadcast, in a
/// stable order.
pub fn claimed(adapter: &UtxoAdapter) -> Vec<OutPoint> {
    adapter.claims().claimed()
}

/// The transaction `adapter` holds `outpoint` for.
pub fn claim_owner(adapter: &UtxoAdapter, outpoint: OutPoint) -> Option<Txid> {
    adapter.claims().owner(outpoint)
}

/// The script kind this build spends `address` with, or the refusal of a form it
/// cannot spend.
pub fn spendable_kind(params: &UtxoParams, address: &str) -> Result<ScriptKind, UtxoError> {
    Ok(params.spendable_script(address)?.kind().into())
}

/// The balance and coin count a stored sync cursor records, or `None` for bytes
/// this build refuses as unreadable.
pub fn cursor_summary(bytes: &[u8]) -> Option<(Amount, usize)> {
    let cursor = SyncCursor::decode(bytes)?;
    Some((cursor.balance(), cursor.coins().len()))
}

pub const BITCOIN: UtxoParams = UtxoParams {
    chain: ChainId::Bitcoin,
    codec: &UtxoCodec {
        chain: ChainId::Bitcoin,
        segwit_hrp: Some("bc"),
        base58: Base58Versions { p2pkh: 0x00, p2sh: 0x05, superseded_p2sh: None },
    },
    script_kinds: &[
        ScriptKind::P2tr,
        ScriptKind::P2wpkh,
        ScriptKind::P2shP2wpkh,
        ScriptKind::P2pkh,
    ],
    fee_rate_unit: "sat/vB",
    genesis: [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0xd6, 0x68, 0x9c, 0x08, 0x5a, 0xe1, 0x65, 0x83, 0x1e,
        0x93, 0x4f, 0xf7, 0x63, 0xae, 0x46, 0xa2, 0xa6, 0xc1, 0x72, 0xb3, 0xf1, 0xb6, 0x0a, 0x8c,
        0xe2, 0x6f,
    ],
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
    min_tx_fee: FeeRate::from_sat_per_kwu(250),
    fee_market: FeeMarket::Auction(Auction {
        target_blocks: [6, 3, 1],
        block_vsize: 1_000_000,
        api_route: "/v1/fees/precise",
        api_fallback_route: Some("/v1/fees/recommended"),
    }),
    replacement: Replacement::ByDefault,
    bump_method: Some(BumpMethod::Replace),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    pegout_maturity: 0,
    address_notes: &[],
    address_refusals: &[],
    fee_ceiling: Amount::from_base_units(2_000_000),
    long_term_fee_rate: FeeRate::from_sat_per_kwu(2_500),
};

pub const LITECOIN: UtxoParams = UtxoParams {
    chain: ChainId::Litecoin,
    codec: &UtxoCodec {
        chain: ChainId::Litecoin,
        segwit_hrp: Some("ltc"),
        base58: Base58Versions { p2pkh: 0x30, p2sh: 0x32, superseded_p2sh: Some(0x05) },
    },
    script_kinds: &[ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh, ScriptKind::P2pkh],
    fee_rate_unit: "lit/vB",
    genesis: [
        0x12, 0xa7, 0x65, 0xe3, 0x1f, 0xfd, 0x40, 0x59, 0xba, 0xda, 0x1e, 0x25, 0x19, 0x0f, 0x6e,
        0x98, 0xc9, 0x9d, 0x97, 0x14, 0xd3, 0x34, 0xef, 0xa4, 0x1a, 0x19, 0x5a, 0x7e, 0x7e, 0x04,
        0xbf, 0xe2,
    ],
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
    min_tx_fee: FeeRate::from_sat_per_kwu(2_500),
    fee_market: FeeMarket::Auction(Auction {
        target_blocks: [24, 6, 2],
        block_vsize: 1_000_000,
        api_route: "/v1/fees/recommended",
        api_fallback_route: None,
    }),
    replacement: Replacement::WhenConfigured,
    bump_method: Some(BumpMethod::ChildPaysForParent),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    pegout_maturity: 6,
    address_notes: &[AddressNote {
        prefix: "3",
        kind: TxWarningKind::AmbiguousAddress,
        message: "This legacy 3... address is valid on both Litecoin and Bitcoin. Confirm the recipient expects Litecoin before sending.",
    }],
    address_refusals: &[AddressRefusal {
        prefix: "ltcmweb",
        message: "Jelly cannot send to or scan Litecoin MWEB addresses. Use a regular Litecoin address.",
    }],
    fee_ceiling: Amount::from_base_units(2_000_000),
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250),
};

pub const BITCOIN_CASH: UtxoParams = UtxoParams {
    chain: ChainId::BitcoinCash,
    codec: &UtxoCodec {
        chain: ChainId::BitcoinCash,
        segwit_hrp: None,
        base58: Base58Versions { p2pkh: 0x00, p2sh: 0x05, superseded_p2sh: None },
    },
    script_kinds: &[ScriptKind::P2pkh],
    fee_rate_unit: "sat/B",
    genesis: BITCOIN.genesis,
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
    incremental_relay_fee: FeeRate::from_sat_per_kwu(250),
    max_money: Amount::from_base_units(21_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    min_tx_fee: FeeRate::from_sat_per_kwu(250),
    fee_market: FeeMarket::Flat,
    replacement: Replacement::Never,
    bump_method: None,
    sighash_fork_id: Some(0),
    has_cash_tokens: true,
    has_slp_tokens: true,
    confirmation_watch_depth: 6,
    coinbase_maturity: 100,
    pegout_maturity: 0,
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
    fee_ceiling: Amount::from_base_units(2_000_000),
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250),
};

pub const DOGECOIN: UtxoParams = UtxoParams {
    chain: ChainId::Dogecoin,
    codec: &UtxoCodec {
        chain: ChainId::Dogecoin,
        segwit_hrp: None,
        base58: Base58Versions { p2pkh: 0x1e, p2sh: 0x16, superseded_p2sh: None },
    },
    script_kinds: &[ScriptKind::P2pkh],
    fee_rate_unit: "koinu/B",
    genesis: [
        0x1a, 0x91, 0xe3, 0xda, 0xce, 0x36, 0xe2, 0xbe, 0x3b, 0xf0, 0x30, 0xa6, 0x56, 0x79, 0xfe,
        0x82, 0x1a, 0xa1, 0xd6, 0xef, 0x92, 0xe7, 0xc9, 0x90, 0x2e, 0xb3, 0x18, 0x18, 0x2c, 0x35,
        0x56, 0x91,
    ],
    checkpoint: None,
    is_merge_mined: true,
    has_segwit: false,
    scan_stream: ScanStream::DogecoinElectrumStatus,
    dust: DustRule::Fixed(Amount::from_base_units(1_000_000)),
    min_relay_fee: FeeRate::from_sat_per_kwu(25_000),
    incremental_relay_fee: FeeRate::from_sat_per_kwu(2_500),
    max_money: Amount::from_base_units(10_000_000_000 * 100_000_000),
    max_standard_weight: Weight::from_wu(400_000),
    min_tx_fee: FeeRate::from_sat_per_kwu(250_000),
    fee_market: FeeMarket::Flat,
    replacement: Replacement::ByDefault,
    bump_method: Some(BumpMethod::Replace),
    sighash_fork_id: None,
    has_cash_tokens: false,
    has_slp_tokens: false,
    confirmation_watch_depth: 6,
    coinbase_maturity: 240,
    pegout_maturity: 0,
    address_notes: &[],
    address_refusals: &[],
    fee_ceiling: Amount::from_base_units(10_000_000_000),
    long_term_fee_rate: FeeRate::from_sat_per_kwu(250_000),
};
