//! TRON spells an address three ways: the base58check `T...` form people read, the
//! 21-byte `0x41 || keccak-tail` body that protobuf and the HTTP API carry, and
//! the bare 20-byte tail in an ABI word.
//!
//! A string is parsed once, by [`canonical`] or [`body`], and later steps work on
//! fixed-size arrays.

use jelly_core::{Address, AddressCodec, AddressError, ChainId, KeyError};
use jelly_keypair::keccak_address_bytes;

use crate::config::CHAIN;
use crate::error::TronError;

/// Mainnet's prefix byte, which makes every address start with `T`.
pub(crate) const PREFIX: u8 = 0x41;

/// The address body, prefix included.
pub(crate) const BODY_BYTES: usize = 21;

/// The same 20 bytes as the key's Ethereum address.
pub(crate) const TAIL_BYTES: usize = 20;

pub(crate) const CODEC: &dyn AddressCodec = &TronCodec;

#[derive(Debug)]
struct TronCodec;

impl AddressCodec for TronCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        let trimmed = value.trim();
        decode_base58(trimmed)?;
        Ok(Address::assume_canonical(CHAIN, trimmed))
    }
}

/// The body a `T...` address carries, checked for the mainnet prefix.
fn decode_base58(value: &str) -> Result<[u8; BODY_BYTES], AddressError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    let raw = bs58::decode(trimmed)
        .with_check(None)
        .into_vec()
        .map_err(|_| AddressError::Checksum { chain: CHAIN })?;
    let body: [u8; BODY_BYTES] =
        raw.as_slice().try_into().map_err(|_| AddressError::WrongLength { chain: CHAIN })?;
    match body.first() {
        Some(&PREFIX) => Ok(body),
        _ => Err(AddressError::WrongNetwork { chain: CHAIN }),
    }
}

/// Normalizes a `T...` address or a 21-byte hex body, with or without `0x`, to the
/// `T...` form.
///
/// A bare 20-byte hex value is refused, because that's an Ethereum address and
/// recipient fields come through here.
pub(crate) fn canonical(field: &'static str, value: &str) -> Result<String, TronError> {
    Ok(display(&body(field, value)?))
}

/// Parses either spelling to the 21-byte body that transactions and requests
/// carry.
pub(crate) fn body(field: &'static str, value: &str) -> Result<[u8; BODY_BYTES], TronError> {
    let trimmed = value.trim();
    let hex_chars = BODY_BYTES.saturating_mul(2);
    let stripped = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if stripped.len() == hex_chars
        && let Ok(raw) = hex::decode(stripped)
    {
        return from_body(field, &raw);
    }
    decode_base58(trimmed).map_err(|source| TronError::invalid_address(field, source.to_string()))
}

/// A 21-byte body, checked for the mainnet prefix.
pub(crate) fn from_body(field: &'static str, raw: &[u8]) -> Result<[u8; BODY_BYTES], TronError> {
    let body: [u8; BODY_BYTES] = raw
        .try_into()
        .map_err(|_| TronError::invalid_address(field, format!("{} bytes, not 21", raw.len())))?;
    match body.first() {
        Some(&PREFIX) => Ok(body),
        _ => Err(TronError::invalid_address(field, "a prefix byte that is not tron mainnet")),
    }
}

pub(crate) fn display(body: &[u8; BODY_BYTES]) -> String {
    bs58::encode(body).with_check().into_string()
}

pub(crate) const fn tail(body: &[u8; BODY_BYTES]) -> [u8; TAIL_BYTES] {
    let [_, tail @ ..] = *body;
    tail
}

/// Rebuilds the body from the 20-byte tail in an ABI word.
pub(crate) const fn from_tail(tail: &[u8; TAIL_BYTES]) -> [u8; BODY_BYTES] {
    let mut body = [PREFIX; BODY_BYTES];
    let (_, rest) = body.split_at_mut(1);
    rest.copy_from_slice(tail);
    body
}

/// The address of a secp256k1 public key in either SEC1 form.
pub(crate) fn from_pubkey(pubkey: &[u8]) -> Result<String, KeyError> {
    Ok(display(&from_tail(&keccak_address_bytes(pubkey)?)))
}

#[cfg(test)]
mod tests {
    use jelly_keypair::Secp256k1Key;
    use serde::Deserialize;

    use super::*;
    use crate::test_util::{OWNER, OWNER_KEY, RECIPIENT};

