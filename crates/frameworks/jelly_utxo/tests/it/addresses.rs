//! Address vectors per chain, and the address gate as the adapter applies it to
//! recipients and watch rows.

use bitcoin::hashes::Hash as _;
use bitcoin::{ScriptBuf, WScriptHash};
use jelly_core::{ChainAdapter as _, ChainError, ScriptKind, TxWarningKind};
use jelly_keypair::{
    parse_path,
    test_util::{TEST_PHRASE, test_keychain},
};
use jelly_utxo::{
    UtxoParams,
    test_util::{claimed, spendable_kind, test_key},
};

use crate::common::{
    BITCOIN, BITCOIN_CASH, CHAINS, DOGECOIN, Harness, LITECOIN, Setup, TABLES, address_vectors,
    kind_of, payload, rows, synced, text, watched,
};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;

/// The tables whose recorded addresses derive from one public key. Litecoin's
/// derive from a phrase, which its own test reads.
const FROM_ONE_KEY: [UtxoParams; 3] = [BITCOIN, BITCOIN_CASH, DOGECOIN];

/// A valid pay-to-witness-script-hash address on the chain, which this build pays
/// but never spends from.
fn p2wsh(params: &UtxoParams) -> String {
    let script_pubkey = ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51]));
    params.address_of(&script_pubkey).unwrap_or_default()
}

#[test]
fn every_script_kind_a_table_derives_gives_the_recorded_address_and_script() {
    for params in FROM_ONE_KEY {
        let chain = params.chain;
        let file = address_vectors(&params).unwrap();
        let pubkey = hex::decode(text(&file, "pubkey")).unwrap();
        let derived = rows(&file, "/derived");
        assert_eq!(derived.len(), params.script_kinds.len(), "{chain}");

        for row in &derived {
            let name = text(row, "scriptKind");
            let kind = ScriptKind::parse(&name)
                .ok_or_else(|| format!("{name} is not a script kind"))
                .unwrap();
            let expected = text(row, "address");
            let address = params.key_address(kind, &pubkey).unwrap();
            assert_eq!(address, expected, "{chain}: {kind:?}");

            let script_pubkey = params.script_pubkey(&address).unwrap();
            assert_eq!(
                hex::encode(script_pubkey.as_bytes()),
                text(row, "scriptPubkey"),
                "{chain}: {kind:?}"
            );
            assert_eq!(params.codec.script_kind(&address), Some(kind), "{chain}");
            assert_eq!(spendable_kind(&params, &address).unwrap(), kind, "{chain}");
            assert_eq!(
                params.address_of(script_pubkey.as_script()).as_deref(),
                Some(address.as_str()),
                "{chain}: the address round trips through the script and back"
            );
        }
    }
}

#[test]
fn every_derived_litecoin_address_matches_the_cross_checked_vectors() {
    let file = address_vectors(&LITECOIN).unwrap();
    assert_eq!(text(&file, "phrase"), TEST_PHRASE);
    let keychain = test_keychain();
    let derived = rows(&file, "/derived");
    assert_eq!(derived.len(), 4);

    for row in &derived {
        let path = parse_path(&text(row, "path")).unwrap();
        let name = text(row, "scriptKind");
        let kind =
            ScriptKind::parse(&name).ok_or_else(|| format!("{name} is not a script kind")).unwrap();
        let expected = text(row, "address");

        let pubkey = keychain.secp256k1_at(&path).unwrap().public_compressed();
        assert_eq!(LITECOIN.key_address(kind, &pubkey).unwrap(), expected);
        assert_eq!(jelly_litecoin::ENTRY.validate_address(&expected).unwrap().value(), expected);

        let script_pubkey = LITECOIN.script_pubkey(&expected).unwrap();
        assert_eq!(hex::encode(script_pubkey.as_bytes()), text(row, "scriptPubkey"), "{expected}");
        assert_eq!(LITECOIN.codec.script_kind(&expected), Some(kind));
        assert_eq!(
            spendable_kind(&LITECOIN, &expected).unwrap(),
            kind,
            "the address and the script it pays agree on the script kind, which is what lets \
             the watch set take the script kind from the script"
        );
        assert_eq!(
            LITECOIN.address_of(script_pubkey.as_script()).as_deref(),
            Some(expected.as_str()),
            "the address round trips through the script and back"
        );
    }
}

#[test]
fn an_address_is_canonicalised_before_it_is_used() {
    for params in TABLES {
        for row in &rows(&address_vectors(&params).unwrap(), "/canonical") {
            let input = text(row, "input");
            assert_eq!(
                params.canonical(&input).unwrap(),
                text(row, "output"),
                "{}: {}",
                params.chain,
                text(row, "note")
            );
        }
    }
}

#[test]
fn an_address_from_another_network_or_a_broken_one_is_refused() {
    for params in TABLES {
        for row in &rows(&address_vectors(&params).unwrap(), "/invalid") {
            let address = text(row, "address");
            assert!(
                params.canonical(&address).is_err(),
                "{}: {address} was accepted but {}",
                params.chain,
                text(row, "note")
            );
        }
    }
}

