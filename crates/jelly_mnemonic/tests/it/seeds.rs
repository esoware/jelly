use jelly_keypair::test_util::{TEST_PHRASE, test_seed};
use jelly_mnemonic::Mnemonic;

fn seed_hex(mnemonic: &Mnemonic, passphrase: &str) -> String {
    mnemonic.to_seed(passphrase).with_bytes(|bytes| hex::encode(bytes))
}

#[test]
fn the_abandon_phrase_expands_to_the_test_seed_and_a_passphrase_changes_it() {
    let mnemonic = Mnemonic::parse(TEST_PHRASE).expect("the phrase parses");
    assert_eq!(seed_hex(&mnemonic, ""), test_seed().with_bytes(|bytes| hex::encode(bytes)));
    assert_ne!(seed_hex(&mnemonic, ""), seed_hex(&mnemonic, "x"));
}

#[test]
fn the_passphrase_is_nfkd_normalized() {
    let mnemonic = Mnemonic::parse(TEST_PHRASE).expect("the phrase parses");
    let composed = seed_hex(&mnemonic, "caf\u{00e9}");
    assert_eq!(composed, seed_hex(&mnemonic, "cafe\u{0301}"));
    assert_ne!(composed, seed_hex(&mnemonic, "cafe"));
    // Cross-checked against PBKDF2-HMAC-SHA512 over the NFKD forms.
    assert_eq!(
        composed,
        "af8bbd2566df7b69d926f2b09dfdbd75db6c994a3399b2cc65f928d63e3fd4e6\
         1218ee0d15f8c810be4d45e66d47b43c15a5cc753976b1666912377ff7ae9818"
    );
    // A compatibility character decomposes to its ASCII expansion.
    assert_eq!(seed_hex(&mnemonic, "\u{ff21}"), seed_hex(&mnemonic, "A"));
}
