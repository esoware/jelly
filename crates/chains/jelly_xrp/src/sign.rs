use jelly_core::{KeyKind, KeyMaterial, RawTx, SignedTx, TxId, UnsignedTx};
use jelly_keypair::Secp256k1Key;
use sha2::{Digest as _, Sha512};

use crate::address::{pubkey_address, xrp_destination};
use crate::config::CHAIN;
use crate::error::XrpError;
use crate::payload::{Payment, SendKind};
use crate::preview;

const SIGNING_PREFIX: [u8; 4] = *b"STX\0";
const TXID_PREFIX: [u8; 4] = *b"TXN\0";

fn sha512_half(prefix: [u8; 4], bytes: &[u8]) -> [u8; 32] {
    let digest = Sha512::new().chain_update(prefix).chain_update(bytes).finalize();
    let mut half = [0; 32];
    for (output, input) in half.iter_mut().zip(digest) {
        *output = input;
    }
    half
}

pub(crate) fn txid(bytes: &[u8]) -> String {
    hex::encode_upper(sha512_half(TXID_PREFIX, bytes))
}

/// `payment` is a validated one, so both of its addresses read.
fn serialize(
    payment: &Payment,
    pubkey: &[u8; 33],
    signature: Option<&[u8]>,
) -> Result<Vec<u8>, XrpError> {
    let account = xrp_destination(&payment.from)
        .map_err(|_| XrpError::Internal("a validated XRP payment has an unreadable sender"))?
        .account;
    let destination = xrp_destination(&payment.to)
        .map_err(|_| XrpError::Internal("a validated XRP payment has an unreadable recipient"))?
        .account;
    // The XRP Ledger's canonical binary format: each field behind its header byte, sorted by
    // type code and then field code. That puts TransactionType (Payment 0 or AccountDelete
    // 21), Flags (tfFullyCanonicalSig), Sequence, DestinationTag, LastLedgerSequence, a
    // Payment's Amount, Fee, SigningPubKey, TxnSignature, Account, and Destination in that
    // order. A native amount clears bit 63 and sets bit 62, which marks it positive.
    let (transaction_type, amount_drops) = match payment.kind {
        SendKind::Payment => (0u16, Some(payment.amount_drops)),
        SendKind::AccountDelete => (21, None),
    };
    let mut bytes = Vec::with_capacity(240);
    bytes.push(0x12);
    bytes.extend_from_slice(&transaction_type.to_be_bytes());
    bytes.push(0x22);
    bytes.extend_from_slice(&0x8000_0000u32.to_be_bytes());
    bytes.push(0x24);
    bytes.extend_from_slice(&payment.sequence.to_be_bytes());
    if let Some(tag) = payment.destination_tag {
        bytes.push(0x2e);
        bytes.extend_from_slice(&tag.to_be_bytes());
    }
    bytes.extend_from_slice(&[0x20, 0x1b]);
    bytes.extend_from_slice(&payment.last_ledger.to_be_bytes());
    if let Some(amount_drops) = amount_drops {
        bytes.push(0x61);
        bytes.extend_from_slice(&(amount_drops | 0x4000_0000_0000_0000).to_be_bytes());
    }
    bytes.push(0x68);
    bytes.extend_from_slice(&(payment.fee_drops | 0x4000_0000_0000_0000).to_be_bytes());
    bytes.extend_from_slice(&[0x73, 33]);
    bytes.extend_from_slice(pubkey);
    if let Some(signature) = signature {
        // A DER secp256k1 signature is 8 to 72 bytes, so a one-byte length prefix holds it.
        let length = u8::try_from(signature.len())
            .ok()
            .filter(|length| (8..=72).contains(length))
            .ok_or(XrpError::Internal("an XRP signature has an unexpected length"))?;
        bytes.extend_from_slice(&[0x74, length]);
        bytes.extend_from_slice(signature);
    }
    bytes.extend_from_slice(&[0x81, 20]);
    bytes.extend_from_slice(&account);
    bytes.extend_from_slice(&[0x83, 20]);
    bytes.extend_from_slice(&destination);
    Ok(bytes)
}