    /// The 21-byte body [`RECIPIENT`] decodes to.
    const RECIPIENT_HEX: &str = "41e28b3cfd4e0e909077821478e9fcb86b84be786e";

    const VECTORS_JSON: &str = include_str!("../tests/vectors/addresses.json");

    #[derive(Deserialize)]
    struct Vectors {
        body_hex: String,
        body_address: String,
    }

    #[test]
    fn every_spelling_of_one_address_reaches_the_same_body() {
        let expected = hex::decode(RECIPIENT_HEX).expect("the recorded body is hex");
        for spelling in
            [RECIPIENT, RECIPIENT_HEX, &format!("0x{RECIPIENT_HEX}"), &format!("  {RECIPIENT}  ")]
        {
            let decoded = body("recipient", spelling).expect("it decodes");
            assert_eq!(decoded.as_slice(), expected.as_slice(), "{spelling}");
            assert_eq!(canonical("recipient", spelling).expect("it re-encodes"), RECIPIENT);
        }
    }

    #[test]
    fn the_hex_body_and_the_base58_form_are_one_encoding() {
        let suite: Vectors = serde_json::from_str(VECTORS_JSON).expect("the vector file parses");
        let body: [u8; BODY_BYTES] =
            hex::decode(&suite.body_hex).expect("hex").as_slice().try_into().expect("21 bytes");
        assert_eq!(display(&body), suite.body_address);
        assert_eq!(hex::encode(decode_base58(&suite.body_address).expect("body")), suite.body_hex);
    }

    #[test]
    fn a_bare_twenty_byte_address_is_never_read_as_a_tron_address() {
        // Twenty bytes of hex is an Ethereum address, and a pasted one is almost
        // certainly meant for another chain.
        let error = body("recipient", "0x9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f")
            .expect_err("the bare tail is refused");
        assert!(matches!(error, TronError::InvalidAddress { field: "recipient", .. }), "{error:?}");
    }

    #[test]
    fn a_broken_checksum_and_a_foreign_prefix_are_both_refused() {
        let mut broken = String::from(RECIPIENT);
        broken.pop();
        broken.push(if RECIPIENT.ends_with('b') { 'c' } else { 'b' });
        assert!(matches!(body("recipient", &broken), Err(TronError::InvalidAddress { .. })));

        let mut foreign = hex::decode(RECIPIENT_HEX).expect("hex");
        if let Some(first) = foreign.first_mut() {
            *first = 0x42;
        }
        assert!(matches!(from_body("recipient", &foreign), Err(TronError::InvalidAddress { .. })));
        assert!(matches!(
            body("recipient", &hex::encode(&foreign)),
            Err(TronError::InvalidAddress { .. })
        ));
        let foreign_body: [u8; BODY_BYTES] = foreign.as_slice().try_into().expect("21 bytes");
        assert_eq!(
            decode_base58(&display(&foreign_body)),
            Err(AddressError::WrongNetwork { chain: CHAIN })
        );
    }

    #[test]
    fn a_checked_string_of_another_length_is_refused_by_length() {
        let short = bs58::encode([PREFIX; 20]).with_check().into_string();
        assert_eq!(decode_base58(&short), Err(AddressError::WrongLength { chain: CHAIN }));
        assert_eq!(decode_base58("1"), Err(AddressError::Checksum { chain: CHAIN }));
        assert_eq!(decode_base58(" "), Err(AddressError::Empty { chain: CHAIN }));
    }

    #[test]
    fn the_tail_is_the_body_without_its_prefix_and_round_trips() {
        let decoded = body("recipient", RECIPIENT).expect("it decodes");
        assert_eq!(hex::encode(tail(&decoded)), "e28b3cfd4e0e909077821478e9fcb86b84be786e");
        assert_eq!(display(&from_tail(&tail(&decoded))), RECIPIENT);
    }

    #[test]
    fn the_eip155_example_key_holds_the_address_this_suite_signs_with() {
        let key = Secp256k1Key::from_secret_bytes(&OWNER_KEY).expect("the example key");
        let derived = from_pubkey(&key.public_uncompressed()).expect("an address");
        assert_eq!(derived, OWNER);
        assert_eq!(
            from_pubkey(&key.public_compressed()).expect("an address"),
            derived,
            "either SEC1 form is the same key and so the same address"
        );
        // The same key's Ethereum address behind TRON's prefix byte.
        assert_eq!(
            canonical("address", "0x419d8a62f656a8d1615c1294fd71e9cfb3e4855a4f").expect("a body"),
            OWNER
        );
    }
}
