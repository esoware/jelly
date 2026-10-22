use std::fmt::Debug;

use jelly_core::{ChainId, CoreError};

use crate::common::{PASSWORD, harness, secret};

const CHECKSUMMED: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
const BAD_CHECKSUM: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeD";
const BITCOIN: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

fn is_invalid(outcome: &Result<impl Debug, CoreError>, wanted: &str) -> bool {
    matches!(outcome, Err(CoreError::InvalidInput { field, .. }) if *field == wanted)
}

#[tokio::test]
async fn a_contact_is_saved_in_its_canonical_spelling_with_a_trimmed_name() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("book").await.unwrap();

    let lowercase = CHECKSUMMED.to_ascii_lowercase();
    let saved = harness
        .engine
        .add_contact(ChainId::Ethereum, &format!("  {lowercase} "), "  Alice ", Some("  "))
        .await
        .unwrap();
    assert_eq!(saved.address, CHECKSUMMED);
    assert_eq!(saved.name, "Alice");
    assert_eq!(saved.note, None);

    let listed = harness.engine.list_contacts().await.unwrap();
    assert_eq!(listed, vec![saved]);
}

#[tokio::test]
async fn an_address_the_chain_refuses_never_reaches_the_book() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("strict").await.unwrap();

    let typo = harness.engine.add_contact(ChainId::Ethereum, BAD_CHECKSUM, "Typo", None).await;
    assert!(is_invalid(&typo, "address"), "{typo:?}");
    let wrong_chain =
        harness.engine.add_contact(ChainId::Bitcoin, CHECKSUMMED, "Wrong", None).await;
    assert!(is_invalid(&wrong_chain, "address"), "{wrong_chain:?}");
    let nameless = harness.engine.add_contact(ChainId::Bitcoin, BITCOIN, " ", None).await;
    assert!(is_invalid(&nameless, "name"), "{nameless:?}");

    assert!(harness.engine.list_contacts().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_same_address_on_one_chain_is_saved_once() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("dupes").await.unwrap();
    harness.engine.add_contact(ChainId::Ethereum, CHECKSUMMED, "First", None).await.unwrap();

    let lowercase = CHECKSUMMED.to_ascii_lowercase();
    let again = harness.engine.add_contact(ChainId::Ethereum, &lowercase, "Second", None).await;
    assert!(matches!(again, Err(CoreError::AlreadyExists { entity: "contact" })), "{again:?}");

    harness.engine.add_contact(ChainId::Bsc, CHECKSUMMED, "Same key", None).await.unwrap();
}

#[tokio::test]
async fn an_edit_checks_the_new_address_against_the_saved_chain() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("edits").await.unwrap();
    let saved = harness.engine.add_contact(ChainId::Bitcoin, BITCOIN, "Bob", None).await.unwrap();

    let refused = harness.engine.update_contact(saved.id, CHECKSUMMED, "Bob", None).await;
    assert!(is_invalid(&refused, "address"), "{refused:?}");

    let edited = harness
        .engine
        .update_contact(saved.id, &format!(" {BITCOIN}"), "Robert ", Some(" Payroll "))
        .await
        .unwrap();
    assert_eq!(edited.chain, ChainId::Bitcoin);
    assert_eq!(edited.address, BITCOIN);
    assert_eq!(edited.name, "Robert");
    assert_eq!(edited.note.as_deref(), Some("Payroll"));

    harness.engine.delete_contact(saved.id).await.unwrap();
    assert!(harness.engine.list_contacts().await.unwrap().is_empty());
    let missing = harness.engine.delete_contact(saved.id).await;
    assert!(matches!(missing, Err(CoreError::NotFound { .. })), "{missing:?}");
}

#[tokio::test]
async fn each_wallet_keeps_its_own_book() {
    let harness = harness().await.unwrap();
    let first = harness.seed_wallet("first").await.unwrap();
    let kept = harness.engine.add_contact(ChainId::Bitcoin, BITCOIN, "Kept", None).await.unwrap();

    harness.seed_wallet("second").await.unwrap();
    assert!(harness.engine.list_contacts().await.unwrap().is_empty());
    let reached = harness.engine.update_contact(kept.id, BITCOIN, "Taken", None).await;
    assert!(matches!(reached, Err(CoreError::NotFound { .. })), "{reached:?}");
    let removed = harness.engine.delete_contact(kept.id).await;
    assert!(matches!(removed, Err(CoreError::NotFound { .. })), "{removed:?}");

    harness.engine.unlock_wallet(first.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert_eq!(harness.engine.list_contacts().await.unwrap(), vec![kept]);
}

#[tokio::test]
async fn a_locked_wallet_has_no_book_to_read_or_write() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("locked").await.unwrap();
    let saved =
        harness.engine.add_contact(ChainId::Bitcoin, BITCOIN, "Hidden", None).await.unwrap();
    harness.engine.lock_wallet().await;

    assert!(matches!(harness.engine.list_contacts().await, Err(CoreError::Locked)));
    let added = harness.engine.add_contact(ChainId::Ethereum, CHECKSUMMED, "Late", None).await;
    assert!(matches!(added, Err(CoreError::Locked)), "{added:?}");
    let removed = harness.engine.delete_contact(saved.id).await;
    assert!(matches!(removed, Err(CoreError::Locked)), "{removed:?}");
}
