//! Signing makes no network call.
//!
//! TRON signs `sha256(raw_data)` with secp256k1. This build appends `r || s ||
//! recovery_id` with the raw `0..=3` recovery id, which java-tron accepts
//! alongside the form offset by 27.
//!
//! The digest is the transaction id, so the id is known before broadcast and an
//! ambiguous send can be looked up by it.

use jelly_core::{KeyKind, KeyMaterial, RawTx, SignedTx, TxId, UnsignedTx};
use jelly_keypair::Secp256k1Key;

use crate::address;
use crate::config::CHAIN;
use crate::error::TronError;
use crate::payload::{Facts, TronPayload};
use crate::raw;

/// Refuses a payload that does not decode, facts that differ from the preview, and
/// a key that does not hold the owner address inside the signed bytes.
///
/// Answers the facts the signed bytes state alongside them, so following the send
/// needs no second decode.
pub(crate) fn sign(
    unsigned: &UnsignedTx,
    key: &KeyMaterial,
) -> Result<(Facts, SignedTx), TronError> {
    if key.kind() != KeyKind::Secp256k1 {
        return Err(TronError::WrongKeyKind { got: key.kind() });
    }
    let payload = TronPayload::decode(&unsigned.payload)?;
    let facts = payload.facts()?;
    matches_preview(unsigned, &facts)?;

    let signer = key.with_bytes(Secp256k1Key::from_secret_bytes)?;
    if address::from_pubkey(&signer.public_uncompressed())? != facts.owner {
        return Err(TronError::KeyNotOwner);
    }

    let digest = payload.txid();
    let signature = signer.sign_prehash_recoverable(&digest)?;
    let signed = SignedTx {
        raw: RawTx::from_bytes(raw::signed_bytes(payload.raw_data(), &signature)),
        txid: TxId::new(CHAIN, hex::encode(digest)),
    };
    Ok((facts, signed))
}

