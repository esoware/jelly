use jelly_core::{ChainId, DerivationPath, KeySource};
use jelly_store::{NewAddress, StoreError};
use rusqlite::ffi;

use crate::common;

#[tokio::test]
async fn every_key_source_survives_the_round_trip() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "sources").await.unwrap();

    let sources = [
        KeySource::Derived { path: DerivationPath::new("m/44'/60'/0'/0/2") },
        KeySource::Imported { vault_entry: String::from("0123456789abcdef0123456789abcdef") },
        KeySource::Subaddress { major: 1, minor: 4 },
    ];
    for (position, source) in sources.into_iter().enumerate() {
        let new = NewAddress {
            source: source.clone(),
            ..common::new_address(wallet.id, ChainId::Bitcoin, &format!("bc1qsource{position}"))
        };
        let inserted = harness.store.insert_address(new).await.unwrap();
        assert_eq!(inserted.source, source);
        assert_eq!(harness.store.get_address(inserted.id).await.unwrap().source, source);
    }
}

#[tokio::test]
async fn a_row_may_carry_an_index_or_none_at_all() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "indexes").await.unwrap();

    let fixed = NewAddress {
        derivation_index: Some(9),
        ..common::new_address(wallet.id, ChainId::Bitcoin, "bc1qfixed")
    };
    assert_eq!(harness.store.insert_address(fixed).await.unwrap().derivation_index, Some(9));

    let loose = NewAddress {
        derivation_index: None,
        ..common::new_address(wallet.id, ChainId::Bitcoin, "bc1qloose")
    };
    assert_eq!(harness.store.insert_address(loose).await.unwrap().derivation_index, None);
}

#[tokio::test]
async fn the_same_address_twice_in_one_wallet_already_exists() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "dupes").await.unwrap();
    harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Bitcoin, "bc1qsame"))
        .await
        .unwrap();

    let error = harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Bitcoin, "bc1qsame"))
        .await
        .expect_err("the second insert");
    assert!(
        matches!(
            error,
            StoreError::AlreadyExists { entity: "address", columns: "wallet_id, chain, address" }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn the_same_address_under_two_wallets_is_two_rows() {
    let harness = common::harness().await.unwrap();
    let first = common::wallet(&harness.store, "first").await.unwrap();
    let second = common::wallet(&harness.store, "second").await.unwrap();

    let mine = harness
        .store
        .insert_address(common::new_address(first.id, ChainId::Bitcoin, "bc1qshared"))
        .await
        .unwrap();
    let theirs = harness
        .store
        .insert_address(common::new_address(second.id, ChainId::Bitcoin, "bc1qshared"))
        .await
        .unwrap();
    assert_ne!(mine.id, theirs.id);

    for (wallet, expected) in [(first.id, mine.id), (second.id, theirs.id)] {
        let rows = harness.store.list_addresses(wallet, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().map(|row| row.id), Some(expected));
    }
}

#[tokio::test]
async fn an_address_under_a_wallet_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = harness
        .store
        .insert_address(common::new_address(404, ChainId::Bitcoin, "bc1qorphan"))
        .await
        .expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");
}

#[tokio::test]
async fn a_key_source_with_the_wrong_payload_cannot_reach_the_file() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "shape").await.unwrap();
    let closed = harness.close().await.unwrap();
    let connection = rusqlite::Connection::open(common::db_path(&closed.directory)).unwrap();

    // The schema check keeps the discriminant and its detail columns consistent, so no
    // read has to cope with a mismatch.
    let refused = connection
        .execute(
            "INSERT INTO addresses (wallet_id, chain, address, key_source, created_at) \
             VALUES (?1, 'bitcoin', 'bc1qbroken', 'derived', 0)",
            rusqlite::params![wallet.id],
        )
        .expect_err("a derived address without a path was accepted");
    assert_eq!(
        refused.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{refused}"
    );

    let refused = connection
        .execute(
            "INSERT INTO addresses \
               (wallet_id, chain, address, key_source, derivation_path, vault_entry, created_at) \
             VALUES (?1, 'bitcoin', 'bc1qboth', 'derived', 'm/0', 'entry', 0)",
            rusqlite::params![wallet.id],
        )
        .expect_err("a derived address with a vault entry was accepted");
    assert_eq!(
        refused.sqlite_extended_error_code(),
        Some(ffi::SQLITE_CONSTRAINT_CHECK),
        "{refused}"
    );
}

#[tokio::test]
async fn listing_filters_by_chain() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "listing").await.unwrap();
    let bitcoin = harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Bitcoin, "bc1qlisted"))
        .await
        .unwrap();
    harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Solana, "SoLaNaAddress"))
        .await
        .unwrap();

    let rows = harness.store.list_addresses(wallet.id, Some(ChainId::Bitcoin)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.first().map(|row| row.id), Some(bitcoin.id));

    assert_eq!(harness.store.list_addresses(wallet.id, None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_label_can_be_set_and_cleared() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "labels").await.unwrap();
    let row = harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Bitcoin, "bc1qlabel"))
        .await
        .unwrap();

    let labelled =
        harness.store.set_address_label(row.id, Some(String::from("savings"))).await.unwrap();
    assert_eq!(labelled.label.as_deref(), Some("savings"));

    let cleared = harness.store.set_address_label(row.id, None).await.unwrap();
    assert_eq!(cleared.label, None);
}

#[tokio::test]
async fn deleting_an_address_reports_a_second_attempt_as_missing() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "delete").await.unwrap();
    let row = harness
        .store
        .insert_address(common::new_address(wallet.id, ChainId::Bitcoin, "bc1qgone"))
        .await
        .unwrap();

    harness.store.delete_address(row.id).await.unwrap();
    let error = harness.store.delete_address(row.id).await.expect_err("the second attempt");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");

    let error = harness.store.set_address_label(row.id, None).await.expect_err("no such address");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");
}
