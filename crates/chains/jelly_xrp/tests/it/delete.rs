use jelly_core::{
    Amount, ChainAdapter as _, ChainError, ChainId, Deadline, FeeSelection, SendAmount,
    TxDirection, TxOutcome, TxWarningKind, WalletEvent,
    test_util::{FakeEventSink, WatchedAddressBuilder, records},
};
use jelly_xrp::Method;
use serde_json::{Value, json};

use super::common::{
    LSF_DEPOSIT_AUTH, LSF_REQUIRE_DEST_TAG, Node, OWNER, assert_refused, deletion, key, transaction,
};

const DELETED: &str = "rf2c74F1Z2BrvL1dV7WMHYo6Jyaw446Fre";

#[tokio::test]
async fn a_deletion_pays_out_everything_but_the_owner_reserve_it_burns_as_its_fee() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let quote = adapter.quote_fee(&deletion()).await.unwrap();
    assert_eq!(quote.fee.base_units(), 200_000, "the fee is the live owner reserve");
    assert_eq!(quote.max_amount.map(Amount::base_units), Some(9_800_000));
    assert!(node.calls_to(Method::Fee).is_empty(), "a deletion is not priced like a payment");
    let blockers = node.calls_to(Method::AccountObjects);
    assert_eq!(blockers[0]["deletion_blockers_only"], true);
    assert_eq!(blockers[0]["account"], OWNER);

    assert_eq!(quote.custom, None, "a deletion's fee is the reserve, so it takes no custom fee");

    let mut custom = deletion();
    custom.fee = FeeSelection::Total { fee: Amount::from_base_units(300_000) };
    assert_refused(
        &adapter.quote_fee(&custom).await.unwrap_err(),
        "An account deletion pays the owner reserve as its fee, so it takes no custom fee.",
    );

    let tx = adapter.build_tx(&deletion()).await.unwrap();
    assert_eq!(tx.preview.amount.base_units(), 9_800_000);
    assert_eq!(tx.preview.fee.base_units(), 200_000);
    let warning = tx
        .preview
        .warnings
        .iter()
        .find(|warning| warning.kind == TxWarningKind::AccountDeletion)
        .unwrap();
    assert!(warning.message.contains("removes your XRP account from the ledger"));
    assert!(warning.message.contains("at least 1 XRP to recreate the account"));
    let detail = |label: &str| {
        tx.preview.details.iter().find(|row| row.label == label).map(|row| row.value.clone())
    };
    assert_eq!(detail("Transaction").as_deref(), Some("Account deletion"));
    assert_eq!(detail("Reserve to receive again").as_deref(), Some("1 XRP"));
    assert_eq!(detail("Account reserve"), None, "a deletion keeps no reserve back");

    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    let blob = hex::encode_upper(signed.raw.as_bytes());
    assert!(blob.starts_with("12001522800000002400000007201B"), "an AccountDelete: {blob}");
    assert_eq!(
        blob.get(38..56),
        Some("684000000000030D40"),
        "no Amount, and the owner reserve as Fee"
    );
    adapter.broadcast(&signed).await.unwrap();
}

#[tokio::test]
async fn the_deletion_fee_follows_the_server_and_an_absurd_or_unaffordable_one_is_refused() {
    let node = Node::new();
    node.state.lock().reserve_inc = 2_000_000;
    let adapter = node.adapter().await.unwrap();
    let quote = adapter.quote_fee(&deletion()).await.unwrap();
    assert_eq!(
        (quote.fee.base_units(), quote.max_amount.unwrap().base_units()),
        (2_000_000, 8_000_000)
    );
    node.state.lock().reserve_inc = 5_000_001;
    assert_refused(
        &adapter.build_tx(&deletion()).await.unwrap_err(),
        "The XRP account deletion fee is unusually high. Try again later.",
    );
    node.state.lock().reserve_inc = 200_000;
    node.state.lock().balance_drops = 200_000;
    assert!(matches!(
        adapter.build_tx(&deletion()).await.unwrap_err(),
        ChainError::InsufficientFunds { .. }
    ));
}

#[tokio::test]
async fn a_deletion_of_less_than_the_whole_balance_is_refused() {
    let adapter = Node::new().adapter().await.unwrap();
    let mut request = deletion();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(1_000_000) };
    assert_refused(
        &adapter.build_tx(&request).await.unwrap_err(),
        "An account deletion sends the whole balance. Choose the maximum amount.",
    );
}

#[tokio::test]
async fn a_deletion_pays_only_an_existing_recipient_that_accepts_it() {
    let node = Node::new();
    node.state.lock().recipient.is_funded = false;
    let adapter = node.adapter().await.unwrap();
    assert_refused(
        &adapter.build_tx(&deletion()).await.unwrap_err(),
        "An account deletion can only pay an XRP account that already exists.",
    );
    node.state.lock().recipient.is_funded = true;
    node.state.lock().recipient.flags = LSF_REQUIRE_DEST_TAG;
    assert_refused(
        &adapter.build_tx(&deletion()).await.unwrap_err(),
        "This recipient requires a destination tag.",
    );
    node.state.lock().recipient.flags = LSF_DEPOSIT_AUTH;
    assert_refused(
        &adapter.build_tx(&deletion()).await.unwrap_err(),
        "This recipient has not authorized deposits from your XRP account.",
    );
    node.state.lock().recipient.is_authorized = true;
    adapter.build_tx(&deletion()).await.unwrap();
}