/// Nothing may be signed that was not previewed.
fn matches_preview(unsigned: &UnsignedTx, facts: &Facts) -> Result<(), TronError> {
    let preview = &unsigned.preview;
    if preview.chain != CHAIN {
        return Err(TronError::ForeignChain { chain: preview.chain });
    }
    if preview.from != [facts.owner.as_str()]
        || preview.to != facts.recipient
        || preview.amount != facts.amount
        || preview.asset != facts.asset
    {
        return Err(TronError::PreviewMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use jelly_core::{
        Amount, AssetId, ChainId, TokenId, TxPayload, TxPreview,
        test_util::{key_material, key_material_of},
    };

    use super::*;
    use crate::fixtures::{body, header};
    use crate::test_util::{OWNER, OWNER_KEY, RECIPIENT};

    fn material(secret: [u8; 32]) -> KeyMaterial {
        key_material_of(KeyKind::Secp256k1, secret.to_vec())
    }

    fn unsigned(from: &str, to: &str, amount: u128) -> UnsignedTx {
        let raw_data = raw::transfer(
            &header(),
            &body(from),
            &body(to),
            u64::try_from(amount).expect("a sun amount"),
        )
        .expect("a transfer builds");
        let payload = TronPayload::new(raw_data);
        let facts = payload.facts().expect("it reads back");
        let preview = TxPreview {
            chain: ChainId::Tron,
            asset: facts.asset,
            from: vec![facts.owner.clone()],
            to: facts.recipient.clone(),
            destination_tag: None,
            amount: facts.amount,
            fee: Amount::ZERO,
            details: Vec::new(),
            warnings: Vec::new(),
        };
        UnsignedTx { preview, payload: payload.encode() }
    }

    #[test]
    fn signing_is_deterministic_and_the_id_is_the_digest_that_was_signed() {
        let tx = unsigned(OWNER, RECIPIENT, 1_000_000);
        let payload = TronPayload::decode(&tx.payload).expect("a payload");
        let (facts, first) = sign(&tx, &material(OWNER_KEY)).expect("a signature");
        let (_, second) = sign(&tx, &material(OWNER_KEY)).expect("a signature");
        assert_eq!(
            first.raw.as_bytes(),
            second.raw.as_bytes(),
            "rfc-6979 nonces are deterministic"
        );
        assert_eq!(first.txid.value(), payload.txid_hex());
        assert_eq!(first.chain(), ChainId::Tron);
        assert_eq!(facts, payload.facts().expect("it reads back"), "the facts of the signed bytes");
        let wire = hex::encode(first.raw.as_bytes());
        assert!(wire.contains(&hex::encode(payload.raw_data())), "raw_data travels verbatim");
    }

    #[test]
    fn the_recovery_id_is_raw_rather_than_ethereums_offset_v() {
        let tx = unsigned(OWNER, RECIPIENT, 1);
        let (_, signed) = sign(&tx, &material(OWNER_KEY)).expect("a signature");
        let (_, signature) = raw::signed_parts(signed.raw.as_bytes()).expect("one signature");
        assert!(signature.last().is_some_and(|byte| *byte <= 3), "a raw recovery id");
    }

    #[test]
    fn a_key_that_does_not_hold_the_owner_address_is_refused() {
        // The transaction spends from RECIPIENT, and the key holds OWNER.
        let tx = unsigned(RECIPIENT, OWNER, 1_000_000);
        let error = sign(&tx, &material(OWNER_KEY)).expect_err("the signer must hold the owner");
        assert!(matches!(error, TronError::KeyNotOwner), "{error:?}");
    }

    #[test]
    fn a_key_on_another_curve_or_of_the_wrong_length_is_refused() {
        let tx = unsigned(OWNER, RECIPIENT, 1);
        let ed25519 = key_material(KeyKind::Ed25519Seed, 7);
        assert!(matches!(
            sign(&tx, &ed25519),
            Err(TronError::WrongKeyKind { got: KeyKind::Ed25519Seed })
        ));

        let zero = key_material(KeyKind::Secp256k1, 0);
        assert!(
            matches!(sign(&tx, &zero), Err(TronError::Signing(_))),
            "zero is not a valid secret key"
        );
        let short = key_material_of(KeyKind::Secp256k1, vec![1u8; 31]);
        assert!(
            matches!(sign(&tx, &short), Err(TronError::Signing(_))),
            "a secret key is thirty-two bytes"
        );
    }

    #[test]
    fn nothing_may_be_signed_that_the_preview_does_not_state() {
        let mut tx = unsigned(OWNER, RECIPIENT, 1_000_000);
        tx.preview.amount = Amount::from_base_units(2_000_000);
        let error = sign(&tx, &material(OWNER_KEY)).expect_err("only what was previewed");
        assert!(matches!(error, TronError::PreviewMismatch), "{error:?}");

        let mut wrong_asset = unsigned(OWNER, RECIPIENT, 1_000_000);
        wrong_asset.preview.asset = AssetId::Token(TokenId::UsdtTrc20);
        assert!(matches!(
            sign(&wrong_asset, &material(OWNER_KEY)),
            Err(TronError::PreviewMismatch)
        ));

        let mut foreign = unsigned(OWNER, RECIPIENT, 1_000_000);
        foreign.preview.chain = ChainId::Ethereum;
        assert!(matches!(
            sign(&foreign, &material(OWNER_KEY)),
            Err(TronError::ForeignChain { chain: ChainId::Ethereum })
        ));
    }

    #[test]
    fn a_payload_this_build_did_not_produce_is_refused_before_a_key_is_touched() {
        let mut tx = unsigned(OWNER, RECIPIENT, 1_000_000);
        let raw_data = TronPayload::decode(&tx.payload).expect("a payload").raw_data().to_vec();
        tx.payload = TxPayload::from_bytes(raw_data);
        // Zero is not a secret key, so reading the key first would fail as Signing.
        let unusable = key_material(KeyKind::Secp256k1, 0);
        let error = sign(&tx, &unusable).expect_err("an unframed payload");
        assert!(matches!(error, TronError::Payload { .. }), "{error:?}");
    }
}
