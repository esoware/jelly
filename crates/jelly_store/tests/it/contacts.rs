use jelly_core::ChainId;
use jelly_store::{ContactEdit, NewContact, StoreError};

use crate::common;

fn contact(wallet_id: i64, chain: ChainId, address: &str, name: &str) -> NewContact {
    NewContact { wallet_id, chain, address: address.to_owned(), name: name.to_owned(), note: None }
}

#[tokio::test]
async fn a_contact_survives_the_round_trip() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "book").await.unwrap();

    let mut new = contact(wallet.id, ChainId::Ethereum, "0xabc", "Alice");
    new.note = Some(String::from("Rent"));
    let inserted = harness.store.insert_contact(new).await.unwrap();
    assert_eq!(inserted.chain, ChainId::Ethereum);
    assert_eq!(inserted.address, "0xabc");
    assert_eq!(inserted.name, "Alice");
    assert_eq!(inserted.note.as_deref(), Some("Rent"));

    let fetched = harness.store.get_contact(wallet.id, inserted.id).await.unwrap();
    assert_eq!(fetched, inserted);
    assert_eq!(harness.store.list_contacts(wallet.id).await.unwrap(), vec![inserted]);
}

#[tokio::test]
async fn the_list_runs_in_chain_then_address_order() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "ordered").await.unwrap();
    for (chain, address) in
        [(ChainId::Ethereum, "0xb"), (ChainId::Bitcoin, "bc1qz"), (ChainId::Ethereum, "0xa")]
    {
        harness.store.insert_contact(contact(wallet.id, chain, address, "Someone")).await.unwrap();
    }

    let listed: Vec<String> = harness
        .store
        .list_contacts(wallet.id)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.address)
        .collect();
    assert_eq!(listed, ["bc1qz", "0xa", "0xb"]);
}

#[tokio::test]
async fn the_same_address_twice_on_one_chain_already_exists() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "dupes").await.unwrap();
    harness.store.insert_contact(contact(wallet.id, ChainId::Bsc, "0xsame", "One")).await.unwrap();

    let error = harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Bsc, "0xsame", "Two"))
        .await
        .expect_err("the second insert");
    assert!(
        matches!(
            error,
            StoreError::AlreadyExists { entity: "contact", columns: "wallet_id, chain, address" }
        ),
        "{error:?}"
    );

    harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Ethereum, "0xsame", "Other chain"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_edit_keeps_the_chain_and_refuses_a_duplicate() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "edits").await.unwrap();
    let first = harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Tron, "TFirst", "First"))
        .await
        .unwrap();
    harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Tron, "TTaken", "Taken"))
        .await
        .unwrap();

    let edit = ContactEdit {
        address: String::from("TMoved"),
        name: String::from("Renamed"),
        note: Some(String::from("Exchange")),
    };
    let edited = harness.store.update_contact(wallet.id, first.id, edit).await.unwrap();
    assert_eq!(edited.chain, ChainId::Tron);
    assert_eq!(edited.address, "TMoved");
    assert_eq!(edited.name, "Renamed");
    assert_eq!(edited.note.as_deref(), Some("Exchange"));
    assert_eq!(edited.id, first.id);

    let clash = ContactEdit { address: String::from("TTaken"), name: edited.name, note: None };
    let error =
        harness.store.update_contact(wallet.id, first.id, clash).await.expect_err("a duplicate");
    assert!(matches!(error, StoreError::AlreadyExists { entity: "contact", .. }), "{error:?}");
}

#[tokio::test]
async fn a_contact_under_a_wallet_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = harness
        .store
        .insert_contact(contact(404, ChainId::Bitcoin, "bc1qorphan", "Orphan"))
        .await
        .expect_err("no such wallet");
    assert!(matches!(error, StoreError::NotFound { entity: "wallet", .. }), "{error:?}");
}

#[tokio::test]
async fn an_empty_name_is_rejected_by_the_schema() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "nameless").await.unwrap();

    let error = harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Bitcoin, "bc1qnameless", ""))
        .await
        .expect_err("an empty name");
    assert!(matches!(error, StoreError::Rejected { entity: "contact", .. }), "{error:?}");
}

#[tokio::test]
async fn one_wallet_can_neither_see_nor_touch_another_wallets_contacts() {
    let harness = common::harness().await.unwrap();
    let owner = common::wallet(&harness.store, "owner").await.unwrap();
    let stranger = common::wallet(&harness.store, "stranger").await.unwrap();
    let kept = harness
        .store
        .insert_contact(contact(owner.id, ChainId::Solana, "So1", "Kept"))
        .await
        .unwrap();

    assert!(harness.store.list_contacts(stranger.id).await.unwrap().is_empty());
    let read = harness.store.get_contact(stranger.id, kept.id).await;
    assert!(matches!(read, Err(StoreError::NotFound { entity: "contact", .. })), "{read:?}");
    let edit =
        ContactEdit { address: String::from("So2"), name: String::from("Stolen"), note: None };
    let written = harness.store.update_contact(stranger.id, kept.id, edit).await;
    assert!(matches!(written, Err(StoreError::NotFound { entity: "contact", .. })), "{written:?}");
    let removed = harness.store.delete_contact(stranger.id, kept.id).await;
    assert!(matches!(removed, Err(StoreError::NotFound { entity: "contact", .. })), "{removed:?}");

    assert_eq!(harness.store.list_contacts(owner.id).await.unwrap(), vec![kept]);
}

#[tokio::test]
async fn a_deleted_contact_is_gone_and_a_second_delete_finds_nothing() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "deletes").await.unwrap();
    let gone = harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Litecoin, "ltc1qgone", "Gone"))
        .await
        .unwrap();

    harness.store.delete_contact(wallet.id, gone.id).await.unwrap();
    assert!(harness.store.list_contacts(wallet.id).await.unwrap().is_empty());
    let again = harness.store.delete_contact(wallet.id, gone.id).await;
    assert!(matches!(again, Err(StoreError::NotFound { entity: "contact", .. })), "{again:?}");
}

#[tokio::test]
async fn deleting_a_wallet_deletes_its_contacts() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "doomed").await.unwrap();
    harness
        .store
        .insert_contact(contact(wallet.id, ChainId::Monero, "4doomed", "Doomed"))
        .await
        .unwrap();

    harness.store.delete_wallet(wallet.id).await.unwrap();
    assert!(harness.store.list_contacts(wallet.id).await.unwrap().is_empty());
}
