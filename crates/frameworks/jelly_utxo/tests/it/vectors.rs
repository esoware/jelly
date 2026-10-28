//! Known-answer tests. Every expectation here comes from a published
//! specification, from a chain's own constants, or from two independent
//! implementations agreeing.

use bitcoin::consensus::{Encodable as _, deserialize};
use bitcoin::hashes::{Hash as _, HashEngine as _, sha256};
use bitcoin::opcodes::all::{OP_EQUAL, OP_HASH256};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::{Amount as BtcAmount, Network, ScriptBuf, Transaction, TxOut, WScriptHash};
use jelly_core::{Amount, ScriptKind};
use jelly_utxo::{DustRule, UtxoParams, test_util::test_key};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::common::{BITCOIN, BITCOIN_CASH, DOGECOIN, Fallible, LITECOIN};

const BIP341: &str = include_str!("../vectors/bip341.json");
const DUST: &str = include_str!("../vectors/dust.json");

fn load<T: DeserializeOwned>(text: &str) -> Fallible<T> {
    Ok(serde_json::from_str(text)?)
}

fn bytes(text: &str) -> Fallible<Vec<u8>> {
    Ok(hex::decode(text)?)
}

fn params_of(name: &str) -> Option<UtxoParams> {
    match name {
        "bitcoin" => Some(BITCOIN),
        "litecoin" => Some(LITECOIN),
        "bitcoin_cash" => Some(BITCOIN_CASH),
        "dogecoin" => Some(DOGECOIN),
        _ => None,
    }
}

/// One script of each standard form, from the Bitcoin fixture key where a key
/// produces one. A script's bytes are the same on every chain.
fn scripts() -> Fallible<Vec<(&'static str, ScriptBuf)>> {
    let mut scripts = Vec::new();
    for (name, kind) in [
        ("p2pkh", ScriptKind::P2pkh),
        ("p2sh", ScriptKind::P2shP2wpkh),
        ("p2wpkh", ScriptKind::P2wpkh),
    ] {
        scripts.push((name, test_key(&BITCOIN, kind, 1)?.script_pubkey()));
    }
    scripts.push(("p2wsh", ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51]))));
    scripts.push(("p2tr", test_key(&BITCOIN, ScriptKind::P2tr, 1)?.script_pubkey()));
    Ok(scripts)
}

/// Bitcoin Cash's 32-byte script hash form, which no other chain has.
fn p2sh32() -> ScriptBuf {
    ScriptBuf::builder()
        .push_opcode(OP_HASH256)
        .push_slice([0x51; 32])
        .push_opcode(OP_EQUAL)
        .into_script()
}

mod bip341 {
    use super::*;

    #[derive(Deserialize)]
    struct File {
        cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        unsigned_tx: String,
        input_index: usize,
        prevouts: Vec<Prevout>,
        sighash: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Prevout {
        value: u64,
        script_pubkey: String,
    }

    fn prevouts(case: &Case) -> Fallible<Vec<TxOut>> {
        case.prevouts
            .iter()
            .map(|prevout| {
                Ok(TxOut {
                    value: BtcAmount::from_sat(prevout.value),
                    script_pubkey: ScriptBuf::from(bytes(&prevout.script_pubkey)?),
                })
            })
            .collect()
    }

    /// The BIP-341 signature message written from the specification, so the
    /// pinned digest is two independent implementations agreeing.
    fn sig_msg(tx: &Transaction, prevouts: &[TxOut], index: usize) -> [u8; 32] {
        let mut sha_prevouts = sha256::Hash::engine();
        let mut sha_amounts = sha256::Hash::engine();
        let mut sha_scripts = sha256::Hash::engine();
        let mut sha_sequences = sha256::Hash::engine();
        for (txin, previous) in tx.input.iter().zip(prevouts) {
            let _ = txin.previous_output.consensus_encode(&mut sha_prevouts);
            let _ = previous.value.consensus_encode(&mut sha_amounts);
            let _ = previous.script_pubkey.consensus_encode(&mut sha_scripts);
            let _ = txin.sequence.consensus_encode(&mut sha_sequences);
        }
        let mut sha_outputs = sha256::Hash::engine();
        for txout in &tx.output {
            let _ = txout.consensus_encode(&mut sha_outputs);
        }

        let mut message: Vec<u8> = Vec::new();
        message.push(0x00);
        message.push(0x00);
        message.extend_from_slice(&tx.version.0.to_le_bytes());
        message.extend_from_slice(&tx.lock_time.to_consensus_u32().to_le_bytes());
        message.extend_from_slice(&sha256::Hash::from_engine(sha_prevouts).to_byte_array());
        message.extend_from_slice(&sha256::Hash::from_engine(sha_amounts).to_byte_array());
        message.extend_from_slice(&sha256::Hash::from_engine(sha_scripts).to_byte_array());
        message.extend_from_slice(&sha256::Hash::from_engine(sha_sequences).to_byte_array());
        message.extend_from_slice(&sha256::Hash::from_engine(sha_outputs).to_byte_array());
        message.push(0x00);
        message.extend_from_slice(&u32::try_from(index).unwrap_or_default().to_le_bytes());

        let tag = sha256::Hash::hash(b"TapSighash").to_byte_array();
        let mut engine = sha256::Hash::engine();
        engine.input(&tag);
        engine.input(&tag);
        engine.input(&message);
        sha256::Hash::from_engine(engine).to_byte_array()
    }

