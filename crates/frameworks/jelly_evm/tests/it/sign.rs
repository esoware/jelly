use std::fs;
use std::slice::from_ref;

use alloy::consensus::SignableTransaction as _;
use alloy::consensus::TxEip1559;
use alloy::hex;
use alloy::primitives::{TxKind, U256};
use jelly_core::{
    ChainAdapter as _, ChainError, ChainId, KeyKind, TxPayload, UnsignedTx,
    test_util::{key_material as fill_key, key_material_of},
};
use jelly_evm::Method;
use serde_json::Value;

use crate::common::{
    Answer, Fallible, GWEI, Harness, OTHER_KEY, PEER, Setup, TEST_KEY, key_material, payment,
    pinned, usdt, word,
};

fn vectors() -> Fallible<Value> {
    let path = format!("{}/tests/vectors/signing.json", env!("CARGO_MANIFEST_DIR"));
    Ok(serde_json::from_str(&fs::read_to_string(&path)?)?)
}

/// A harness watching the address the test key owns.
async fn signer_wallet(token_units: Option<u128>) -> Fallible<Harness> {
    let harness = Harness::signer(Setup::default(), 0).await?;
    if let Some(units) = token_units {
        harness.node.always(Method::Call, Answer::ok(&word(units)));
    }
    Ok(harness)
}

/// The native transfer the vectors pin: one coin at nonce 7, with a 30 gwei cap and
/// a 1 gwei tip.
async fn native_build(harness: &Harness) -> Fallible<UnsignedTx> {
    Ok(harness.adapter.build_tx(&payment(PEER, 7)).await?)
}

/// The token transfer the vectors pin: 2.5 USDT at nonce 12, with a 40 gwei cap and
/// a 2 gwei tip.
async fn token_build(harness: &Harness) -> Fallible<UnsignedTx> {
    let mut request = pinned(PEER, usdt(), 2_500_000, 40 * GWEI, 2 * GWEI, 65_000);
    request.nonce = Some(12);
    Ok(harness.adapter.build_tx(&request).await?)
}

fn consensus(unsigned: &UnsignedTx) -> Fallible<TxEip1559> {
    #[derive(serde::Deserialize)]
    struct Wire {
        tx: TxEip1559,
    }
    let wire: Wire = serde_json::from_slice(unsigned.payload.as_bytes())?;
    Ok(wire.tx)
}

#[tokio::test]
async fn the_pinned_vectors_reproduce_byte_for_byte() {
    let file = vectors().unwrap();
    let cases = file.get("cases").and_then(Value::as_array).expect("a case table");
    assert_eq!(cases.len(), 2);

    for case in cases {
        let field = |key: &str| case.get(key).and_then(Value::as_str).expect(key);
        let name = field("name");
        let harness = signer_wallet(Some(10_000_000)).await.unwrap();
        let unsigned = match field("kind") {
            "native" => native_build(&harness).await.unwrap(),
            "token" => token_build(&harness).await.unwrap(),
            other => panic!("{name}: no vector kind {other}"),
        };
        assert_eq!(unsigned.preview.from, [field("address")], "{name}: the sender the key derives");

        let tx = consensus(&unsigned).unwrap();
        assert_eq!(
            format!("0x{}", hex::encode(tx.signature_hash())),
            field("signatureHash"),
            "{name}: the digest the signer signs"
        );

        let key = key_material(field("secretKey")).unwrap();
        let signed = harness.adapter.sign_tx(&unsigned, from_ref(&key)).unwrap();
        assert_eq!(
            format!("0x{}", hex::encode(signed.raw.as_bytes())),
            field("rawTx"),
            "{name}: the eip-2718 wire encoding"
        );
        assert_eq!(
            signed.txid.value(),
            field("txid"),
            "{name}: keccak256 over exactly those bytes"
        );
        assert_eq!(signed.chain(), ChainId::Ethereum);
        assert_eq!(signed.raw.as_bytes().first(), Some(&0x02), "a typed transaction");
    }
}

#[tokio::test]
async fn signing_refuses_a_key_that_does_not_own_the_sender() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();
    let error = harness
        .adapter
        .sign_tx(&unsigned, &[key_material(OTHER_KEY).unwrap()])
        .expect_err("another key");
    assert!(
        matches!(error, ChainError::Signing(ref text) if text.to_string().contains("does not own"))
    );
}

