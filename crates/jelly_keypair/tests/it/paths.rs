use bitcoin::bip32::{self, ChildNumber};
use jelly_core::{DerivationPath, KeyError, KeyKind};
use jelly_keypair::{
    PathTemplate, account_prefix, parse_path, stored_path, test_util::test_keychain,
};
use proptest::prelude::*;

/// The path Phantom and Solflare use.
const SOLANA_TEMPLATE: &str = "m/44'/501'/{index}'/0'";

#[expect(clippy::expect_used, reason = "test helper")]
fn solana() -> PathTemplate {
    PathTemplate::parse(SOLANA_TEMPLATE).expect("the template parses")
}

fn stored(items: &[&str]) -> Vec<DerivationPath> {
    items.iter().map(|text| DerivationPath::new(*text)).collect()
}

fn rendered(path: &bip32::DerivationPath) -> String {
    stored_path(path).to_string()
}

#[test]
fn paths_round_trip_through_the_stored_string() {
    let original = parse_path("m/44'/60'/0'/0/7").unwrap();
    let stored = stored_path(&original);
    assert_eq!(stored.as_str(), "m/44'/60'/0'/0/7");
    assert_eq!(parse_path(stored.as_str()).expect("parse back"), original);
}

#[test]
fn the_account_prefix_ends_at_the_last_hardened_component() {
    assert_eq!(
        account_prefix(&parse_path("m/84'/0'/0'/0/5").unwrap()),
        Some(parse_path("m/84'/0'/0'").unwrap())
    );
    assert_eq!(
        account_prefix(&parse_path("m/44'/501'/3'/0'").unwrap()),
        Some(parse_path("m/44'/501'/3'/0'").unwrap())
    );
    assert_eq!(
        account_prefix(&parse_path("m/0/1").unwrap()),
        None,
        "an unhardened path has no account"
    );
}

#[test]
fn a_family_recognizes_only_its_own_instances() {
    let template = solana();
    assert_eq!(template.index_of(&parse_path("m/44'/501'/7'/0'").unwrap()), Some(7));
    assert_eq!(template.index_of(&parse_path("m/44'/501'/7'/0").unwrap()), None);
    assert_eq!(template.index_of(&parse_path("m/44'/501'/7'").unwrap()), None);
    assert_eq!(template.index_of(&parse_path("m/44'/501'/7'/1'").unwrap()), None);
    assert_eq!(template.index_of(&parse_path("m").unwrap()), None);
}

#[test]
fn every_hardened_marker_parses() {
    let expected = bip32::DerivationPath::from(vec![
        ChildNumber::Hardened { index: 86 },
        ChildNumber::Hardened { index: 0 },
        ChildNumber::Hardened { index: 0 },
        ChildNumber::Normal { index: 0 },
        ChildNumber::Normal { index: 3 },
    ]);
    for text in [
        "m/86'/0'/0'/0/3",
        "m/86h/0h/0h/0/3",
        "M/86H/0H/0H/0/3",
        "86'/0'/0'/0/3",
        "m/86\u{2019}/0\u{2019}/0\u{2019}/0/3",
    ] {
        let parsed = parse_path(text).unwrap();
        assert_eq!(parsed, expected, "{text}");
        assert_eq!(rendered(&parsed), "m/86'/0'/0'/0/3");
    }
}

#[test]
fn the_master_path_has_no_components() {
    for text in ["m", "M", " m "] {
        let parsed = parse_path(text).unwrap();
        assert!(parsed.is_master());
        assert_eq!(rendered(&parsed), "m");
    }
}

#[test]
fn malformed_paths_are_rejected_at_their_component() {
    for (text, component) in [
        ("", 0),
        (" ", 0),
        ("m/", 0),
        ("m//0", 0),
        ("m/x", 0),
        ("x", 0),
        ("m/-1", 0),
        ("m/0''", 0),
        ("m/'0", 0),
        ("m/2147483648", 0),
        ("m/2147483648'", 0),
        ("m/1.5", 0),
        ("n/0", 0),
        ("m/0/+1", 1),
        ("0/+1", 1),
        ("m/0 /1", 0),
        ("44'/x", 1),
        ("m/44'/x", 1),
    ] {
        match parse_path(text) {
            Err(KeyError::InvalidPath { component: got }) => {
                assert_eq!(got, component, "{text}");
            }
            other => panic!("{text} must not parse: {other:?}"),
        }
    }
}

#[test]
fn a_path_deeper_than_bip32_allows_is_refused() {
    let deep = format!("m{}", "/0".repeat(256));
    assert!(matches!(parse_path(&deep), Err(KeyError::PathTooDeep { got: 256, max: 255 })));
    assert_eq!(parse_path(&format!("m{}", "/0".repeat(255))).unwrap().len(), 255);
}

