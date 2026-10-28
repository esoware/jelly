use jelly_core::{KeyError, KeyKind, KeyMaterial, RawTx, SignedTx, TxId};
use jelly_keypair::Ed25519Key;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::config::CHAIN;
use crate::error::SolanaError;
use crate::payload::SolanaPayload;

/// The largest transaction the network moves, set by the IPv6 MTU. A larger one is
/// refused everywhere, so it's checked before the user approves anything.
const MAX_TRANSACTION_BYTES: usize = 1232;

/// What one signature and its length prefix add to a serialized message.
pub(crate) const SIGNATURE_OVERHEAD_BYTES: usize = 65;

/// Signs the payload's message bytes, which are the signing preimage, so the signature
/// covers exactly what the preview decoded.
///
/// Ed25519 signing is deterministic, so re-signing the same payload yields the same
/// transaction, not a second one.
pub(crate) fn sign(payload: &SolanaPayload, key: &KeyMaterial) -> Result<SignedTx, SolanaError> {
    if key.kind() != KeyKind::Ed25519Seed {
        return Err(SolanaError::WrongKeyKind { got: key.kind() });
    }
    let required = payload.signatures_required();
    if required != 1 {
        return Err(SolanaError::SignerCount { required });
    }
    let signer = Ed25519Key::from_key_material(key).map_err(|error| key_failure(&error, key))?;
    if Pubkey::from(signer.public_bytes()) != payload.payer() {
        return Err(SolanaError::KeyNotOwner);
    }
    let signature = Signature::from(signer.sign(payload.message_bytes()));

    let transaction =
        VersionedTransaction { signatures: vec![signature], message: payload.message().clone() };
    let raw = wincode::serialize(&transaction)
        .map_err(|_| SolanaError::Encode { what: "signed transaction" })?;
    check_size(raw.len())?;
    Ok(SignedTx { raw: RawTx::from_bytes(raw), txid: TxId::new(CHAIN, signature.to_string()) })
}

pub(crate) const fn check_size(bytes: usize) -> Result<(), SolanaError> {
    if bytes > MAX_TRANSACTION_BYTES {
        return Err(SolanaError::TransactionTooLarge {
            bytes,
            maximum_bytes: MAX_TRANSACTION_BYTES,
        });
    }
    Ok(())
}

pub(crate) const fn signed_size(message_bytes: usize) -> usize {
    message_bytes.saturating_add(SIGNATURE_OVERHEAD_BYTES)
}

/// The kind is checked before the key is read, so a seed of the wrong length is
/// the one refusal left.
const fn key_failure(error: &KeyError, key: &KeyMaterial) -> SolanaError {
    if let KeyError::InvalidKeyLength { got } = *error {
        SolanaError::WrongKeyLength { got }
    } else {
        SolanaError::WrongKeyKind { got: key.kind() }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use solana_signer::Signer as _;
    use zeroize::Zeroizing;

    use super::*;

    /// Fixed seeds over fixed messages, produced by an implementation this crate doesn't
    /// link. RFC 8032 signing is deterministic, so these are exact.
    const VECTORS: &str = include_str!("../tests/vectors/signing.json");

    #[derive(Debug, Deserialize)]
    struct Vectors {
        cases: Vec<Case>,
    }

    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        seed: String,
        message: String,
        pubkey: String,
        signature: String,
    }

    fn seed_key(seed: &[u8]) -> KeyMaterial {
        KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(seed.to_vec()))
    }

    /// This crate signs with the workspace's own signer, checked here against the
    /// vectors and the upstream keypair.
    #[test]
    fn every_pinned_signature_matches_the_signer_this_crate_uses_and_the_reference_signer() {
        let vectors: Vectors = serde_json::from_str(VECTORS).expect("the vector file parses");
        assert!(!vectors.cases.is_empty());
        for case in vectors.cases {
            let seed_bytes = hex::decode(&case.seed).expect("the seed is hex");
            let seed: [u8; 32] = seed_bytes.clone().try_into().expect("a thirty-two byte seed");
            let message = hex::decode(&case.message).expect("the message is hex");

            let signer = Ed25519Key::from_key_material(&seed_key(&seed_bytes)).expect("a key");
            let pubkey = Pubkey::from(signer.public_bytes());
            let signature = Signature::from(signer.sign(&message));
            assert_eq!(pubkey.to_string(), case.pubkey, "{}", case.name);
            assert_eq!(signature.to_string(), case.signature, "{}", case.name);

            let reference = solana_keypair::Keypair::new_from_array(seed);
            assert_eq!(reference.pubkey().to_string(), case.pubkey, "{}", case.name);
            assert_eq!(
                reference.sign_message(&message).to_string(),
                case.signature,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn a_seed_of_the_wrong_length_is_refused_by_its_length() {
        for length in [0usize, 31, 33, 64] {
            let key = seed_key(&vec![7u8; length]);
            let error = Ed25519Key::from_key_material(&key).expect_err("wrong length");
            assert!(
                matches!(key_failure(&error, &key), SolanaError::WrongKeyLength { got } if got == length),
                "{length}"
            );
        }
    }

    #[test]
    fn the_packet_limit_is_the_protocol_one_and_is_checked_by_size() {
        assert_eq!(MAX_TRANSACTION_BYTES, 1232);
        check_size(MAX_TRANSACTION_BYTES).unwrap();
        let error = check_size(MAX_TRANSACTION_BYTES.saturating_add(1)).expect_err("too large");
        assert!(matches!(error, SolanaError::TransactionTooLarge { .. }), "{error:?}");
        assert_eq!(signed_size(100), 165);
    }
}