    #[test]
    fn the_key_path_sighash_agrees_with_an_independent_implementation() {
        let file: File = load(BIP341).unwrap();
        assert_eq!(file.cases.len(), 1);
        for case in &file.cases {
            let tx: Transaction = deserialize(&bytes(&case.unsigned_tx).unwrap()).unwrap();
            let prevouts = prevouts(case).unwrap();
            let mut cache = SighashCache::new(&tx);
            let sighash = cache
                .taproot_key_spend_signature_hash(
                    case.input_index,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .unwrap();
            assert_eq!(
                sighash.to_byte_array(),
                sig_msg(&tx, &prevouts, case.input_index),
                "{}: the two implementations disagree",
                case.name
            );
            assert_eq!(hex::encode(sighash.to_byte_array()), case.sighash, "{}", case.name);
        }
    }
}

mod dust {
    use super::*;

    #[derive(Deserialize)]
    struct File {
        chains: Vec<Chain>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Chain {
        #[serde(rename = "chain")]
        id: String,
        dust_relay_sat_per_kwu: Option<u64>,
        fixed_dust: Option<u64>,
        thresholds: Vec<Threshold>,
    }

    #[derive(Deserialize)]
    struct Threshold {
        script: String,
        value: u64,
    }

    /// Litecoin's thresholds are ten times Bitcoin's, and `Script::minimal_non_dust`
    /// would give Bitcoin's on both, so the rule has to come from the table.
    #[test]
    fn the_dust_table_matches_every_chain() {
        let file: File = load(DUST).unwrap();
        assert_eq!(file.chains.len(), 4);
        let mut scripts = scripts().unwrap();
        scripts.push(("p2sh32", p2sh32()));
        for chain in &file.chains {
            let params = params_of(&chain.id).expect("a chain the fixtures know");
            match params.dust {
                DustRule::RelayFee(rate) => {
                    assert_eq!(
                        Some(rate.sat_per_kwu()),
                        chain.dust_relay_sat_per_kwu,
                        "{}",
                        chain.id
                    );
                }
                DustRule::Fixed(limit) => {
                    let fixed = chain.fixed_dust.map(u128::from);
                    assert_eq!(Some(limit.base_units()), fixed, "{}", chain.id);
                }
            }
            assert!(!chain.thresholds.is_empty(), "{}", chain.id);
            for entry in &chain.thresholds {
                let script = scripts
                    .iter()
                    .find(|(name, _)| *name == entry.script)
                    .map(|(_, script)| script)
                    .expect("a script the fixtures know");
                let address = params.address_of(script.as_script()).expect("the chain pays it");
                assert_eq!(params.script_pubkey(&address).unwrap(), *script, "{}", chain.id);
                assert_eq!(
                    params.dust_threshold(script.as_script()),
                    Amount::from(entry.value),
                    "{} {}",
                    chain.id,
                    entry.script
                );
            }
        }
    }

    /// The recorded figures checked against each other and against the bitcoin
    /// crate's own default rate, rather than against the tables.
    #[test]
    fn the_dust_thresholds_are_each_cores_own_and_litecoins_are_ten_times_bitcoins() {
        let file: File = load(DUST).unwrap();
        let chain = |id: &str| file.chains.iter().find(|chain| chain.id == id);
        let bitcoin = chain("bitcoin").expect("the bitcoin row");
        let litecoin = chain("litecoin").expect("the litecoin row");
        assert_eq!(
            litecoin.dust_relay_sat_per_kwu,
            bitcoin.dust_relay_sat_per_kwu.map(|rate| rate * 10)
        );

        let scripts = scripts().unwrap();
        assert_eq!(bitcoin.thresholds.len(), 5);
        assert_eq!(litecoin.thresholds.len(), bitcoin.thresholds.len());
        for (on_bitcoin, on_litecoin) in bitcoin.thresholds.iter().zip(&litecoin.thresholds) {
            let name = on_bitcoin.script.as_str();
            assert_eq!(on_litecoin.script, name);
            assert_eq!(on_litecoin.value, on_bitcoin.value * 10, "{name}");
            let script = scripts
                .iter()
                .find(|(entry, _)| *entry == name)
                .map(|(_, script)| script)
                .expect("a script the fixtures know");
            assert_eq!(
                script.minimal_non_dust().to_sat(),
                on_bitcoin.value,
                "{name}: the bitcoin figure, from the bitcoin crate's own default rate"
            );
        }
    }
}

mod addresses {
    use super::*;