#[test]
fn a_path_error_names_its_component_from_one() {
    let error = parse_path("m/44'/x").expect_err("a malformed component");
    assert_eq!(error.to_string(), "Derivation path component 2 is invalid.");
    let error = test_keychain()
        .key_material_at(KeyKind::Ed25519Seed, &parse_path("m/44'/501'/0'/0").unwrap())
        .expect_err("a soft component");
    assert_eq!(error.to_string(), "Add an apostrophe to harden path component 4.");
}

#[test]
fn a_template_parses_its_placeholder_in_either_hardness() {
    let soft = PathTemplate::parse("m/86'/0'/0'/0/{index}").expect("a template");
    assert_eq!(soft, PathTemplate::bip44(86, 0));
    assert_eq!(rendered(&soft.path_at(7).expect("a path")), "m/86'/0'/0'/0/7");

    let hard = PathTemplate::parse("m/44'/501'/{index}'/0'").expect("a template");
    assert_eq!(rendered(&hard.path_at(3).expect("a path")), "m/44'/501'/3'/0'");
    assert_eq!(hard.index_of(&hard.path_at(3).expect("a path")), Some(3));
    assert_eq!(hard.index_of(&soft.path_at(3).expect("a path")), None);
}

#[test]
fn a_template_without_exactly_one_placeholder_is_internal() {
    assert!(matches!(PathTemplate::parse("m/44'/0'/0'"), Err(KeyError::Internal { .. })));
    assert!(matches!(PathTemplate::parse("m/{index}/{index}"), Err(KeyError::Internal { .. })));
}

#[test]
fn a_placeholder_with_an_unknown_marker_is_an_invalid_component() {
    assert!(matches!(
        PathTemplate::parse("m/{index}x"),
        Err(KeyError::InvalidPath { component: 0 })
    ));
}

#[test]
fn an_index_past_the_bip32_limit_is_refused() {
    for template in [PathTemplate::bip44(86, 0), solana()] {
        template.path_at(0x7fff_ffff).expect("the largest index");
        assert!(matches!(
            template.path_at(0x8000_0000),
            Err(KeyError::ChildIndexOutOfRange { index: 0x8000_0000 })
        ));
    }
}

#[test]
fn a_bip44_family_renders_its_purpose_and_coin() {
    for purpose in [44u32, 49, 84, 86] {
        let family = PathTemplate::bip44(purpose, 0).path_at(0).expect("a path");
        assert_eq!(rendered(&family), format!("m/{purpose}'/0'/0'/0/0"));
        assert_eq!(family.as_ref().first(), Some(&ChildNumber::Hardened { index: purpose }));
    }
    assert_eq!(
        rendered(&PathTemplate::bip44(84, 2).path_at(9).expect("a path")),
        "m/84'/2'/0'/0/9"
    );
    let solana = solana().path_at(3).expect("a path");
    assert_eq!(rendered(&solana), "m/44'/501'/3'/0'");
    assert!(solana.into_iter().all(ChildNumber::is_hardened));
}

#[test]
fn the_next_index_fills_the_first_gap() {
    let next_default = |template: &PathTemplate, used_paths: &[DerivationPath]| {
        rendered(&template.path_at(template.next_index(used_paths)).expect("next"))
    };
    let bitcoin = PathTemplate::bip44(86, 0);
    let used = stored(&["m/86'/0'/0'/0/0", "m/86'/0'/0'/0/1", "m/86'/0'/0'/0/3"]);
    assert_eq!(next_default(&bitcoin, &used), "m/86'/0'/0'/0/2");
    assert_eq!(next_default(&bitcoin, &[]), "m/86'/0'/0'/0/0");

    // Duplicates, other families and unparsable strings are all ignored.
    let noise = stored(&[
        "m/86'/0'/0'/0/0",
        "m/86'/0'/0'/0/0",
        "m/86'/0'/1'/0/5",
        "m/84'/0'/0'/0/9",
        "m/44'/60'/0'/0/7",
        "not a path",
    ]);
    assert_eq!(next_default(&bitcoin, &noise), "m/86'/0'/0'/0/1");
    assert_eq!(next_default(&PathTemplate::bip44(44, 60), &noise), "m/44'/60'/0'/0/0");

    let used = stored(&["m/44'/501'/0'/0'", "m/44'/501'/1'/0'"]);
    assert_eq!(next_default(&solana(), &used), "m/44'/501'/2'/0'");
}

proptest! {
    #[test]
    fn parsing_a_rendered_path_returns_it(
        components in prop::collection::vec((0u32..0x8000_0000, any::<bool>()), 0..12)
    ) {
        let original: bip32::DerivationPath = components
            .into_iter()
            .map(|(index, is_hardened)| {
                if is_hardened {
                    ChildNumber::Hardened { index }
                } else {
                    ChildNumber::Normal { index }
                }
            })
            .collect();
        prop_assert_eq!(parse_path(&rendered(&original)).expect("reparse"), original);
    }
}
