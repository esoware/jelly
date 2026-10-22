use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{
    ChainAdapter as _, ChainError, ChainId, Clock, CursorScope, CursorStore, RawTx, RejectionKind,
    ReservationSlot, RetryPolicy, SignedTx, TxId, TxOutcome,
    test_util::{FakeClock, RefusingCursorStore},
};
use jelly_evm::{EvmAdapter, Method};

use crate::common::{
    Answer, ETH, ETHEREUM, FakeNode, Fallible, GWEI, Harness, PEER, SIGNER, Setup, TEST_KEY, TIP,
    detail, fixture, key_material, payment, pinned, quantity, receipt_for, sheet, watched,
};

#[tokio::test]
async fn a_send_whose_record_failed_never_leaves_and_frees_its_nonce() {
    let store = Arc::new(RefusingCursorStore::default());
    let node = FakeNode::new();
    let as_store: Arc<dyn CursorStore> = Arc::<RefusingCursorStore>::clone(&store);
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let adapter = EvmAdapter::builder(ETHEREUM, Arc::<FakeNode>::clone(&node), as_store, clock)
        .build()
        .unwrap();
    adapter.set_watch_set(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    node.always(Method::GetTransactionCount, Answer::ok(&quantity(12)));
    node.always(Method::GetBalance, Answer::ok(&quantity(5 * ETH)));
    node.always(Method::FeeHistory, Answer::ok(&fixture("fee_history.json", "result").unwrap()));
    let request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);

    let unsigned = adapter.build_tx(&request).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key_material(TEST_KEY).unwrap()]).unwrap();
    store.reservation_refusal().store(true, Ordering::SeqCst);
    assert!(adapter.broadcast(&signed).await.is_err(), "the write before sending failed");
    assert!(node.calls_to(Method::SendRawTransaction).is_empty(), "so the bytes never left");

    store.reservation_refusal().store(false, Ordering::SeqCst);
    let next = adapter.build_tx(&request).await.unwrap();
    assert_eq!(
        detail(&next.preview.details, "Nonce"),
        "12",
        "a transaction no node was ever sent cannot confirm, so its nonce is free"
    );
}

/// A wallet holding a signed payment at nonce 7 that it has not broadcast.
async fn signed() -> Fallible<(Harness, SignedTx)> {
    let harness = Harness::signer(Setup::default(), 7).await?;
    let unsigned = harness.adapter.build_tx(&payment(PEER, 7)).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY)?])?;
    Ok((harness, signed))
}

#[tokio::test]
async fn delayed_signed_bytes_keep_their_nonce_through_later_quotes_and_builds() {
    let mut setup = Setup::default();
    setup.config.reservation_ttl = Duration::from_secs(1);
    let harness = Harness::signer(setup, 7).await.unwrap();
    let request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY).unwrap()]).unwrap();

    harness.clock.advance(Duration::from_secs(2));
    harness.adapter.quote_fee(&request).await.unwrap();
    let next = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(detail(&next.preview.details, "Nonce"), "8");

    harness
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    assert_eq!(harness.adapter.broadcast(&signed).await.unwrap(), signed.txid);
    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap();
    assert!(stored.is_some(), "the delayed send is durably tracked before broadcast");
}

#[tokio::test]
async fn discarded_signed_bytes_cannot_be_sent_after_the_nonce_is_reused() {
    let harness = Harness::signer(Setup::default(), 7).await.unwrap();
    let request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY).unwrap()]).unwrap();
    harness.adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    let next = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(detail(&next.preview.details, "Nonce"), "7");

    let error = harness.adapter.broadcast(&signed).await.expect_err("the reservation was released");
    assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
    assert!(harness.node.calls_to(Method::SendRawTransaction).is_empty());
}

#[tokio::test]
async fn a_send_the_network_accepts_answers_with_the_id_of_the_bytes_that_were_signed() {
    let (harness, signed) = signed().await.unwrap();
    harness
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));

    let id = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(id.value(), signed.txid.value());
    assert_eq!(id.chain(), ChainId::Ethereum);
    assert_eq!(harness.node.calls_to(Method::SendRawTransaction).len(), 1, "exactly one attempt");
}