    /// On Bitcoin, script to address is checked against `bitcoin::Address`, a
    /// separate implementation of BIP-173, BIP-350, and `Base58Check`.
    #[test]
    fn bitcoin_renders_every_standard_script_the_way_the_bitcoin_crate_does() {
        for (name, script) in scripts().unwrap() {
            let expected =
                bitcoin::Address::from_script(script.as_script(), Network::Bitcoin).unwrap();
            assert_eq!(
                BITCOIN.address_of(script.as_script()),
                Some(expected.to_string()),
                "{name}"
            );
        }
    }

    /// On Litecoin the cross-check is the codec, so a rendered address has to
    /// decode back to its script.
    #[test]
    fn litecoin_renders_every_script_its_own_codec_can_read_back() {
        for (name, script) in scripts().unwrap() {
            let rendered = LITECOIN.address_of(script.as_script()).ok_or(name).unwrap();
            assert_eq!(LITECOIN.script_pubkey(&rendered).unwrap(), script, "{name}: {rendered}");
        }
    }

    #[test]
    fn the_prefixes_are_the_ones_each_chain_publishes() {
        let cases = [
            (&BITCOIN, "p2pkh", "1"),
            (&BITCOIN, "p2sh", "3"),
            (&BITCOIN, "p2wpkh", "bc1q"),
            (&BITCOIN, "p2wsh", "bc1q"),
            (&BITCOIN, "p2tr", "bc1p"),
            (&LITECOIN, "p2pkh", "L"),
            (&LITECOIN, "p2sh", "M"),
            (&LITECOIN, "p2wpkh", "ltc1q"),
            (&LITECOIN, "p2wsh", "ltc1q"),
            (&LITECOIN, "p2tr", "ltc1p"),
        ];
        for (params, name, prefix) in cases {
            let script = scripts()
                .unwrap()
                .into_iter()
                .find(|(entry, _)| *entry == name)
                .map(|(_, script)| script)
                .ok_or(name)
                .unwrap();
            let rendered = params.address_of(script.as_script()).ok_or(name).unwrap();
            assert!(rendered.starts_with(prefix), "{name}: {rendered} is not a {prefix} address");
        }
    }

    /// Neither chain has segregated witness, so a witness program is no address of
    /// theirs, and each writes its own legacy forms. Only Bitcoin Cash has the
    /// 32-byte script hash.
    #[test]
    fn the_chains_without_segwit_render_their_own_forms_and_no_witness_program() {
        let scripts = scripts().unwrap();
        let script = |name: &str| {
            scripts.iter().find(|(entry, _)| *entry == name).map(|(_, script)| script.clone())
        };
        for name in ["p2wpkh", "p2wsh", "p2tr"] {
            let witness = script(name).unwrap();
            assert_eq!(BITCOIN_CASH.address_of(witness.as_script()), None, "{name}");
            assert_eq!(DOGECOIN.address_of(witness.as_script()), None, "{name}");
        }
        let rendered = |params: &UtxoParams, script: &ScriptBuf| {
            let address = params.address_of(script.as_script()).unwrap();
            assert_eq!(params.script_pubkey(&address).unwrap(), *script, "{address}");
            address
        };
        assert!(rendered(&BITCOIN_CASH, &script("p2pkh").unwrap()).starts_with("bitcoincash:q"));
        assert!(rendered(&BITCOIN_CASH, &script("p2sh").unwrap()).starts_with("bitcoincash:p"));
        assert!(rendered(&BITCOIN_CASH, &p2sh32()).starts_with("bitcoincash:p"));
        assert!(rendered(&DOGECOIN, &script("p2pkh").unwrap()).starts_with('D'));
        let script_hash = rendered(&DOGECOIN, &script("p2sh").unwrap());
        assert!(script_hash.starts_with(['9', 'A']), "{script_hash}");
        assert_eq!(DOGECOIN.address_of(p2sh32().as_script()), None);
        assert_eq!(BITCOIN.address_of(p2sh32().as_script()), None);
    }

    #[test]
    fn a_script_with_no_address_renders_as_nothing() {
        for script in [
            ScriptBuf::new(),
            ScriptBuf::new_op_return([1u8, 2, 3]),
            ScriptBuf::from(vec![0x51, 0x51]),
        ] {
            assert_eq!(BITCOIN.address_of(script.as_script()), None);
            assert_eq!(LITECOIN.address_of(script.as_script()), None);
        }
    }
}
