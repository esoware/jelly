use std::sync::Arc;

use jelly_core::{KeyMaterial, RawTx, SignedTx, TxId, TxPreview};
use jelly_keypair::Ed25519Key;
use tonlib_core::cell::{ArcCell, Cell, CellBuilder};
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::version_helper::VersionHelper;

use crate::config::CHAIN;
use crate::error::TonError;
use crate::message;
use crate::payload::Payment;
use crate::preview;
use crate::wallet::{Version, ton_wallet};

pub(crate) fn normalized_hash(address: &TonAddress, body: &ArcCell) -> Result<String, TonError> {
    // TEP-467 hashes `ext_in_msg_info$10` with src `addr_none$00`, the destination, an
    // `import_fee` of zero, no `init`, and the body as a reference.
    let mut builder = CellBuilder::new();
    builder
        .store_u8(2, 2)?
        .store_u8(2, 0)?
        .store_address(address)?
        .store_u8(4, 0)?
        .store_bit(false)?
        .store_bit(true)?
        .store_reference(&Arc::clone(body))?;
    Ok(hex::encode(builder.build()?.cell_hash().as_slice()))
}

/// The signature a signed body carries. V5R1 appends its 512 bits after the signed
/// fields, and V3R2 and V4R2 put them first.
pub(crate) fn signature_of(version: Version, signed: &Cell) -> Result<Vec<u8>, TonError> {
    let mut parser = signed.parser();
    match version {
        Version::V5r1 => {
            let body_bits = signed.bit_len().checked_sub(512).ok_or(TonError::Encoding)?;
            parser.load_bits(body_bits)?;
        }
        Version::V4r2 | Version::V3r2 => {}
    }
    Ok(parser.load_bytes(64)?)
}

