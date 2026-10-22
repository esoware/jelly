use jelly_core::{ChainId, CoreError, PhraseWords, ScriptKind, WalletColor};
use jelly_engine::PassphraseInput;

use crate::common::{OTHER_PASSWORD, PASSWORD, harness, prompt, secret, stored};

#[tokio::test]
async fn every_secret_export_re_authenticates() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("exports").await.unwrap();
    let added = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    let id = added.id;

    let refused = harness.engine.export_seed_phrase(secret(OTHER_PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    let refused = harness.engine.export_seed_passphrase(secret(OTHER_PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    let refused = harness.engine.export_seed_entropy_hex(secret(OTHER_PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    let refused = harness.engine.export_account_seed(ChainId::Monero, secret(OTHER_PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    let refused = harness.engine.export_private_key(id, secret(OTHER_PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");

    let phrase = harness.engine.export_seed_phrase(secret(PASSWORD)).await.unwrap();
    assert_eq!(phrase.as_str(), created.phrase.as_str());

    let entropy = harness.engine.export_seed_entropy_hex(secret(PASSWORD)).await.unwrap();
    assert_eq!(entropy.len(), 32);
    assert!(is_hex(&entropy));

    let monero =
        harness.engine.export_account_seed(ChainId::Monero, secret(PASSWORD)).await.unwrap();
    assert_eq!(monero.split_whitespace().count(), 25);

    let key = harness.engine.export_private_key(id, secret(PASSWORD)).await.unwrap();
    assert!(key.starts_with('K') || key.starts_with('L'), "{}", key.as_str());
}

#[tokio::test]
async fn an_export_needs_an_open_wallet() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("locked").await.unwrap();
    harness.engine.lock_wallet().await;

    let refused = harness.engine.export_seed_phrase(secret(PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::Locked)), "{refused:?}");
    let refused = harness.engine.export_seed_passphrase(secret(PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::Locked)), "{refused:?}");
}

#[tokio::test]
async fn a_stored_passphrase_exports_unchanged_after_reopening_and_password_rotation() {
    const PASSPHRASE: &str = "  eleven\t marmots\n\u{e9}\u{301}  ";

    let harness = harness().await.unwrap();
    let created = harness
        .engine
        .create_seed_wallet(
            "stored",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            stored(PASSPHRASE),
        )
        .await
        .unwrap();
    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();

    let passphrase = harness.engine.export_seed_passphrase(secret(PASSWORD)).await.unwrap();
    assert_eq!(passphrase.as_str(), PASSPHRASE);

    harness.engine.change_password(secret(PASSWORD), secret(OTHER_PASSWORD)).await.unwrap();
    let refused = harness.engine.export_seed_passphrase(secret(PASSWORD)).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    let passphrase = harness.engine.export_seed_passphrase(secret(OTHER_PASSWORD)).await.unwrap();
    assert_eq!(passphrase.as_str(), PASSPHRASE);
}

#[tokio::test]
async fn a_passphrase_that_is_not_stored_cannot_be_exported() {
    let harness = harness().await.unwrap();
    for (name, passphrase) in
        [("no passphrase", PassphraseInput::None), ("prompted", prompt("eleven marmots"))]
    {
        harness
            .engine
            .create_seed_wallet(
                name,
                WalletColor::default(),
                secret(PASSWORD),
                PhraseWords::Twelve,
                passphrase,
            )
            .await
            .unwrap();

        let refused = harness.engine.export_seed_passphrase(secret(PASSWORD)).await;
        assert!(matches!(refused, Err(CoreError::NotFound { .. })), "{refused:?}");
        harness.engine.lock_wallet().await;
    }
}

#[tokio::test]
async fn an_imported_key_exports_in_the_chain_encoding() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("keys").await.unwrap();
    let evm = harness.engine.create_imported_address(ChainId::Ethereum, None, None).await.unwrap();
    let solana = harness.engine.create_imported_address(ChainId::Solana, None, None).await.unwrap();
    let litecoin = harness
        .engine
        .create_imported_address(ChainId::Litecoin, Some(ScriptKind::P2wpkh), None)
        .await
        .unwrap();

    let evm_key = harness.engine.export_private_key(evm.id, secret(PASSWORD)).await.unwrap();
    assert_eq!(evm_key.len(), 64);
    assert!(is_hex(&evm_key));

    let solana_key = harness.engine.export_private_key(solana.id, secret(PASSWORD)).await.unwrap();
    assert!(solana_key.len() > 64, "{}", solana_key.as_str());

    let litecoin_key =
        harness.engine.export_private_key(litecoin.id, secret(PASSWORD)).await.unwrap();
    assert!(litecoin_key.starts_with('T'), "{}", litecoin_key.as_str());
}

#[tokio::test]
async fn a_monero_subaddress_exports_its_spend_key() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("monero").await.unwrap();
    let primary = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();
    let second = harness.engine.add_address(ChainId::Monero, None, None).await.unwrap();

    let first_key = harness.engine.export_private_key(primary.id, secret(PASSWORD)).await.unwrap();
    let second_key = harness.engine.export_private_key(second.id, secret(PASSWORD)).await.unwrap();
    assert_eq!(first_key.len(), 64);
    assert!(is_hex(&first_key));
    assert_ne!(first_key.as_str(), second_key.as_str());
}

fn is_hex(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
