//! Chain entries don't parse BIP-39, so recognizing a pasted seed phrase is the engine's
//! part of a key import.

use jelly_core::{ChainId, CoreError, SecretText};
use jelly_engine::AddressRecord;
use jelly_keypair::test_util::TEST_PHRASE;
use jelly_mnemonic::Mnemonic;

use crate::common::{Harness, harness};

const SEED_PHRASE: &str =
    "This is a seed phrase, not a private key. Restore it as a wallet instead.";

/// Every word shares its first three letters with a Monero word, which happens for about
/// one 24-word phrase in thirty, and Monero matches a word by those letters.
const TWENTY_FOUR_WORDS: &str = "secret busy topic cage length tragic soon smooth fragile slender secret thank alone dilemma camera relief song firm that country inject energy trap lava";

/// A published Monero seed, from knaccc/subaddress-js.
const MONERO_SEED: &str = "vary ambush western rafts session laboratory jerseys napkin muffin exult tolerant efficient ensign fewest sulking wonders pledge alerts acquire tubes rogue outbreak lifestyle lopped pledge";

async fn import(
    harness: &Harness,
    chain: ChainId,
    input: &str,
) -> Result<AddressRecord, CoreError> {
    harness
        .engine
        .import_private_key(chain, SecretText::new(input.to_owned()), None, None, None)
        .await
}

fn is_named_a_seed_phrase(outcome: &Result<AddressRecord, CoreError>) -> bool {
    matches!(
        outcome,
        Err(CoreError::InvalidInput { field: "key", detail }) if detail == SEED_PHRASE
    )
}

#[tokio::test]
async fn a_seed_phrase_is_named_rather_than_read_as_a_key() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    for chain in [ChainId::Bitcoin, ChainId::Ethereum, ChainId::Solana] {
        let refused = import(&harness, chain, TEST_PHRASE).await;
        assert!(is_named_a_seed_phrase(&refused), "{chain:?}: {refused:?}");
    }
}

#[tokio::test]
async fn a_twenty_four_word_seed_phrase_is_named_rather_than_read_as_a_monero_seed() {
    assert_eq!(Mnemonic::parse(TWENTY_FOUR_WORDS).unwrap().word_count(), 24);
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    for chain in [ChainId::Bitcoin, ChainId::Ethereum] {
        let refused = import(&harness, chain, TWENTY_FOUR_WORDS).await;
        assert!(
            is_named_a_seed_phrase(&refused),
            "expected {chain:?} to name the phrase a seed phrase, as it does a 12-word one, \
             got {refused:?}"
        );
    }
}

#[tokio::test]
async fn a_colon_numbered_seed_phrase_is_named_rather_than_refused_as_an_unknown_format() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let numbered: Vec<String> = TEST_PHRASE
        .split(' ')
        .enumerate()
        .map(|(i, word)| format!("{}: {word}", i.saturating_add(1)))
        .collect();
    let refused = import(&harness, ChainId::Bitcoin, &numbered.join("\n")).await;
    assert!(
        is_named_a_seed_phrase(&refused),
        "expected a seed phrase numbered `1: word` to be named as one, got {refused:?}"
    );
}

/// A TON seed is spelled in BIP-39's words, so TON reads a phrase as its own seed first
/// and names it a seed phrase only once that fails.
#[tokio::test]
async fn a_seed_phrase_that_is_no_ton_seed_is_named_on_ton() {
    let twenty_four = format!("{} art", ["abandon"; 23].join(" "));
    assert_eq!(Mnemonic::parse(&twenty_four).unwrap().word_count(), 24);
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    for phrase in [TEST_PHRASE, twenty_four.as_str()] {
        let refused = import(&harness, ChainId::Ton, phrase).await;
        assert!(
            is_named_a_seed_phrase(&refused),
            "expected a valid BIP-39 phrase that is no TON seed to be named a seed phrase, \
             as on every other chain, got {refused:?}"
        );
    }
}

/// Monero's scanner watches only the wallet's own account, so a standalone key there
/// would never show its funds.
#[tokio::test]
async fn monero_holds_no_standalone_keys() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let refused = import(&harness, ChainId::Monero, MONERO_SEED).await;
    assert!(matches!(refused, Err(CoreError::Unsupported(_))), "{refused:?}");
    let refused = harness.engine.create_imported_address(ChainId::Monero, None, None).await;
    assert!(matches!(refused, Err(CoreError::Unsupported(_))), "{refused:?}");
    assert!(harness.engine.list_addresses(Some(ChainId::Monero)).await.unwrap().is_empty());
}