/// Signs `payment`, refusing unless `approved` is its preview and `key` holds its wallet.
pub(crate) fn sign(
    payment: &Payment,
    approved: &TxPreview,
    key: &KeyMaterial,
) -> Result<SignedTx, TonError> {
    let mismatch =
        || TonError::Invalid("The TON payment does not match the approved preview or signing key.");
    if *approved != preview::of(payment)? {
        return Err(mismatch());
    }
    let signer = Ed25519Key::from_key_material(key)?;
    if signer.public_bytes() != payment.pubkey {
        return Err(mismatch());
    }
    let body = message::body(payment)?;
    let signature = signer.sign(body.cell_hash().as_slice());
    let wallet = ton_wallet(&payment.pubkey, payment.version)?;
    let signed = VersionHelper::sign_msg(wallet.version, &body, &signature)?.to_arc();
    let txid = normalized_hash(&wallet.address, &signed)?;
    let external = wallet.wrap_signed_body(Arc::unwrap_or_clone(signed), payment.is_deploy)?;
    Ok(SignedTx { txid: TxId::new(CHAIN, txid), raw: RawTx::from_bytes(external.to_boc(true)?) })
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use jelly_core::{Amount, ScriptKind, TokenId};
    use serde::Deserialize;
    use tonlib_core::tlb_types::block::message::Message;

    use super::*;
    use crate::entry::ENTRY;
    use crate::fixtures::address;
    use crate::payload::Transfer;

    #[derive(Deserialize)]
    struct Vector {
        name: String,
        script: ScriptKind,
        seed: String,
        to: String,
        expected_boc: String,
        seqno: u32,
        valid_until: u32,
        amount_nanogram: u64,
        is_max: bool,
        is_bounceable: bool,
        is_deploy: bool,
        comment: Option<String>,
        jetton: Option<JettonVector>,
        normalized_hash: String,
        from: String,
        pubkey: String,
    }

    #[derive(Deserialize)]
    struct JettonVector {
        query_id: u64,
        amount: String,
        destination: String,
    }

    fn vectors() -> Vec<Vector> {
        serde_json::from_str(include_str!("../tests/vectors/signing.json")).unwrap()
    }

    /// The payment a vector signs, with the Gram it attaches even where that is not what
    /// Jelly attaches, so a reference vector of another wallet still pins the layout.
    fn payment(vector: &Vector, pubkey: [u8; 32]) -> Payment {
        let (to, transfer) = match vector.jetton {
            None => (
                address(&vector.to).to_base64_url_flags(!vector.is_bounceable, false),
                Transfer::Native {
                    amount_nanogram: vector.amount_nanogram.max(1),
                    is_bounceable: vector.is_bounceable,
                    comment: vector.comment.clone(),
                },
            ),
            Some(ref held) => (
                held.destination.clone(),
                Transfer::Jetton {
                    token: TokenId::UsdtJetton,
                    wallet: address(&vector.to).to_hex(),
                    amount: Amount::from_base_units(held.amount.parse().unwrap()),
                    query_id: held.query_id,
                    attached_nanogram: vector.amount_nanogram,
                    comment: vector.comment.clone(),
                },
            ),
        };
        Payment {
            from: vector.from.clone(),
            to,
            pubkey,
            version: Version::try_from(vector.script).unwrap(),
            fee_nanogram: 1,
            seqno: vector.seqno,
            before_lt: 0,
            valid_until: vector.valid_until,
            is_deploy: vector.is_deploy,
            is_max: vector.is_max,
            transfer,
        }
    }

    /// The vectors whose payment Jelly itself could build.
    fn own(key: &KeyMaterial, vector: &Vector) -> Option<Payment> {
        let payment = payment(vector, Ed25519Key::from_key_material(key).unwrap().public_bytes());
        payment.validate().is_ok().then_some(payment)
    }

    #[test]
    fn vector_keys_derive_their_addresses_and_match_reference_signatures_and_hashes() {
        for vector in vectors() {
            let key = ENTRY.import_key(&vector.seed, None).unwrap().material;
            assert_eq!(
                ENTRY.address(&key, Some(vector.script)).unwrap(),
                vector.from,
                "{}",
                vector.name
            );
            let signer = Ed25519Key::from_key_material(&key).unwrap();
            let pubkey = signer.public_bytes();
            assert_eq!(hex::encode(pubkey), vector.pubkey);
            let payment = payment(&vector, pubkey);
            let body = message::body(&payment).unwrap();
            let signature = signer.sign(body.cell_hash().as_slice());
            let wallet = ton_wallet(&pubkey, payment.version).unwrap();
            let signed = VersionHelper::sign_msg(wallet.version, &body, &signature).unwrap();
            assert_eq!(signature_of(payment.version, &signed).unwrap(), signature);
            let external = wallet.wrap_signed_body(signed.clone(), payment.is_deploy).unwrap();
            let actual = Message::from_cell(&external).unwrap();
            let expected = Message::from_cell(
                &Cell::from_boc(&STANDARD.decode(&vector.expected_boc).unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(actual.body.value, expected.body.value, "{}", vector.name);
            assert_eq!(
                actual.init.map(|init| init.value),
                expected.init.map(|init| init.value),
                "{}",
                vector.name
            );
            assert_eq!(
                normalized_hash(&wallet.address, &signed.to_arc()).unwrap(),
                vector.normalized_hash,
                "{}",
                vector.name
            );
        }
    }

    #[test]
    fn signing_matches_the_reference_hash_for_every_payment_jelly_builds() {
        let mut signed_count = 0;
        for vector in vectors() {
            let key = ENTRY.import_key(&vector.seed, None).unwrap().material;
            let Some(payment) = own(&key, &vector) else {
                continue;
            };
            let signed = sign(&payment, &preview::of(&payment).unwrap(), &key).unwrap();
            assert_eq!(signed.txid.value(), vector.normalized_hash, "{}", vector.name);
            signed_count += 1;
        }
        assert!(signed_count >= 15, "only {signed_count} vectors are payments Jelly builds");
    }

    #[test]
    fn a_payment_whose_preview_was_changed_is_refused() {
        for vector in vectors() {
            let key = ENTRY.import_key(&vector.seed, None).unwrap().material;
            let Some(payment) = own(&key, &vector) else {
                continue;
            };
            let mut changed = preview::of(&payment).unwrap();
            changed.amount = Amount::from_base_units(42);
            let error = sign(&payment, &changed, &key).unwrap_err();
            assert!(
                matches!(error, TonError::Invalid(text) if text.contains("approved preview")),
                "{}: {error:?}",
                vector.name
            );
        }
    }
}
