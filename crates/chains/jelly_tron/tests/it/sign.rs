use std::slice::from_ref;
use std::time::Duration;

use jelly_core::{
    Amount, ChainAdapter as _, ChainError, KeyKind, TxPayload, UnsignedTx,
    test_util::key_material_of,
};
use jelly_tron::{
    Route, TronConfig,
    test_util::{OWNER_KEY, RECIPIENT},
};

use crate::common::{
    Answer, FakeNode, Fallible, Harness, TRX, account, base, detail, fixture, funded, key_material,
    number, raw_data_hex, ready, request, text, vector, watched,
};

async fn built(harness: &Harness) -> Fallible<UnsignedTx> {
    Ok(harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await?)
}

#[tokio::test]
async fn a_transfer_signs_to_its_pinned_bytes_end_to_end() {
    let vectors = vector("signing").unwrap();
    let case = vectors.get("tronTransfer").expect("the tron case");
    let from = text(case, "from").unwrap();
    let to = text(case, "to").unwrap();
    let amount = u128::try_from(number(case, "amountSun").unwrap()).unwrap();
    let window_millis =
        number(case, "expirationMs").unwrap().saturating_sub(number(case, "timestampMs").unwrap());
    let stamped = Duration::from_millis(u64::try_from(window_millis).unwrap());

    let fake = FakeNode::new();
    let fake = base(&fake)
        .always(Route::NowBlock, Answer::body(fixture!("now_block_trx")))
        .on_matching(Route::Account, &from, account(&from, 100_000_000))
        .on_matching(Route::Account, &to, account(&to, 1));
    let harness = Harness::with(&fake, TronConfig { expiration: stamped, ..TronConfig::default() });
    harness.adapter.set_watch_set(&[watched(1, &from)]).await.unwrap();

    let unsigned = harness.adapter.build_tx(&request(TRX, &to, amount)).await.unwrap();
    assert_eq!(raw_data_hex(&unsigned).unwrap(), text(case, "rawData").unwrap());
    assert_eq!(
        detail(&unsigned.preview.details, "Transaction id").unwrap(),
        text(case, "txid").unwrap()
    );

    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    assert_eq!(signed.txid.value(), text(case, "txid").unwrap());
    assert_eq!(hex::encode(signed.raw.as_bytes()), text(case, "signed").unwrap());
    assert!(
        text(case, "signed").unwrap().contains(&text(case, "signature").unwrap()),
        "the signed transaction carries the pinned signature"
    );
}

#[tokio::test]
async fn signing_the_same_build_twice_is_one_transaction() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = built(&harness).await.unwrap();
    let first = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    let second = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    assert_eq!(first.txid, second.txid);
    assert_eq!(first.raw.as_bytes(), second.raw.as_bytes());
    assert_eq!(harness.adapter.in_flight(), 1, "one transaction, one entry");
}

#[tokio::test]
async fn a_key_that_does_not_hold_the_spending_address_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = built(&harness).await.unwrap();
    let error =
        harness.adapter.sign_tx(&unsigned, &[key_material([0x47; 32])]).expect_err("another key");
    assert!(matches!(error, ChainError::Signing(_)), "{error:?}");
    assert_eq!(harness.adapter.in_flight(), 0, "nothing is followed");
}

#[tokio::test]
async fn a_key_on_another_curve_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = built(&harness).await.unwrap();
    let ed25519 = key_material_of(KeyKind::Ed25519Seed, vec![7u8; 32]);
    let error =
        harness.adapter.sign_tx(&unsigned, from_ref(&ed25519)).expect_err("the wrong curve");
    assert!(matches!(error, ChainError::Signing(_)), "{error:?}");
}

#[tokio::test]
async fn a_preview_that_does_not_state_the_bytes_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let mut unsigned = built(&harness).await.unwrap();
    unsigned.preview.amount = Amount::from_base_units(2_000_000);
    let error = harness
        .adapter
        .sign_tx(&unsigned, &[key_material(OWNER_KEY)])
        .expect_err("only what was previewed");
    assert!(matches!(error, ChainError::Signing(_)), "{error:?}");
    assert_eq!(harness.adapter.in_flight(), 0, "nothing is followed");
}

#[tokio::test]
async fn a_payload_this_build_did_not_write_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let mut unsigned = built(&harness).await.unwrap();
    let mut bytes = unsigned.payload.as_bytes().to_vec();
    if let Some(first) = bytes.first_mut() {
        *first ^= 0xff;
    }
    unsigned.payload = TxPayload::from_bytes(bytes);
    let error = harness
        .adapter
        .sign_tx(&unsigned, &[key_material(OWNER_KEY)])
        .expect_err("a payload with a foreign header");
    assert!(matches!(error, ChainError::Signing(_)), "{error:?}");
}