#[tokio::test]
async fn an_endpoint_that_echoes_another_hash_does_not_get_to_name_the_transaction() {
    let (harness, signed) = signed().await.unwrap();
    let invented = format!("0x{}", "ab".repeat(32));
    assert_ne!(invented, signed.txid.value());
    harness.node.on(Method::SendRawTransaction, Answer::ok(&format!("\"{invented}\"")));

    let id = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(id.value(), signed.txid.value(), "the id is the one this build computed");
    assert_eq!(harness.adapter.in_flight(), 1, "the send is still followed under its own id");

    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .expect("a stored set");
    let text = stored.with_bytes(|bytes| String::from_utf8(bytes.to_vec())).unwrap();
    assert!(text.contains(&signed.txid.value().to_ascii_lowercase()), "{text}");
    assert!(!text.contains(&invented), "{text}");
}

#[tokio::test]
async fn the_watched_set_is_written_before_the_bytes_go_out() {
    let (harness, signed) = signed().await.unwrap();
    assert_eq!(
        harness
            .cursors
            .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
            .await
            .unwrap(),
        None,
        "a build that has not been sent is not persisted"
    );

    harness
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    harness.adapter.broadcast(&signed).await.unwrap();

    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .expect("a stored set");
    let text = stored.with_bytes(|bytes| String::from_utf8(bytes.to_vec())).unwrap();
    assert!(text.contains(&signed.txid.value().to_ascii_lowercase()), "{text}");
    assert!(text.contains("\"nonce\":7"), "{text}");
}

#[tokio::test]
async fn a_transaction_the_network_already_has_is_a_send_that_landed() {
    let (harness, signed) = signed().await.unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::refused("already known"));

    let id = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(id.value(), signed.txid.value(), "the id is derived from the bytes either way");
    assert_eq!(
        harness.adapter.in_flight(),
        1,
        "a landed transaction keeps its slot and is watched"
    );
}

#[tokio::test]
async fn a_refusal_the_node_is_sure_of_releases_the_nonce_slot() {
    for message in [
        "nonce too low: address 0xab, tx: 3 state: 5",
        "insufficient funds for gas * price + value",
        "intrinsic gas too low",
        "transaction underpriced",
    ] {
        let (harness, signed) = signed().await.unwrap();
        harness.node.on(Method::SendRawTransaction, Answer::refused(message));

        let error = harness.adapter.broadcast(&signed).await.expect_err("a refusal");
        let ChainError::Rejected(ref rejection) = error else {
            panic!("unexpected {error:?}");
        };
        assert_eq!(rejection.retry, RetryPolicy::FinalEverywhere, "{message}");
        assert_eq!(rejection.detail, message, "the node's own wording survives");
        assert_eq!(
            harness.adapter.in_flight(),
            0,
            "{message}: a transaction that can never confirm holds nothing"
        );
    }
}

#[tokio::test]
async fn a_nonce_gap_refusal_releases_the_nonce_slot() {
    let (harness, signed) = signed().await.unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::refused("nonce too high"));

    let error = harness.adapter.broadcast(&signed).await.expect_err("a gap");
    let ChainError::Rejected(ref rejection) = error else {
        panic!("unexpected {error:?}");
    };
    assert_eq!(rejection.kind, RejectionKind::NonceTooHigh);
    assert_eq!(rejection.retry, RetryPolicy::DidNotLand);
    assert_eq!(
        harness.adapter.in_flight(),
        0,
        "a refusal that says the transaction is in no mempool leaves no slot to hold"
    );
}

#[tokio::test]
async fn a_rebuild_after_a_nonce_gap_refusal_does_not_skip_ahead() {
    let harness = Harness::signer(Setup::default(), 7).await.unwrap();
    let request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    harness.send(&request).await.unwrap();

    let second = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(detail(&second.preview.details, "Nonce"), "8", "the first send holds 7");
    let second = harness.adapter.sign_tx(&second, &[key_material(TEST_KEY).unwrap()]).unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::refused("nonce too high"));
    harness.adapter.broadcast(&second).await.expect_err("a gap");

    let rebuilt = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(
        detail(&rebuilt.preview.details, "Nonce"),
        "8",
        "the refused nonce is free again, so the rebuild takes it"
    );
}