#[test]
fn an_address_this_build_cannot_spend_from_is_still_a_valid_address() {
    let mut cases = vec![(LITECOIN, p2wsh(&LITECOIN))];
    for params in FROM_ONE_KEY {
        let unwatchable = rows(&address_vectors(&params).unwrap(), "/unwatchable");
        assert!(!unwatchable.is_empty(), "{}", params.chain);
        cases.extend(unwatchable.iter().map(|row| (params, text(row, "address"))));
    }
    for (params, address) in cases {
        let chain = params.chain;
        let canonical = params.canonical(&address).unwrap();
        assert_eq!(canonical, address, "{chain}");
        let script_pubkey = params.script_pubkey(&canonical).unwrap();
        assert_eq!(
            params.address_of(script_pubkey.as_script()).as_deref(),
            Some(canonical.as_str()),
            "{chain}"
        );
        assert!(spendable_kind(&params, &canonical).is_err(), "{chain}: {address}");
    }
}

#[tokio::test]
async fn the_form_litecoin_will_not_pay_is_refused_by_name() {
    let (harness, _wallet) =
        synced(Setup::on(&LITECOIN), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
    let refused = rows(&address_vectors(&LITECOIN).unwrap(), "/refused");
    assert_eq!(refused.len(), 1);
    for row in &refused {
        let address = text(row, "address");
        let note = text(row, "note");
        for spelling in [address.clone(), address.to_uppercase()] {
            let error = harness
                .adapter
                .build_tx(&harness.request(&spelling, PAYMENT_SAT, "1"))
                .await
                .err()
                .ok_or_else(|| format!("{spelling} was accepted but {note}"))
                .unwrap();
            assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
            assert!(error.to_string().contains("MWEB"), "{error}");
        }
    }
    // A refused recipient never reaches a coin, so the wallet is untouched.
    assert!(claimed(&harness.adapter).is_empty());
}

/// The recipient goes through the address gate before anything is planned, so a
/// string that is no address never reaches a coin.
#[tokio::test]
async fn a_recipient_that_is_not_an_address_is_refused_before_a_coin_is_claimed() {
    let (harness, _wallet) =
        synced(Setup::on(&LITECOIN), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&harness.request("not an address", PAYMENT_SAT, "1"))
        .await
        .expect_err("not an address");
    assert!(matches!(error, ChainError::InvalidAddress(_)), "{error}");
    assert!(claimed(&harness.adapter).is_empty());
}

#[tokio::test]
async fn a_form_litecoin_derives_no_key_for_is_still_paid() {
    let payable = rows(&address_vectors(&LITECOIN).unwrap(), "/payable");
    assert_eq!(payable.len(), 2);
    for row in &payable {
        // One wallet per row, because a build holds a claim on the coin it selected.
        let (harness, wallet) =
            synced(Setup::on(&LITECOIN), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let address = text(row, "address");
        let expected = LITECOIN.script_pubkey(&address).unwrap();
        assert_eq!(hex::encode(expected.as_bytes()), text(row, "scriptPubkey"), "{address}");

        let unsigned =
            harness.adapter.build_tx(&harness.request(&address, PAYMENT_SAT, "1")).await.unwrap();
        let payload = payload(&unsigned).unwrap();
        assert_eq!(payload.to, address);
        assert_eq!(unsigned.preview.to, address, "the preview names the recipient as written");
        assert!(
            payload.tx.output.iter().any(|txout| txout.script_pubkey == expected),
            "{address} is not the script the recipient output pays"
        );
        harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
    }
}

#[test]
fn the_only_script_kinds_each_chain_derives_are_the_ones_it_declares() {
    assert_eq!(
        BITCOIN.script_kinds,
        [ScriptKind::P2tr, ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh, ScriptKind::P2pkh],
    );
    assert_eq!(
        LITECOIN.script_kinds,
        [ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh, ScriptKind::P2pkh],
        "Litecoin has activated taproot, and the codec reads and pays a taproot address, \
         but the key layer derives no taproot key"
    );
    let pubkey = *test_key(&LITECOIN, ScriptKind::P2wpkh, 1).unwrap().pubkey();
    LITECOIN.key_address(ScriptKind::P2tr, &pubkey).unwrap_err();
    for params in CHAINS {
        for &kind in params.script_kinds {
            let derived = params.key_address(kind, &pubkey).unwrap();
            assert_eq!(params.codec.script_kind(&derived), Some(kind));
            assert_eq!(spendable_kind(&params, &derived).unwrap(), kind);
        }
    }
}

/// The same `3...` string is a valid script hash address on both chains, and only
/// Litecoin's table warns about it.
#[tokio::test]
async fn a_recipient_in_litecoins_ambiguous_legacy_form_is_paid_with_a_warning() {
    let legacy = rows(&address_vectors(&LITECOIN).unwrap(), "/canonical")
        .into_iter()
        .find_map(|row| {
            let address = text(&row, "output");
            address.starts_with('3').then_some(address)
        })
        .expect("the vectors carry a 3... address");

    for (params, is_warned) in [(LITECOIN, true), (BITCOIN, false)] {
        let (harness, _wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let unsigned =
            harness.adapter.build_tx(&harness.request(&legacy, PAYMENT_SAT, "1")).await.unwrap();
        assert_eq!(
            unsigned
                .preview
                .warnings
                .iter()
                .any(|warning| warning.kind == TxWarningKind::AmbiguousAddress),
            is_warned,
            "{}: {:?}",
            params.chain,
            unsigned.preview.warnings
        );

        // The payment goes to that script, and the payload names the address as
        // written rather than in the modern form.
        let payload = payload(&unsigned).unwrap();
        assert_eq!(payload.to, legacy);
        let expected = params.script_pubkey(&legacy).unwrap();
        assert!(
            payload.tx.output.iter().any(|txout| txout.script_pubkey == expected),
            "the recipient output pays the script the 3... address names"
        );
    }
}

#[tokio::test]
async fn a_watch_set_row_that_does_not_resolve_fails_the_whole_set() {
    for (params, other) in [(BITCOIN, LITECOIN), (LITECOIN, BITCOIN)] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let chain = params.chain;

        let foreign = test_key(&other, kind_of(&other), 1).unwrap();
        let error = harness
            .watch(&[watched(&other, 8, foreign.address())])
            .await
            .expect_err("another chain is refused");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{chain}: {error}");

        // A row from another network, or a typo that skipping would hide.
        for row in &rows(&address_vectors(&params).unwrap(), "/invalid") {
            let address = text(row, "address");
            assert!(
                harness.watch(&[watched(&params, 9, &address)]).await.is_err(),
                "{chain}: {address}"
            );
        }

        // Paying a form is not holding one. A P2WSH address is a recipient this
        // build pays but never an input it could sign, so it cannot be watched.
        let error = harness
            .watch(&[wallet.address(), watched(&params, 11, &p2wsh(&params))])
            .await
            .expect_err("an address this build cannot spend from is refused");
        assert!(error.to_string().contains("cannot spend this output type"), "{chain}: {error}");

        // Every refusal left the set that was already being watched exactly as it
        // was.
        harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_address_litecoin_refuses_by_name_cannot_be_watched_either() {
    let harness = Harness::new(Setup::on(&LITECOIN)).unwrap();
    let file = address_vectors(&LITECOIN).unwrap();
    let mweb = text(&rows(&file, "/refused")[0], "address");
    let error = harness.watch(&[watched(&LITECOIN, 10, &mweb)]).await.expect_err("mweb is refused");
    assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
    assert!(
        harness.watch(&[watched(&LITECOIN, 10, &mweb.to_uppercase())]).await.is_err(),
        "bech32 permits an all-uppercase spelling"
    );
}

/// The same output scripts spelled on both chains, which only the human-readable part
/// tells apart: taproot, pay-to-witness-script-hash, and witness version two.
const SHARED_SCRIPTS: [(&str, &str); 3] = [
    (
        "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
        "ltc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxq4arnzx",
    ),
    (
        "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
        "ltc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qmu8tk5",
    ),
    ("bc1zw508d6qejxtdg4y5r3zarvary0c5xw7k0w7t5p", "ltc1zw508d6qejxtdg4y5r3zarvary0c5xw7ktjy0v3"),
];

#[test]
fn the_two_segwit_chains_read_the_same_scripts() {
    for (bitcoin, litecoin) in SHARED_SCRIPTS {
        assert_eq!(
            BITCOIN.script_pubkey(bitcoin).unwrap(),
            LITECOIN.script_pubkey(litecoin).unwrap(),
            "{bitcoin} {litecoin}"
        );
        assert_eq!(BITCOIN.codec.script_kind(bitcoin), LITECOIN.codec.script_kind(litecoin));
    }
}

#[test]
fn one_key_has_other_addresses_but_the_same_scripts_on_litecoin_and_bitcoin() {
    let keychain = test_keychain();
    for (text, kind) in [
        ("m/84'/0'/0'/0/0", ScriptKind::P2wpkh),
        ("m/49'/0'/0'/0/0", ScriptKind::P2shP2wpkh),
        ("m/44'/0'/0'/0/0", ScriptKind::P2pkh),
    ] {
        let pubkey = keychain.secp256k1_at(&parse_path(text).unwrap()).unwrap().public_compressed();
        let bitcoin_address = BITCOIN.key_address(kind, &pubkey).unwrap();
        let litecoin_address = LITECOIN.key_address(kind, &pubkey).unwrap();
        assert_ne!(bitcoin_address, litecoin_address);
        assert_eq!(
            BITCOIN.script_pubkey(&bitcoin_address).unwrap(),
            LITECOIN.script_pubkey(&litecoin_address).unwrap(),
            "{text}"
        );
    }
}