/// Signs the payment `tx` carries, and returns that payment beside the signed bytes.
pub(crate) fn sign(tx: &UnsignedTx, key: &KeyMaterial) -> Result<(Payment, SignedTx), XrpError> {
    let payment = Payment::decode(&tx.payload)?;
    if tx.preview != preview::of(&payment) {
        return Err(XrpError::Invalid("The XRP payment does not match its preview."));
    }
    let not_owner = XrpError::Invalid("The signing key does not belong to this XRP account.");
    if key.kind() != KeyKind::Secp256k1 {
        return Err(not_owner);
    }
    let key = key.with_bytes(Secp256k1Key::from_secret_bytes)?;
    let pubkey = key.public_compressed();
    if pubkey_address(&pubkey) != payment.from {
        return Err(not_owner);
    }
    let unsigned = serialize(&payment, &pubkey, None)?;
    let digest = sha512_half(SIGNING_PREFIX, &unsigned);
    let signature = key.sign_prehash_der(&digest)?;
    let bytes = serialize(&payment, &pubkey, Some(&signature))?;
    let txid = TxId::new(CHAIN, txid(&bytes));
    Ok((payment, SignedTx { raw: RawTx::from_bytes(bytes), txid }))
}

#[cfg(test)]
mod tests {
    use jelly_core::Amount;
    use jelly_core::test_util::key_material_of;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Vectors {
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    struct Vector {
        kind: SendKind,
        private_key: String,
        from: String,
        to: String,
        tag: Option<u32>,
        amount_drops: u64,
        fee_drops: u64,
        sequence: u32,
        last_ledger: u32,
        blob: String,
        hash: String,
    }

    fn vectors() -> Vec<Vector> {
        let vectors: Vectors =
            serde_json::from_str(include_str!("../tests/vectors/signing.json")).unwrap();
        vectors.vectors
    }

    fn unsigned(vector: &Vector) -> UnsignedTx {
        let payment = Payment {
            kind: vector.kind,
            from: vector.from.clone(),
            to: vector.to.clone(),
            destination_tag: vector.tag,
            amount_drops: vector.amount_drops,
            fee_drops: vector.fee_drops,
            sequence: vector.sequence,
            first_ledger: vector.last_ledger.saturating_sub(19),
            last_ledger: vector.last_ledger,
            reserve_drops: match vector.kind {
                SendKind::Payment => 1_000_000,
                SendKind::AccountDelete => 0,
            },
            base_reserve_drops: 1_000_000,
            is_activation: false,
        };
        UnsignedTx { preview: preview::of(&payment), payload: payment.encode().unwrap() }
    }

    fn key(vector: &Vector, kind: KeyKind) -> KeyMaterial {
        key_material_of(kind, hex::decode(&vector.private_key).unwrap())
    }

    #[track_caller]
    fn assert_refused(error: &XrpError, message: &str) {
        assert!(
            matches!(error, XrpError::Invalid(text) if *text == message),
            "expected {message:?}, got {error:?}"
        );
    }

    #[test]
    fn payments_and_account_deletions_match_xrpl_js_byte_for_byte() {
        for vector in vectors() {
            let (_, signed) = sign(&unsigned(&vector), &key(&vector, KeyKind::Secp256k1)).unwrap();
            assert_eq!(hex::encode_upper(signed.raw.as_bytes()), vector.blob);
            assert_eq!(signed.txid.value(), vector.hash);
        }
    }

    #[test]
    fn signing_refuses_a_changed_preview_or_a_foreign_key() {
        for vector in vectors() {
            let unsigned = unsigned(&vector);
            let mut changed = unsigned.clone();
            changed.preview.amount = Amount::from_base_units(20);
            assert_refused(
                &sign(&changed, &key(&vector, KeyKind::Secp256k1)).unwrap_err(),
                "The XRP payment does not match its preview.",
            );
            let other_key = key_material_of(KeyKind::Secp256k1, vec![1; 32]);
            assert_refused(
                &sign(&unsigned, &other_key).unwrap_err(),
                "The signing key does not belong to this XRP account.",
            );
            assert_refused(
                &sign(&unsigned, &key(&vector, KeyKind::Ed25519Seed)).unwrap_err(),
                "The signing key does not belong to this XRP account.",
            );
        }
    }
}
