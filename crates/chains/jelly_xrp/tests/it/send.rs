use jelly_core::{Amount, ChainAdapter as _, ChainError, FeeLevel, FeeSelection, SendAmount};
use jelly_xrp::ENTRY;

use super::common::{
    BUSY, LSF_DEPOSIT_AUTH, LSF_DISABLE_MASTER, LSF_REQUIRE_DEST_TAG, Node, RECIPIENT,
    assert_refused, request,
};

#[tokio::test]
async fn send_max_leaves_the_live_base_and_owner_reserve() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 10);
    assert_eq!(quote.max_amount.unwrap().base_units(), 8_599_990);
    let tx = adapter.build_tx(&request).await.unwrap();
    assert_eq!(tx.preview.amount, quote.max_amount.unwrap());
    assert!(
        tx.preview
            .details
            .iter()
            .any(|row| row.label == "Account reserve" && row.value == "1.4 XRP")
    );
}

/// The normal level pays the open ledger's cost, and a custom fee only has to be
/// enough to be queued for a later ledger.
#[tokio::test]
async fn a_custom_fee_is_paid_as_given_between_the_queue_minimum_and_the_ceiling() {
    let node = Node::new();
    node.state.lock().fee = String::from("12");
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 12);
    assert_eq!(quote.custom, Some(FeeSelection::Total { fee: Amount::from_base_units(12) }));
    let levels: Vec<(FeeLevel, u128)> =
        quote.levels.iter().map(|row| (row.level, row.fee.base_units())).collect();
    assert_eq!(levels, [(FeeLevel::Normal, 12)]);

    request.fee = FeeSelection::Total { fee: Amount::from_base_units(9) };
    assert_refused(
        &adapter.quote_fee(&request).await.unwrap_err(),
        "The fee of 0.000009 XRP is below the network minimum of 0.00001 XRP.",
    );
    request.fee = FeeSelection::Total { fee: Amount::from_base_units(100_001) };
    assert_refused(
        &adapter.quote_fee(&request).await.unwrap_err(),
        "The fee can be at most 0.1 XRP.",
    );

    request.fee = FeeSelection::Total { fee: Amount::from_base_units(10) };
    let quote = adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.fee.base_units(), 10, "below the open ledger's cost it waits in the queue");
    assert_eq!(quote.levels.first().map(|row| row.fee.base_units()), Some(12));
    let tx = adapter.build_tx(&request).await.unwrap();
    assert_eq!(tx.preview.fee.base_units(), 10);
}

#[tokio::test]
async fn reserve_changes_are_read_not_hardcoded() {
    let node = Node::new();
    node.state.lock().reserve_base = 2_000_000;
    node.state.lock().reserve_inc = 500_000;
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Max;
    assert_eq!(
        adapter.quote_fee(&request).await.unwrap().max_amount.unwrap().base_units(),
        6_999_990
    );
}

#[tokio::test]
async fn exact_payments_cannot_spend_the_reserve() {
    let adapter = Node::new().adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(8_600_000) };
    assert!(matches!(
        adapter.build_tx(&request).await.unwrap_err(),
        ChainError::InsufficientFunds { .. }
    ));
}

#[tokio::test]
async fn activation_requires_the_live_base_reserve() {
    let node = Node::new();
    node.state.lock().recipient.is_funded = false;
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    request.amount = SendAmount::Exact { value: Amount::from_base_units(999_999) };
    assert_refused(
        &adapter.build_tx(&request).await.unwrap_err(),
        "The payment is below the reserve needed to activate the recipient's XRP account.",
    );
    request.amount = SendAmount::Exact { value: Amount::from_base_units(1_000_000) };
    let tx = adapter.build_tx(&request).await.unwrap();
    assert!(tx.preview.warnings.iter().any(|warning| warning.message.contains("activates")));
}

#[tokio::test]
async fn required_tags_include_zero_and_are_visible_before_signing() {
    let node = Node::new();
    node.state.lock().recipient.flags = LSF_REQUIRE_DEST_TAG;
    let adapter = node.adapter().await.unwrap();
    let mut request = request();
    assert_refused(
        &adapter.build_tx(&request).await.unwrap_err(),
        "This recipient requires a destination tag.",
    );
    request.destination_tag = Some(0);
    let tx = adapter.build_tx(&request).await.unwrap();
    assert!(
        tx.preview.details.iter().any(|row| row.label == "Destination tag" && row.value == "0")
    );
    assert_eq!(tx.preview.destination_tag, Some(0));
    adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    request.destination_tag = Some(u32::MAX);
    adapter.build_tx(&request).await.unwrap();
}

#[tokio::test]
async fn x_addresses_keep_tags_and_refuse_conflicts() {
    let adapter = Node::new().adapter().await.unwrap();
    let mut request = request();
    request.to = ENTRY.validate_address(&format!("xrpl:{RECIPIENT}?dt=123")).unwrap().into_value();
    request.destination_tag = Some(456);
    assert_refused(
        &adapter.build_tx(&request).await.unwrap_err(),
        "The address and destination tag do not match.",
    );
    request.destination_tag = None;
    let tx = adapter.build_tx(&request).await.unwrap();
    assert_eq!(tx.preview.to, RECIPIENT);
    assert!(
        tx.preview.details.iter().any(|row| row.label == "Destination tag" && row.value == "123")
    );
    assert_eq!(tx.preview.destination_tag, Some(123));
}

#[tokio::test]
async fn disabled_master_keys_and_unauthorized_deposits_are_refused() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    node.state.lock().sender.flags = LSF_DISABLE_MASTER;
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "This XRP account has disabled its master key and cannot send from Jelly.",
    );
    node.state.lock().sender.flags = 0;
    node.state.lock().recipient.flags = LSF_DEPOSIT_AUTH;
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "This recipient has not authorized deposits from your XRP account.",
    );
    node.state.lock().recipient.is_authorized = true;
    adapter.build_tx(&request()).await.unwrap();
}

#[tokio::test]
async fn unvalidated_accounts_and_excessive_or_malformed_fees_are_refused() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    node.state.lock().is_validated = false;
    assert!(matches!(adapter.build_tx(&request()).await.unwrap_err(), ChainError::Rpc(_)));
    node.state.lock().is_validated = true;
    node.state.lock().fee = String::from("100001");
    assert_refused(
        &adapter.build_tx(&request()).await.unwrap_err(),
        "The XRP network fee is unusually high. Try again later.",
    );
    for fee in ["1e3", "-1", "18446744073709551616"] {
        node.state.lock().fee = fee.to_owned();
        assert!(matches!(adapter.build_tx(&request()).await.unwrap_err(), ChainError::Rpc(_)));
    }
}

#[tokio::test]
async fn one_account_cannot_build_two_competing_payments() {
    let adapter = Node::new().adapter().await.unwrap();
    let request = request();
    let (first, second) = tokio::join!(adapter.build_tx(&request), adapter.build_tx(&request));
    let (tx, refusal) = match (first, second) {
        (Ok(tx), Err(refusal)) | (Err(refusal), Ok(tx)) => (tx, refusal),
        (first, second) => panic!("expected exactly one payment, got {first:?} and {second:?}"),
    };
    assert_refused(&refusal, BUSY);
    adapter.pending_tracker().unwrap().discard(&tx).await.unwrap();
    adapter.build_tx(&request).await.unwrap();
}