/// rippled answers `tecTOO_SOON` while the Sequence plus 255 is above the ledger it
/// applies the deletion in, which follows the validated one.
#[tokio::test]
async fn an_account_used_within_256_ledgers_is_too_new_to_delete() {
    let node = Node::new();
    node.state.lock().sender.sequence = 90_000_000 - 256;
    let adapter = node.adapter().await.unwrap();
    assert_refused(
        &adapter.build_tx(&deletion()).await.unwrap_err(),
        "This XRP account is too new to delete. Try again in about 20 minutes.",
    );
    node.state.lock().sender.sequence = 90_000_000 - 257;
    adapter.build_tx(&deletion()).await.unwrap();
}

#[tokio::test]
async fn an_account_that_owns_a_deletion_blocker_cannot_be_deleted() {
    let node = Node::new();
    let line = json!({"LedgerEntryType":"RippleState","Balance":{"currency":"USD","value":"0"}});
    node.state.lock().object_pages.push_back(json!({
        "account":OWNER,"validated":true,"account_objects":[line],
    }));
    let adapter = node.adapter().await.unwrap();
    let refusal = "This XRP account holds trust lines, escrows, payment channels, checks, or other objects that must be removed before it can be deleted.";
    assert_refused(&adapter.build_tx(&deletion()).await.unwrap_err(), refusal);

    let empty_page = json!({"account":OWNER,"validated":true,"account_objects":[],
        "marker":"ABCD"});
    node.state.lock().object_pages.push_back(empty_page.clone());
    let quote = adapter.quote_fee(&deletion()).await.unwrap();
    assert_eq!(quote.fee.base_units(), 200_000, "a marker is followed to the end");
    let pages = node.calls_to(Method::AccountObjects);
    assert_eq!(pages.last().unwrap()["marker"], "ABCD");

    for _ in 0..3 {
        node.state.lock().object_pages.push_back(empty_page.clone());
    }
    assert_refused(&adapter.build_tx(&deletion()).await.unwrap_err(), refusal);
}

#[tokio::test]
async fn a_deletion_refused_with_a_tec_result_records_failed_with_the_fee_charged() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let tx = adapter.build_tx(&deletion()).await.unwrap();
    let signed = adapter.sign_tx(&tx, &[key().unwrap()]).unwrap();
    node.state.lock().engine_result = String::from("tecHAS_OBLIGATIONS");
    adapter.broadcast(&signed).await.unwrap();
    let mut applied = transaction(signed.txid.value(), "tecHAS_OBLIGATIONS", 10_000_000, 9_800_000);
    applied["TransactionType"] = "AccountDelete".into();
    applied["Fee"] = "200000".into();
    applied.as_object_mut().unwrap().remove("Amount");
    node.state.lock().tx = Some(applied);
    node.state.lock().ledger = 90_000_001;
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let record = sink.records().into_iter().find(|record| record.txid == signed.txid).unwrap();
    assert_eq!(record.outcome, TxOutcome::Failed);
    assert!(record.amount.is_zero());
    assert_eq!(record.fee.map(Amount::base_units), Some(200_000));
    adapter.build_tx(&deletion()).await.unwrap();
}

/// The real mainnet `AccountDelete`
/// 1AF19BF9717DA0B05A3BFC5007873E7743BA54C0311CCCCC60776AAEAC5C4635, as rippled's `tx`
/// returns it and as `account_tx` lists it for the account it deleted.
#[tokio::test]
async fn a_deleted_account_reads_as_empty_and_its_history_is_still_read() {
    let node = Node::new();
    let row: Value = serde_json::from_str(include_str!("../fixtures/account_delete.json")).unwrap();
    {
        let mut state = node.state.lock();
        state.recipient.is_funded = false;
        state.ledger = 75_728_700;
        state.pages.push_back(json!({"account":DELETED,"validated":true,
            "ledger_index_min":32_570,"ledger_index_max":75_728_700,"transactions":[row]}));
    }
    let adapter = node.adapter().await.unwrap();
    let watched = WatchedAddressBuilder::new(2, ChainId::Xrp, DELETED).build();
    adapter.set_watch_set(&[watched]).await.unwrap();
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let events = sink.drain();
    assert!(
        events.iter().any(|event| matches!(event,
        WalletEvent::BalanceChanged(update) if update.address_id == 2 && update.balance.is_zero()))
    );
    let record = records(&events).into_iter().next().unwrap();
    assert_eq!(
        record.txid.value(),
        "1AF19BF9717DA0B05A3BFC5007873E7743BA54C0311CCCCC60776AAEAC5C4635"
    );
    assert_eq!(record.direction, TxDirection::Outgoing);
    assert_eq!(record.outcome, TxOutcome::Success);
    assert_eq!(record.amount.base_units(), 12_665_795, "the delivered amount");
    assert_eq!(record.fee.map(Amount::base_units), Some(2_000_000));
    assert_eq!(record.counterparty.as_deref(), Some("rf1BiGeXwwQoi8Z2ueFYTEXSwuJYfV2Jpn"));
}
