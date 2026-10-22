use jelly_core::{ChainAdapter as _, ChainError, ChainId, RetryPolicy, SignedTx, TxId};
use jelly_tron::{
    Route,
    test_util::{OWNER, OWNER_KEY, RECIPIENT},
};
use serde_json::Value;

use crate::common::{
    Answer, Fallible, Harness, TRX, body_of, funded, key_material, ready, request,
};

async fn signed(harness: &Harness) -> Fallible<SignedTx> {
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await?;
    Ok(harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)])?)
}

#[expect(
    clippy::expect_used,
    reason = "a fixture helper outside a #[test] body is still test code and must fail loudly"
)]
fn body(text: &str) -> Value {
    serde_json::from_str(text).expect("a broadcast body is json")
}

#[tokio::test]
async fn an_accepted_transaction_reports_the_id_that_was_signed() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let txid = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(txid, signed.txid, "the id is sha256(raw_data), which this build already knew");
    assert_eq!(txid.chain(), ChainId::Tron);
    assert_eq!(harness.fake.count(Route::BroadcastHex), 1);
}

#[tokio::test]
async fn the_hex_route_carries_exactly_the_bytes_that_were_signed() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();

    let sent = harness.fake.last_body(Route::BroadcastHex).expect("a body");
    assert_eq!(
        body(&sent).get("transaction").and_then(Value::as_str),
        Some(hex::encode(signed.raw.as_bytes()).as_str())
    );
}

#[tokio::test]
async fn a_duplicate_is_a_send_that_already_landed() {
    let fake = funded(100_000_000).on(
        Route::BroadcastHex,
        Answer::body(
            r#"{"result":false,"code":"DUP_TRANSACTION_ERROR","message":"6475706c6963617465"}"#,
        ),
    );
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let txid = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(txid, signed.txid);
    assert_eq!(harness.adapter.in_flight(), 1, "a landed send is still followed until it settles");
}

#[tokio::test]
async fn a_rejection_the_network_is_sure_of_is_reported_and_stops_the_follow_up() {
    let fake = funded(100_000_000).on(
        Route::BroadcastHex,
        Answer::body(
            r#"{"result":false,"code":"CONTRACT_VALIDATE_ERROR",
                "message":"62616c616e6365206973206e6f742073756666696369656e74"}"#,
        ),
    );
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("a rejection");
    let ChainError::Rejected(rejection) = error else {
        panic!("a refusal travels as a rejection, not as an endpoint failure");
    };
    assert_eq!(rejection.retry, RetryPolicy::FinalEverywhere);
    assert!(rejection.detail.contains("balance is not sufficient"), "{}", rejection.detail);
    assert_eq!(
        harness.adapter.in_flight(),
        0,
        "bytes the network will always refuse are not worth following"
    );
    assert_eq!(harness.fake.count(Route::BroadcastHex), 1, "one endpoint, one attempt");
}