#[tokio::test]
async fn signing_refuses_the_wrong_key_shape() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();

    let ed = fill_key(KeyKind::Ed25519Seed, 1);
    assert!(matches!(
        harness.adapter.sign_tx(&unsigned, from_ref(&ed)),
        Err(ChainError::Signing(_))
    ));

    let short = key_material_of(KeyKind::Secp256k1, vec![1u8; 31]);
    let error = harness.adapter.sign_tx(&unsigned, from_ref(&short)).expect_err("thirty-one bytes");
    assert!(
        matches!(error, ChainError::Signing(ref text) if text.to_string().contains("thirty-two"))
    );
}

#[tokio::test]
async fn signing_refuses_a_payload_for_a_chain_id_this_build_does_not_serve() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();

    let mut document: Value = serde_json::from_slice(unsigned.payload.as_bytes()).unwrap();
    document["tx"]["chainId"] = Value::from("0x89");
    let forged = UnsignedTx {
        preview: unsigned.preview,
        payload: TxPayload::from_bytes(serde_json::to_vec(&document).unwrap()),
    };
    let error =
        harness.adapter.sign_tx(&forged, &[key_material(TEST_KEY).unwrap()]).expect_err("polygon");
    assert!(
        matches!(error, ChainError::Unsupported(ref text) if text.to_string().contains("137")),
        "{error:?}"
    );
}

#[tokio::test]
async fn signing_refuses_a_transaction_built_for_another_chain() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();
    let mut preview = unsigned.preview.clone();
    preview.chain = ChainId::Bsc;
    let foreign = UnsignedTx { preview, payload: unsigned.payload };
    let error = harness
        .adapter
        .sign_tx(&foreign, &[key_material(TEST_KEY).unwrap()])
        .expect_err("the wrong chain");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
}

#[tokio::test]
async fn a_payload_that_is_not_this_builds_is_refused_rather_than_guessed_at() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();
    let broken = UnsignedTx {
        preview: unsigned.preview,
        payload: TxPayload::from_bytes(b"not json".to_vec()),
    };
    let error =
        harness.adapter.sign_tx(&broken, &[key_material(TEST_KEY).unwrap()]).expect_err("not json");
    assert!(matches!(error, ChainError::Internal { .. }), "{error:?}");
}

#[tokio::test]
async fn signing_fills_in_the_slot_the_build_took_rather_than_adding_one() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1, "the build holds one slot");
    harness.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY).unwrap()]).unwrap();
    assert_eq!(harness.adapter.in_flight(), 1, "signing fills the slot in, it does not add one");
}

#[tokio::test]
async fn no_signing_failure_carries_the_secret_it_was_handed() {
    let harness = signer_wallet(None).await.unwrap();
    let unsigned = native_build(&harness).await.unwrap();
    let error = harness
        .adapter
        .sign_tx(&unsigned, &[key_material(OTHER_KEY).unwrap()])
        .expect_err("the wrong owner");
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(OTHER_KEY), "{rendered}");
    assert!(!rendered.contains(TEST_KEY), "{rendered}");
    assert!(!rendered.to_ascii_lowercase().contains("4646"), "{rendered}");
}

#[tokio::test]
async fn the_payload_the_engine_holds_is_the_transaction_that_gets_signed() {
    let harness = signer_wallet(Some(10_000_000)).await.unwrap();
    let unsigned = token_build(&harness).await.unwrap();
    let tx = consensus(&unsigned).unwrap();

    assert_eq!(tx.chain_id, 1);
    assert_eq!(tx.nonce, 12);
    assert_eq!(tx.gas_limit, 65_000);
    assert_eq!(tx.max_fee_per_gas, 40 * GWEI);
    assert_eq!(tx.max_priority_fee_per_gas, 2 * GWEI);
    assert_eq!(tx.value, U256::ZERO, "a token transfer attaches no coin");
    assert!(matches!(tx.to, TxKind::Call(_)), "never a contract creation");
    assert!(tx.access_list.is_empty(), "this build never sends an access list");
    assert_eq!(tx.input.len(), 68, "a selector and two words");
}
