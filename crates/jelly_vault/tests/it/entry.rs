use jelly_vault::BlobKind;

/// The payload stores each kind under its spelling, so editing one makes every entry of
/// that kind unreadable.
#[test]
fn blob_kinds_keep_their_persisted_spellings() {
    let spellings = [
        "seed_entropy",
        "seed_passphrase",
        "passphrase_verifier",
        "private_key",
        "monero_scan_state",
    ];
    assert_eq!(BlobKind::COUNT, spellings.len());
    for (kind, spelling) in BlobKind::ALL.into_iter().zip(spellings) {
        assert_eq!(kind.as_str(), spelling);
        assert_eq!(kind.to_string(), spelling);
        assert_eq!(BlobKind::parse(spelling), Some(kind));
    }
    assert_eq!(BlobKind::parse(""), None);
    assert_eq!(BlobKind::parse("unknown"), None);
}