#[tokio::test]
async fn a_stale_reference_stops_the_follow_up_too_because_those_bytes_can_never_land() {
    let fake = funded(100_000_000)
        .on(Route::BroadcastHex, Answer::body(r#"{"result":false,"code":"TAPOS_ERROR"}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("a stale reference");
    let ChainError::Rejected(rejection) = error else { panic!("a refusal is a rejection") };
    assert_eq!(rejection.retry, RetryPolicy::DidNotLand);
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_busy_node_leaves_the_send_being_followed() {
    let fake = funded(100_000_000)
        .on(Route::BroadcastHex, Answer::body(r#"{"result":false,"code":"SERVER_BUSY"}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("a busy node");
    let ChainError::Rejected(rejection) = error else { panic!("a refusal is a rejection") };
    assert_eq!(rejection.retry, RetryPolicy::TryAnotherEndpoint);
    assert_eq!(harness.adapter.in_flight(), 1, "unknown is not failed");
}

#[tokio::test]
async fn a_transport_failure_leaves_the_send_being_followed() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::Transport);
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("nothing completed");
    assert!(!matches!(error, ChainError::Rejected(_)), "{error:?}");
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "a transport failure leaves the outcome unknown, so the send is still followed"
    );
}

#[tokio::test]
async fn a_node_without_the_hex_route_takes_the_json_form_in_the_same_attempt() {
    let fake = funded(100_000_000)
        .on(Route::BroadcastHex, Answer::Status(404))
        .on(Route::BroadcastTransaction, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let txid = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(txid, signed.txid);
    assert_eq!(harness.fake.count(Route::BroadcastTransaction), 1);

    // java-tron rebuilds the transaction from the decoded object and ignores
    // raw_data_hex, so the object has to be there and has to agree with the bytes.
    let sent = body(&harness.fake.last_body(Route::BroadcastTransaction).expect("a body"));
    assert_eq!(sent.get("txID").and_then(Value::as_str), Some(signed.txid.value()));
    let raw_data = sent.get("raw_data").expect("the decoded raw_data object");
    let contract = raw_data
        .get("contract")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .expect("one contract");
    assert_eq!(contract.get("type").and_then(Value::as_str), Some("TransferContract"));
    assert_eq!(
        contract
            .get("parameter")
            .and_then(|parameter| parameter.get("type_url"))
            .and_then(Value::as_str),
        Some("type.googleapis.com/protocol.TransferContract")
    );
    let signature = sent
        .get("signature")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(Value::as_str)
        .expect("one signature");
    assert_eq!(signature.len(), 130, "a tron signature is sixty-five bytes");
}

#[tokio::test]
async fn the_json_broadcast_sends_the_contract_body_as_an_object_java_tron_can_read() {
    let fake = funded(100_000_000)
        .on(Route::BroadcastHex, Answer::Status(404))
        .on(Route::BroadcastTransaction, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();

    let sent = body(&harness.fake.last_body(Route::BroadcastTransaction).unwrap());
    let value = sent.pointer("/raw_data/contract/0/parameter/value").unwrap();
    let owner = hex::encode(body_of(OWNER).unwrap());
    let recipient = hex::encode(body_of(RECIPIENT).unwrap());
    assert!(value.is_object(), "java-tron merges parameter.value as an object, got {value}");
    assert_eq!(value.get("owner_address").and_then(Value::as_str), Some(owner.as_str()));
    assert_eq!(value.get("to_address").and_then(Value::as_str), Some(recipient.as_str()));
    assert_eq!(value.get("amount").and_then(Value::as_i64), Some(1_000_000));
}

#[tokio::test]
async fn a_native_transfer_carries_no_fee_limit_in_the_json_form() {
    let fake = funded(100_000_000)
        .on(Route::BroadcastHex, Answer::Status(405))
        .on(Route::BroadcastTransaction, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();
    let sent = body(&harness.fake.last_body(Route::BroadcastTransaction).expect("a body"));
    let raw_data = sent.get("raw_data").expect("the decoded object");
    assert!(raw_data.get("fee_limit").is_none(), "a native transfer has no fee limit to state");
}

#[tokio::test]
async fn a_signed_transaction_from_another_chain_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    let foreign = SignedTx {
        raw: signed.raw.clone(),
        txid: TxId::new(ChainId::Ethereum, signed.txid.value()),
    };
    let error = harness.adapter.broadcast(&foreign).await.expect_err("not a tron transaction");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
}

#[tokio::test]
async fn a_signed_transaction_this_adapter_is_not_following_never_goes_out() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let signer = ready(&fake).await.unwrap();
    let signed = signed(&signer).await.unwrap();
    let stranger = ready(&fake).await.unwrap();
    let error = stranger.adapter.broadcast(&signed).await.expect_err("no record of this send");
    assert!(matches!(error, ChainError::Internal { .. }), "{error:?}");
    assert_eq!(fake.count(Route::BroadcastHex), 0, "bytes with no record never leave");
}

#[tokio::test]
async fn a_send_is_written_out_at_broadcast_and_not_at_signing() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let signed = signed(&harness).await.unwrap();
    assert_eq!(
        harness.cursors.len(),
        0,
        "a build that was signed and not sent is not a transaction"
    );
    harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(
        harness.cursors.len(),
        1,
        "the send is written before the bytes leave, so a crash still leaves a record"
    );
}

#[tokio::test]
async fn a_build_that_is_abandoned_is_forgotten() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let _ = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    let tracker = harness.adapter.pending_tracker().expect("tron tracks its sends");
    tracker.discard(&unsigned).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);
    assert_eq!(harness.cursors.len(), 0, "an abandoned build was never written out");
}

#[tokio::test]
async fn a_discard_after_the_bytes_have_left_does_not_stop_the_follow_up() {
    let fake = funded(100_000_000).on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(OWNER_KEY)]).unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    let tracker = harness.adapter.pending_tracker().expect("tron tracks its sends");
    tracker.discard(&unsigned).await.unwrap();
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "a transaction that reached an endpoint is followed whatever the interface asks"
    );
}
