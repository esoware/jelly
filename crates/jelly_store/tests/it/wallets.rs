use std::time::Duration;

use jelly_core::{ChainId, PassphrasePolicy, WalletColor};
use jelly_store::{ChainStatePatch, ChainStateRow, NewWallet, Patch, Store, StoreError};
use rusqlite::Connection;

use crate::common::{self, Fallible};

/// The one chain state the wallet holds.
async fn only_state(store: &Store, wallet_id: i64) -> Fallible<ChainStateRow> {
    let states = store.list_chain_states(wallet_id).await?;
    let [state] = <[ChainStateRow; 1]>::try_from(states)
        .map_err(|states| format!("expected one chain state, got {states:?}"))?;
    Ok(state)
}

#[tokio::test]
async fn a_new_wallet_comes_back_with_its_settings() {
    let harness = common::harness().await.unwrap();
    let created = harness
        .store
        .create_wallet(NewWallet {
            color: WalletColor::parse("#a855f7").unwrap(),
            passphrase_policy: PassphrasePolicy::Prompt,
            account_index: 3,
            ..common::new_wallet("main")
        })
        .await
        .unwrap();

    assert_eq!(created.name, "main");
    assert_eq!(created.color.as_str(), "#a855f7");
    assert_eq!(harness.store.get_wallet(created.id).await.unwrap(), created);

    let settings = harness.store.get_wallet_settings(created.id).await.unwrap();
    assert_eq!(settings.passphrase_policy, PassphrasePolicy::Prompt);
    assert_eq!(settings.account_index, 3);
}

#[tokio::test]
async fn wallets_list_in_creation_order() {
    let harness = common::harness().await.unwrap();
    common::wallet(&harness.store, "first").await.unwrap();
    common::wallet(&harness.store, "second").await.unwrap();
    let names: Vec<String> =
        harness.store.list_wallets().await.unwrap().into_iter().map(|row| row.name).collect();
    assert_eq!(names, vec![String::from("first"), String::from("second")]);
}

#[tokio::test]
async fn editing_moves_the_updated_stamp_and_leaves_creation_alone() {
    let harness = common::harness().await.unwrap();
    let created = common::wallet(&harness.store, "old").await.unwrap();
    harness.clock.advance(Duration::from_secs(60));

    let color = WalletColor::parse("#a855f7").unwrap();
    let edited =
        harness.store.update_wallet(created.id, String::from("new"), color.clone()).await.unwrap();
    assert_eq!(edited.name, "new");
    assert_eq!(edited.color, color);
    assert_eq!(edited.created_at_unix_secs, created.created_at_unix_secs);
    assert_eq!(harness.store.get_wallet(created.id).await.unwrap(), edited);

    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();
    let updated_at: i64 = connection
        .query_row("SELECT updated_at FROM wallets WHERE id = ?1", [created.id], |row| row.get(0))
        .unwrap();
    assert_eq!(updated_at, created.created_at_unix_secs + 60);
}

#[tokio::test]
async fn a_missing_wallet_is_not_found_rather_than_a_database_failure() {
    let harness = common::harness().await.unwrap();
    let error = harness.store.get_wallet(404).await.expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");

    let error = harness.store.delete_wallet(404).await.expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");

    let error = harness
        .store
        .update_wallet(404, String::from("x"), WalletColor::default())
        .await
        .expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");
}

#[tokio::test]
async fn deleting_a_wallet_takes_its_addresses_with_it() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "doomed").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qaddress").await.unwrap();

    harness.store.delete_wallet(wallet.id).await.unwrap();

    let error = harness.store.get_address(address.id).await.expect_err("cascaded");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");
}

#[tokio::test]
async fn wallet_settings_go_with_their_wallet() {
    let harness = common::harness().await.unwrap();
    let created = common::wallet(&harness.store, "fixed").await.unwrap();

    let settings = harness.store.get_wallet_settings(created.id).await.unwrap();
    assert_eq!(settings.wallet_id, created.id);

    harness.store.delete_wallet(created.id).await.unwrap();
    let error = harness.store.get_wallet_settings(created.id).await.expect_err("cascaded");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet settings", .. }), "{error:?}");
}

#[tokio::test]
async fn chain_state_tells_keeping_a_column_from_clearing_it() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "chains").await.unwrap();

    harness
        .store
        .set_chain_state(
            wallet.id,
            ChainId::Monero,
            ChainStatePatch {
                enabled: Some(true),
                poll_secs: Patch::Set(30),
                restore_height: Patch::Set(3_000_000),
                synced_height: Patch::Set(2_999_999),
            },
        )
        .await
        .unwrap();
    let row = only_state(&harness.store, wallet.id).await.unwrap();
    assert_eq!(row.chain, ChainId::Monero);
    assert!(row.enabled);
    assert_eq!(row.poll_secs, Some(30));
    assert_eq!(row.restore_height, Some(3_000_000));
    assert_eq!(row.synced_height, Some(2_999_999));

    harness
        .store
        .set_chain_state(
            wallet.id,
            ChainId::Monero,
            ChainStatePatch { enabled: Some(false), ..ChainStatePatch::default() },
        )
        .await
        .unwrap();
    let kept = only_state(&harness.store, wallet.id).await.unwrap();
    assert!(!kept.enabled);
    assert_eq!(kept.poll_secs, Some(30));
    assert_eq!(kept.restore_height, Some(3_000_000));

    harness
        .store
        .set_chain_state(
            wallet.id,
            ChainId::Monero,
            ChainStatePatch { poll_secs: Patch::Clear, ..ChainStatePatch::default() },
        )
        .await
        .unwrap();
    let cleared = only_state(&harness.store, wallet.id).await.unwrap();
    assert_eq!(cleared.poll_secs, None);
    assert_eq!(cleared.restore_height, Some(3_000_000));
    assert!(!cleared.enabled);
}

#[tokio::test]
async fn chain_state_under_a_wallet_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = harness
        .store
        .set_chain_state(404, ChainId::Bitcoin, ChainStatePatch::default())
        .await
        .expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");
}

#[tokio::test]
async fn chain_states_list_for_one_wallet_only() {
    let harness = common::harness().await.unwrap();
    let mine = common::wallet(&harness.store, "mine").await.unwrap();
    let other = common::wallet(&harness.store, "other").await.unwrap();
    for chain in [ChainId::Bitcoin, ChainId::Solana] {
        harness.store.set_chain_state(mine.id, chain, ChainStatePatch::default()).await.unwrap();
    }
    harness
        .store
        .set_chain_state(other.id, ChainId::Tron, ChainStatePatch::default())
        .await
        .unwrap();

    let chains: Vec<ChainId> = harness
        .store
        .list_chain_states(mine.id)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.chain)
        .collect();
    assert_eq!(chains, vec![ChainId::Bitcoin, ChainId::Solana]);
}
