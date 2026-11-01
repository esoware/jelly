use jelly_core::KeyKind;
use jelly_keypair::generate_key;

#[test]
fn generated_keys_differ_and_carry_their_kind() {
    for kind in KeyKind::ALL {
        let first = generate_key(kind).unwrap();
        let second = generate_key(kind).unwrap();
        assert_eq!(first.kind(), kind);
        assert_eq!(first.len(), 32);
        first.with_bytes(|a| second.with_bytes(|b| assert_ne!(a, b)));
    }
}
