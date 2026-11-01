use jelly_core::{KeyError, PhraseLength};
use jelly_keypair::test_util::TEST_PHRASE;
use jelly_mnemonic::Mnemonic;
use proptest::{prelude::*, test_runner::RngSeed};

const PROPERTY_SEED: u64 = 20_261_021;

#[test]
fn phrase_whitespace_and_case_are_normalized() {
    let spaced = TEST_PHRASE.replace(' ', "\u{3000}");
    let mnemonic = Mnemonic::parse(&spaced).expect("ideographic spaces are whitespace");
    assert_eq!(mnemonic.phrase().as_str(), TEST_PHRASE);

    let mixed = Mnemonic::parse(
        "  Abandon\tabandon  abandon abandon abandon abandon abandon abandon abandon abandon abandon\nABOUT ",
    )
    .expect("the phrase parses");
    assert_eq!(mixed.phrase().as_str(), TEST_PHRASE);
    assert_eq!(mixed.word_count(), 12);
}

#[test]
fn list_numbers_and_separators_are_not_words() {
    let numbered: Vec<String> = TEST_PHRASE
        .split(' ')
        .enumerate()
        .map(|(i, word)| format!("{}. {word}", i.saturating_add(1)))
        .collect();
    let glued: Vec<String> = TEST_PHRASE
        .split(' ')
        .enumerate()
        .map(|(i, word)| format!("{}){word}", i.saturating_add(1)))
        .collect();
    for phrase in [numbered.join("\n"), glued.join(" "), TEST_PHRASE.replace(' ', ", ")] {
        assert_eq!(
            Mnemonic::parse(&phrase).expect("the phrase parses").phrase().as_str(),
            TEST_PHRASE,
            "{phrase}"
        );
    }
}

#[test]
fn malformed_phrases_are_rejected_with_their_reason() {
    let bad_checksum = TEST_PHRASE.replace("about", "abandon");
    assert!(matches!(Mnemonic::parse(&bad_checksum), Err(KeyError::PhraseChecksum)));

    let unknown = TEST_PHRASE.replacen("abandon", "notaword", 1);
    assert!(matches!(Mnemonic::parse(&unknown), Err(KeyError::UnknownWord { index: 0 })));
    let unknown_later = TEST_PHRASE.replace("about", "notaword");
    assert!(matches!(Mnemonic::parse(&unknown_later), Err(KeyError::UnknownWord { index: 11 })));

    assert!(matches!(
        Mnemonic::parse("abandon abandon about"),
        Err(KeyError::InvalidWordCount { got: 3 })
    ));
    assert!(matches!(Mnemonic::parse(""), Err(KeyError::InvalidWordCount { got: 0 })));
}

#[test]
fn entropy_of_an_illegal_length_is_rejected_with_its_length() {
    for entropy_bytes in [0usize, 15, 17, 31, 33, 64] {
        assert!(
            matches!(
                Mnemonic::from_entropy(&vec![0u8; entropy_bytes]),
                Err(KeyError::InvalidEntropyLength { got }) if got == entropy_bytes
            ),
            "{entropy_bytes} bytes of entropy is not a phrase"
        );
    }
}

#[test]
fn a_generated_phrase_has_the_requested_length_and_reparses_to_its_entropy() {
    for length in PhraseLength::ALL {
        let (word_count, entropy_bytes) = match length {
            PhraseLength::Twelve => (12, 16),
            PhraseLength::TwentyFour => (24, 32),
        };
        let mnemonic = Mnemonic::generate(length).expect("generate");
        assert_eq!(mnemonic.word_count(), word_count);
        assert_eq!(mnemonic.phrase().split(' ').count(), word_count);
        assert_eq!(mnemonic.with_entropy(<[u8]>::len), entropy_bytes);
        let again = Mnemonic::parse(&mnemonic.phrase()).expect("reparse");
        mnemonic.with_entropy(|entropy| {
            again.with_entropy(|reparsed| assert_eq!(reparsed, entropy));
        });
    }
}

#[test]
fn two_generated_phrases_differ() {
    let first = Mnemonic::generate(PhraseLength::TwentyFour).expect("generate");
    let second = Mnemonic::generate(PhraseLength::TwentyFour).expect("generate");
    assert_ne!(first.phrase().as_str(), second.phrase().as_str());
}

#[test]
fn a_phrase_with_a_word_changed_decodes_to_different_entropy() {
    let words: Vec<&str> = TEST_PHRASE.split(' ').collect();
    let original = Mnemonic::parse(TEST_PHRASE).expect("the phrase parses");
    let mut has_parsed_one = false;
    for position in 0..words.len() {
        for replacement in ["ability", "able", "zoo"] {
            let mut altered = words.clone();
            let slot = altered.get_mut(position).expect("in range");
            if *slot == replacement {
                continue;
            }
            *slot = replacement;
            if let Ok(mnemonic) = Mnemonic::parse(&altered.join(" ")) {
                has_parsed_one = true;
                original.with_entropy(|expected| {
                    mnemonic.with_entropy(|entropy| {
                        assert_ne!(
                            entropy, expected,
                            "word {position} as {replacement} decoded to the same entropy"
                        );
                    });
                });
            }
        }
    }
    assert!(has_parsed_one, "no altered phrase passed the checksum");
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        rng_seed: RngSeed::Fixed(PROPERTY_SEED),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn entropy_round_trips_through_its_phrase(
        entropy in prop::sample::select(&[16usize, 20, 24, 28, 32])
            .prop_flat_map(|entropy_bytes| prop::collection::vec(any::<u8>(), entropy_bytes))
    ) {
        let mnemonic = Mnemonic::from_entropy(&entropy).expect("legal entropy");
        let back = Mnemonic::parse(&mnemonic.phrase()).expect("its own words parse");
        prop_assert!(back.with_entropy(|recovered| recovered == entropy.as_slice()));
    }
}