#[tokio::test]
async fn an_ambiguous_failure_keeps_the_slot_because_the_outcome_is_unknown() {
    let (harness, signed) = signed().await.unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::Unreachable);

    let error = harness.adapter.broadcast(&signed).await.expect_err("nothing answered");
    assert!(matches!(error, ChainError::EndpointsDown { .. }), "{error:?}");
    assert_eq!(harness.adapter.in_flight(), 1, "a transport failure is not a refusal");
}

#[tokio::test]
async fn an_ambiguous_send_remains_retryable_after_its_reservation_is_restored() {
    let (harness, signed) = signed().await.unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::Unreachable);
    harness.adapter.broadcast(&signed).await.expect_err("the outcome is unknown");
    harness
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    assert_eq!(harness.adapter.broadcast(&signed).await.unwrap(), signed.txid);

    let stored = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
        .await
        .unwrap()
        .expect("the reservation survived the ambiguous send");
    let restored = Harness::new(Setup::default()).unwrap();
    restored
        .cursors
        .reserve(CursorScope::Address(7), ReservationSlot::EvmNonces, stored)
        .await
        .unwrap();
    restored.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await.unwrap();
    restored
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    assert_eq!(restored.adapter.broadcast(&signed).await.unwrap(), signed.txid);
    assert_eq!(restored.adapter.in_flight(), 1);
}

/// The first attempt timed out after the node took the bytes.
///
/// The transaction was mined before the retry. Geth answers a mined transaction
/// that is sent again with nonce too low, because only its own pool is checked for a
/// known hash.
#[tokio::test]
async fn a_retried_send_refused_as_nonce_too_low_is_followed_to_its_receipt() {
    let harness = Harness::signer(Setup::default().one_window(), 7).await.unwrap();
    let unsigned = harness.adapter.build_tx(&payment(PEER, 7)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY).unwrap()]).unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::Unreachable);
    harness.adapter.broadcast(&signed).await.expect_err("the outcome is unknown");

    harness
        .node
        .on(Method::SendRawTransaction, Answer::refused("nonce too low: next nonce 8, tx nonce 7"));
    let retried = harness.adapter.broadcast(&signed).await;

    harness.node.quiet(&sheet(4 * ETH));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(8)));
    harness.node.always(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(signed.txid.value(), TIP - 1)),
    );
    harness.sync().await.unwrap();

    let records = harness.records();
    assert!(
        records
            .iter()
            .any(|record| record.txid == signed.txid && record.outcome == TxOutcome::Success),
        "a nonce too low answer to a send that already went out does not prove it is in no \
         mempool, so the mined payment must still be followed to its receipt, but the retry \
         answered {retried:?}, {} sends are followed and the pass recorded {records:?}",
        harness.adapter.in_flight()
    );
}

#[tokio::test]
async fn wording_no_table_row_matches_reaches_the_caller_and_keeps_the_slot() {
    let (harness, signed) = signed().await.unwrap();
    harness.node.on(Method::SendRawTransaction, Answer::refused("some brand new client wording"));

    let error = harness.adapter.broadcast(&signed).await.expect_err("unclassified");
    let ChainError::Rpc(ref text) = error else {
        panic!("unexpected {error:?}");
    };
    assert!(text.to_string().contains("some brand new client wording"), "{text}");
    assert!(text.to_string().contains("eth_sendRawTransaction"), "{text}");
    assert_eq!(harness.adapter.in_flight(), 1, "another endpoint might still take it");
}

#[tokio::test]
async fn a_signed_transaction_for_another_chain_never_reaches_the_network() {
    let harness = Harness::signer(Setup::default(), 7).await.unwrap();
    let foreign = SignedTx {
        raw: RawTx::from_bytes(vec![0x02]),
        txid: TxId::new(ChainId::Bsc, format!("0x{}", "11".repeat(32))),
    };
    let error = harness.adapter.broadcast(&foreign).await.expect_err("the wrong chain");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
    assert!(harness.node.calls_to(Method::SendRawTransaction).is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_transaction_id_that_is_not_thirty_two_bytes_is_refused() {
    let harness = Harness::signer(Setup::default(), 7).await.unwrap();
    let malformed =
        SignedTx { raw: RawTx::from_bytes(vec![0x02]), txid: TxId::new(ChainId::Ethereum, "0xab") };
    let error = harness.adapter.broadcast(&malformed).await.expect_err("not an id");
    assert!(matches!(error, ChainError::InvalidInput(..)), "{error:?}");
}
