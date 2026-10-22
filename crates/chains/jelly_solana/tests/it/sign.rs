use std::slice::from_ref;

use jelly_core::{
    ChainAdapter as _, ChainError, ChainId, KeyKind, TxPayload, UnsignedTx,
    test_util::{key_material, key_material_of},
};
use solana_signer::Signer as _;

use crate::common::{
    self, Harness, MAX_TRANSACTION_BYTES, OWNER, OWNER_SEED, PEER, native, other_key, owner_key,
    request, wire,
};

#[tokio::test]
async fn a_signature_matches_the_reference_signer_over_the_message_it_covers() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    let transaction = wire(&signed).unwrap();

    let reference = solana_keypair::Keypair::new_from_array([OWNER_SEED; 32]);
    let expected = reference.sign_message(&transaction.message.serialize());
    assert_eq!(transaction.signatures, vec![expected]);
    assert_eq!(signed.txid.value(), expected.to_string());
    assert_eq!(signed.chain(), ChainId::Solana);
    assert!(signed.raw.as_bytes().len() <= MAX_TRANSACTION_BYTES);
}

#[tokio::test]
async fn signing_the_same_payload_twice_is_the_same_transaction() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let first = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    let second = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    assert_eq!(first.txid, second.txid);
    assert_eq!(first.raw.as_bytes(), second.raw.as_bytes());
    assert_eq!(harness.adapter.in_flight(), 1, "one transaction, one entry");
}

#[tokio::test]
async fn a_key_that_does_not_own_the_fee_payer_is_refused() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let error = harness.adapter.sign_tx(&unsigned, &[other_key()]).expect_err("the wrong key");
    assert!(matches!(error, ChainError::Signing(_)), "{error}");
    assert_eq!(harness.adapter.in_flight(), 0, "nothing is followed up");
}

#[tokio::test]
async fn a_key_on_the_wrong_curve_is_refused() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let secp = key_material(KeyKind::Secp256k1, OWNER_SEED);
    let error = harness.adapter.sign_tx(&unsigned, from_ref(&secp)).expect_err("the wrong curve");
    assert!(matches!(error, ChainError::Signing(_)), "{error}");
}

#[tokio::test]
async fn a_seed_of_the_wrong_length_is_refused() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let short = key_material_of(KeyKind::Ed25519Seed, vec![OWNER_SEED; 16]);
    let error = harness.adapter.sign_tx(&unsigned, from_ref(&short)).expect_err("a short seed");
    assert!(matches!(error, ChainError::Signing(_)), "{error}");
}

mod payloads {
    use super::*;

    fn tampered(unsigned: &UnsignedTx, edit: impl FnOnce(&mut Vec<u8>)) -> UnsignedTx {
        let mut bytes = unsigned.payload.as_bytes().to_vec();
        edit(&mut bytes);
        UnsignedTx { preview: unsigned.preview.clone(), payload: TxPayload::from_bytes(bytes) }
    }

    #[tokio::test]
    async fn a_payload_this_build_did_not_write_is_refused() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();

        let wrong_magic = tampered(&unsigned, |bytes| {
            if let Some(byte) = bytes.first_mut() {
                *byte = b'X';
            }
        });
        let error =
            harness.adapter.sign_tx(&wrong_magic, &[owner_key()]).expect_err("another magic");
        assert!(matches!(error, ChainError::Signing(_)), "{error}");

        let trailing = tampered(&unsigned, |bytes| bytes.push(0xAA));
        let error =
            harness.adapter.sign_tx(&trailing, &[owner_key()]).expect_err("a trailing byte");
        assert!(matches!(error, ChainError::Signing(_)), "{error}");

        let truncated = tampered(&unsigned, Vec::clear);
        let error = harness.adapter.sign_tx(&truncated, &[owner_key()]).expect_err("no bytes");
        assert!(matches!(error, ChainError::Signing(_)), "{error}");
    }

    #[tokio::test]
    async fn a_payload_carries_the_facts_a_record_needs() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_234)).await.unwrap();
        let bytes = unsigned.payload.as_bytes();
        assert_eq!(bytes.get(..4), Some(b"JSOL".as_slice()));
        assert_eq!(
            bytes.get(4..12).map(<[u8]>::to_vec),
            Some(common::EXPIRY_HEIGHT.to_le_bytes().to_vec())
        );
        assert_eq!(bytes.get(12..20).map(<[u8]>::to_vec), Some(1_234u64.to_le_bytes().to_vec()));
        let key = "native:solana";
        assert_eq!(bytes.get(20).copied(), Some(u8::try_from(key.len()).unwrap()));
        assert_eq!(bytes.get(21..21 + key.len()), Some(key.as_bytes()));
    }

    #[tokio::test]
    async fn a_message_wanting_more_than_one_signature_is_refused() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        // The message follows the 20-byte fixed header and the two length-prefixed
        // texts. The number of required signatures is the message header's first
        // byte, right after the version prefix of a v0 message.
        let message = 20 + 1 + "native:solana".len() + 1 + PEER.len();
        let required_signatures = message + 1;
        assert_eq!(unsigned.payload.as_bytes().get(message).copied(), Some(0x80));
        let forged = tampered(&unsigned, |bytes| {
            if let Some(byte) = bytes.get_mut(required_signatures) {
                *byte = 2;
            }
        });
        let error = harness.adapter.sign_tx(&forged, &[owner_key()]).expect_err("two signers");
        assert!(matches!(error, ChainError::Signing(_)), "{error}");
    }
}

#[tokio::test]
async fn the_fee_payer_is_the_watched_sender() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    assert_eq!(unsigned.preview.from, [OWNER]);
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    let transaction = wire(&signed).unwrap();
    let message = common::v0_of(&transaction).expect("a v0 message");
    assert_eq!(message.account_keys.first().map(ToString::to_string), Some(OWNER.to_owned()));
    assert_eq!(message.header.num_required_signatures, 1);
}
