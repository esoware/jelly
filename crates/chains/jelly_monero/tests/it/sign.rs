use std::slice::from_ref;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, ErrorCode, KeyKind, TxPayload, UnsignedTx,
    test_util::{key_material, key_material_of},
};
use jelly_monero::{MoneroConfig, MoneroKeys};
use monero_wallet::transaction::{Input, Transaction};

use crate::common::{Harness, PRIMARY_ID, bytes32, config, images_in, watched};

#[tokio::test]
async fn a_signed_transaction_is_the_transaction_that_was_previewed() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();

    let mut bytes = signed.raw.as_bytes();
    let parsed = Transaction::read(&mut bytes).unwrap();
    assert!(bytes.is_empty(), "nothing follows the transaction");
    assert_eq!(parsed.serialize(), signed.raw.as_bytes(), "the round trip is byte for byte");
    assert_eq!(hex::encode(parsed.hash()), signed.txid.value());

    let prefix = parsed.prefix();
    assert_eq!(prefix.inputs.len(), 1, "one input covers this amount");
    assert_eq!(prefix.outputs.len(), 2, "one payment and one change");
    let Some(Input::ToKey { key_offsets, .. }) = prefix.inputs.first() else {
        panic!("the input spends a ring");
    };
    assert_eq!(key_offsets.len(), 16, "a full ring at hardfork 16");

    let Transaction::V2 { proofs: Some(proofs), .. } = &parsed else {
        panic!("a signed transaction carries RingCT proofs");
    };
    assert_eq!(
        Amount::from(proofs.base.fee),
        unsigned.preview.fee,
        "the committed fee is the previewed fee"
    );
}

#[tokio::test]
async fn this_builds_key_image_derivation_agrees_with_upstreams_signing_path() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(None)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();

    let mut from_transaction = images_in(&signed).unwrap();
    from_transaction.sort_unstable();
    assert_eq!(from_transaction.len(), 2, "a sweep spends every unlocked output");

    let mut derived = harness.own_key_images();
    derived.sort_unstable();
    assert_eq!(
        from_transaction, derived,
        "upstream's signing path and this crate's derivation must agree"
    );
}

/// Every refusal here is a signing failure, so each is also told apart by what it
/// says.
fn assert_refused_for(error: &ChainError, reason: &str) {
    assert_eq!(error.code(), ErrorCode::Signing, "{error}");
    assert!(error.to_string().contains(reason), "expected {reason:?} in {error}");
}

#[tokio::test]
async fn a_key_from_another_wallet_or_of_another_kind_cannot_sign() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();

    let mut other = bytes32(&harness.recorded.spend_key).unwrap();
    if let Some(byte) = other.first_mut() {
        *byte ^= 1;
    }
    let wrong_wallet = key_material_of(KeyKind::MoneroSpend, other.to_vec());
    assert_refused_for(
        &harness.adapter.sign_tx(&unsigned, from_ref(&wrong_wallet)).expect_err("wrong wallet"),
        "does not belong",
    );

    let wrong_kind = key_material(KeyKind::Ed25519Seed, 7);
    assert_refused_for(
        &harness.adapter.sign_tx(&unsigned, from_ref(&wrong_kind)).expect_err("wrong kind"),
        "a monero spend key was expected, not a",
    );

    let wrong_length = key_material_of(KeyKind::MoneroSpend, vec![7u8; 31]);
    assert_refused_for(
        &harness.adapter.sign_tx(&unsigned, from_ref(&wrong_length)).expect_err("wrong length"),
        "32 bytes, not 31",
    );
}

#[tokio::test]
async fn a_payload_from_elsewhere_is_refused() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();

    let foreign = UnsignedTx {
        preview: unsigned.preview.clone(),
        payload: TxPayload::from_bytes(b"not ours at all".to_vec()),
    };
    assert_refused_for(
        &harness.adapter.sign_tx(&foreign, &[harness.spend_key()]).expect_err("foreign"),
        "lacks this build's header",
    );

    let mut truncated = unsigned.payload.as_bytes().to_vec();
    truncated.truncate(20);
    let short =
        UnsignedTx { preview: unsigned.preview.clone(), payload: TxPayload::from_bytes(truncated) };
    assert_refused_for(
        &harness.adapter.sign_tx(&short, &[harness.spend_key()]).expect_err("truncated"),
        "truncated",
    );

    let mut trailing = unsigned.payload.as_bytes().to_vec();
    trailing.push(0);
    let extra = UnsignedTx { preview: unsigned.preview, payload: TxPayload::from_bytes(trailing) };
    assert_refused_for(
        &harness.adapter.sign_tx(&extra, &[harness.spend_key()]).expect_err("trailing"),
        "trailing bytes",
    );
}

#[tokio::test]
async fn a_ring_chosen_too_long_ago_is_refused_rather_than_signed() {
    let harness =
        Harness::with(MoneroConfig { max_payload_age_blocks: 2, ..config() }).await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    // The chain moves on, so the ring this payload carries is stale.
    harness.node.set_tip(harness.tip.saturating_add(50));
    harness.sync().await.unwrap();

    let error = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).expect_err("stale");
    assert_eq!(error.code(), ErrorCode::Signing, "{error}");
    assert!(error.to_string().contains("past the limit of 2"), "{error}");
}

/// A send from a subaddress is signed with the key the wallet holds for that subaddress.
///
/// The wallet signs a send with the key behind the row it is sent from, and for a
/// subaddress row the chain entry hands out that subaddress's spend key, `ks + m`. The
/// payload names the subaddress, so that key is checked against it rather than refused
/// for not being the account's own spend key.
#[tokio::test]
async fn a_send_from_a_subaddress_is_signed_with_the_key_the_wallet_holds_for_it() {
    const SUBADDRESS_ID: i64 = 8;
    let harness = Harness::new().await.unwrap();
    let subaddress = harness.account.address(0, 1).unwrap();
    harness
        .adapter
        .set_watch_set(&[
            watched(PRIMARY_ID, 0, 0, &harness.primary),
            watched(SUBADDRESS_ID, 0, 1, &subaddress),
        ])
        .await
        .unwrap();
    let mut request = harness.request(Some(1_000));
    request.from_address_ids = vec![SUBADDRESS_ID];
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();

    let keys = MoneroKeys::from_spend_bytes(&bytes32(&harness.recorded.spend_key).unwrap());
    let row_key = keys.key_material_at(0, 1);
    let signed = harness.adapter.sign_tx(&unsigned, from_ref(&row_key));
    assert!(
        signed.is_ok(),
        "expected the key behind subaddress (0, 1) to sign a send from (0, 1), got {:?}",
        signed.err()
    );
}
