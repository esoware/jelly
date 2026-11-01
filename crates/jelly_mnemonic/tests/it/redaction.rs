use jelly_core::test_util::{MARKER, MARKER_PHRASE, assert_rejected_without_leak};
use jelly_keypair::test_util::TEST_PHRASE;
use jelly_mnemonic::Mnemonic;

#[test]
fn a_rejected_phrase_or_entropy_never_appears_in_its_error() {
    assert_rejected_without_leak("Mnemonic::parse", MARKER_PHRASE, Mnemonic::parse(MARKER_PHRASE));
    assert_rejected_without_leak(
        "Mnemonic::from_entropy",
        MARKER,
        Mnemonic::from_entropy(MARKER.as_bytes()),
    );
}

#[test]
fn a_mnemonic_redacts_its_debug() {
    let mnemonic = Mnemonic::parse(TEST_PHRASE).expect("the phrase parses");
    let debug = format!("{mnemonic:?}");
    assert!(debug.contains("redacted"));
    for word in mnemonic.phrase().split(' ') {
        assert!(!debug.contains(word), "Debug leaked a word");
    }
    mnemonic.with_entropy(|entropy| assert!(!debug.contains(&hex::encode(entropy))));
}
