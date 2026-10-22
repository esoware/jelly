use jelly_core::{ChainId, CoreError, PassphrasePolicy, PhraseWords, SecretText, WalletColor};
use jelly_engine::{PassphraseInput, WalletRecord};

use crate::common::{Fallible, Harness, PASSWORD, harness, prompt, secret, stored};

const PASSPHRASE: &str = "eleven marmots";

#[test]
fn a_passphrase_input_never_prints_its_value() {
    for input in [stored(PASSPHRASE), prompt(PASSPHRASE)] {
        let debug = format!("{input:?}");
        assert!(!debug.contains(PASSPHRASE), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }
}

#[test]
fn a_passphrase_input_implies_its_policy() {
    assert_eq!(PassphraseInput::None.policy(), PassphrasePolicy::None);
    assert!(PassphraseInput::None.value().is_none());
    let kept = stored(PASSPHRASE);
    assert_eq!(kept.policy(), PassphrasePolicy::Stored);
    assert!(kept.value().is_some_and(|value| value.with_str(|text| text == PASSPHRASE)));
    assert_eq!(prompt(PASSPHRASE).policy(), PassphrasePolicy::Prompt);
}

#[test]
fn a_passphrase_input_deserializes_from_its_tagged_form() {
    let input: PassphraseInput =
        serde_json::from_str("{\"kind\":\"prompt\",\"value\":\"pw\"}").unwrap();
    assert_eq!(input.policy(), PassphrasePolicy::Prompt);
    assert!(input.value().is_some_and(|value| value.with_str(|text| text == "pw")));
}

async fn seed_with(harness: &Harness, passphrase: PassphraseInput) -> Fallible<WalletRecord> {
    let created = harness
        .engine
        .create_seed_wallet(
            "passphrased",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            passphrase,
        )
        .await?;
    Ok(created.wallet)
}

#[tokio::test]
async fn a_prompted_passphrase_is_checked_against_the_verifier() {
    let harness = harness().await.unwrap();
    let wallet = seed_with(&harness, prompt(PASSPHRASE)).await.unwrap();
    assert_eq!(wallet.passphrase_policy, PassphrasePolicy::Prompt);
    harness.engine.lock_wallet().await;

    let missing = harness.engine.unlock_wallet(wallet.id, secret(PASSWORD), None).await;
    assert!(matches!(missing, Err(CoreError::PassphraseRequired)), "{missing:?}");

    let empty = harness
        .engine
        .unlock_wallet(wallet.id, secret(PASSWORD), Some(SecretText::new(String::new())))
        .await;
    assert!(matches!(empty, Err(CoreError::WrongPassphrase)), "{empty:?}");

    let wrong = harness
        .engine
        .unlock_wallet(
            wallet.id,
            secret(PASSWORD),
            Some(SecretText::new(String::from("twelve marmots"))),
        )
        .await;
    assert!(matches!(wrong, Err(CoreError::WrongPassphrase)), "{wrong:?}");
    assert!(!harness.engine.lock_state_now().is_unlocked());

    harness
        .engine
        .unlock_wallet(wallet.id, secret(PASSWORD), Some(SecretText::new(String::from(PASSPHRASE))))
        .await
        .unwrap();
    assert!(harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn a_prompted_passphrase_is_checked_against_a_stored_address() {
    let harness = harness().await.unwrap();
    let wallet = seed_with(&harness, prompt(PASSPHRASE)).await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;

    let wrong = harness
        .engine
        .unlock_wallet(
            wallet.id,
            secret(PASSWORD),
            Some(SecretText::new(String::from("twelve marmots"))),
        )
        .await;
    assert!(matches!(wrong, Err(CoreError::WrongPassphrase)), "{wrong:?}");

    harness
        .engine
        .unlock_wallet(wallet.id, secret(PASSWORD), Some(SecretText::new(String::from(PASSPHRASE))))
        .await
        .unwrap();
    let listed = harness.engine.list_addresses(Some(ChainId::Bitcoin)).await.unwrap();
    assert_eq!(listed.first().map(|row| row.address.clone()), Some(added.address));
}

#[tokio::test]
async fn a_stored_passphrase_needs_no_prompt() {
    let harness = harness().await.unwrap();
    let wallet = seed_with(&harness, stored(PASSPHRASE)).await.unwrap();
    assert_eq!(wallet.passphrase_policy, PassphrasePolicy::Stored);
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;

    harness.engine.unlock_wallet(wallet.id, secret(PASSWORD), None).await.unwrap();
    let listed = harness.engine.list_addresses(Some(ChainId::Bitcoin)).await.unwrap();
    assert_eq!(listed.first().map(|row| row.address.clone()), Some(added.address));
}

#[tokio::test]
async fn a_passphrase_changes_the_addresses_a_seed_produces() {
    let harness = harness().await.unwrap();
    let plain = harness.seed_wallet("plain").await.unwrap();
    let phrase = plain.phrase.to_string();
    let bare = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;

    harness
        .engine
        .import_seed_wallet(
            "extended",
            WalletColor::default(),
            secret(PASSWORD),
            SecretText::new(phrase),
            prompt(PASSPHRASE),
        )
        .await
        .unwrap();
    let extended = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    assert_ne!(bare.address, extended.address);
}

#[tokio::test]
async fn a_wallet_without_a_passphrase_ignores_one_that_is_offered() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("plain").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;

    harness
        .engine
        .unlock_wallet(
            created.wallet.id,
            secret(PASSWORD),
            Some(SecretText::new(String::from("ignored"))),
        )
        .await
        .unwrap();
    let listed = harness.engine.list_addresses(Some(ChainId::Bitcoin)).await.unwrap();
    assert_eq!(listed.first().map(|row| row.address.clone()), Some(added.address));
}
