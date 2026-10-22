use alloy_primitives::Address;
use jelly_keypair::keccak256;

use crate::common::secp256k1_key;

/// The EIP-155 worked example, with nonce 9, 20 gwei, 21000 gas, one ether to the all-`35`
/// address, and chain id 1.
///
/// This is the RLP signing data the specification prints. Hashing it here rather than
/// pinning the digest puts [`keccak256`] under the same vector.
const SIGNING_DATA: &str =
    "ec098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a764000080018080";
const SIGNING_HASH: &str = "daf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53";
const SECRET: &str = "4646464646464646464646464646464646464646464646464646464646464646";
const SENDER: &str = "0x9d8A62f656a8d1615C1294fd71e9CFb3E4855A4F";
const SIGNATURE_R: &str = "28ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276";
const SIGNATURE_S: &str = "67cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83";
/// EIP-155 prints `v = 37` for this transaction, and `v = chain_id * 2 + 35 +
/// recovery_id` with chain id one leaves the recovery id at zero.
const RECOVERY_ID: u8 = 0;

/// Half the secp256k1 group order, big endian.
///
/// Of the two signatures that verify, the one whose `s` is at or below this is
/// canonical. Bitcoin standardness under BIP-146, Ethereum consensus under EIP-2, and
/// TRON all require it.
const HALF_ORDER: &str = "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0";

/// The `s` half of a compact signature, as hex.
fn s_half_hex(signature: &[u8]) -> Option<String> {
    signature.get(32..64).map(hex::encode)
}

#[test]
fn the_eip155_example_signature_is_reproduced() {
    let hash = keccak256(&hex::decode(SIGNING_DATA).expect("the signing data is hex"));
    assert_eq!(hex::encode(hash), SIGNING_HASH, "the signing hash the specification prints");

    let key = secp256k1_key(SECRET).unwrap();
    let sender = Address::from_raw_public_key(
        key.public_uncompressed().get(1..).expect("the key after its 0x04 prefix"),
    );
    assert_eq!(
        sender.to_checksum(None),
        SENDER,
        "the key belongs to the sender of that transaction"
    );

    let recoverable = key.sign_prehash_recoverable(&hash).expect("a recoverable signature");
    assert_eq!(hex::encode(recoverable.get(..32).expect("r")), SIGNATURE_R);
    assert_eq!(s_half_hex(&recoverable).unwrap(), SIGNATURE_S);
    assert_eq!(recoverable.last().copied(), Some(RECOVERY_ID));
}

/// RFC 6979 derives the nonce from the key and the digest alone, so a signature is a
/// pure function of its inputs.
#[test]
fn the_nonce_is_deterministic() {
    let key = secp256k1_key(SECRET).unwrap();
    let hash = keccak256(b"jelly");
    assert_eq!(
        key.sign_prehash_recoverable(&hash).expect("a signature"),
        key.sign_prehash_recoverable(&hash).expect("a signature")
    );
    assert_ne!(
        key.sign_prehash_recoverable(&hash).expect("a signature"),
        key.sign_prehash_recoverable(&keccak256(b"jellz")).expect("a signature")
    );
    assert_ne!(
        key.sign_prehash_recoverable(&hash).expect("a signature"),
        secp256k1_key(&"11".repeat(32))
            .unwrap()
            .sign_prehash_recoverable(&hash)
            .expect("a signature")
    );
}

#[test]
fn every_signature_is_low_s() {
    for key_byte in 0u8..16 {
        let key = secp256k1_key(&hex::encode([key_byte.saturating_add(1); 32])).unwrap();
        for message in 0u8..16 {
            let signature = key
                .sign_prehash_recoverable(&keccak256(&[key_byte, message]))
                .expect("a signature");
            assert!(
                s_half_hex(&signature).unwrap().as_str() <= HALF_ORDER,
                "({key_byte}, {message}) is high-s"
            );
        }
    }
}

#[test]
fn the_recovery_id_is_the_raw_one() {
    let key = secp256k1_key(SECRET).unwrap();
    let mut seen = [false; 4];
    for message in 0u8..64 {
        let signature = key.sign_prehash_recoverable(&keccak256(&[message])).expect("a signature");
        let recovery_id = signature.last().copied().expect("a recovery id");
        assert!(recovery_id < 4, "{recovery_id} is not a recovery id");
        if let Some(slot) = seen.get_mut(usize::from(recovery_id)) {
            *slot = true;
        }
    }
    // Ids two and three need `r >= n`, which happens with probability about
    // 2^-128, so only the first two are ever observed.
    assert_eq!(seen.get(0..2), Some([true, true].as_slice()), "both parities occur");
}
