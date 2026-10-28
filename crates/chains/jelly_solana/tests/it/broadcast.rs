use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{
    ChainAdapter as _, ChainError, ChainId, Clock as _, CursorScope, CursorStore as _, RawTx,
    RejectionKind, ReservationSlot, RetryPolicy, SignedTx, TxId,
    test_util::{FakeClock, FakeEventSink, RefusingCursorStore},
};
use jelly_solana::{Method, SolanaAdapter};
use serde_json::json;

use crate::common::{
    Answer, FakeNode, Fallible, Harness, OWNER, PEER, native, owner_key, request, statuses, watched,
};

async fn deliver(harness: &Harness) -> Fallible<Result<SignedTx, ChainError>> {
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()])?;
    Ok(match harness.adapter.broadcast(&signed).await {
        Ok(txid) => {
            assert_eq!(txid, signed.txid, "an accepted broadcast reports its own signature");
            Ok(signed)
        }
        Err(error) => Err(error),
    })
}

const fn rejection(error: &ChainError) -> Option<(RejectionKind, RetryPolicy)> {
    let ChainError::Rejected(ref rejection) = *error else { return None };
    Some((rejection.kind, rejection.retry))
}

#[tokio::test]
async fn an_accepted_transaction_reports_the_signature_of_its_own_bytes() {
    let harness = Harness::priced().await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    harness.node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));

    let txid = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(txid, signed.txid);
    assert_eq!(harness.adapter.in_flight(), 1, "the send is followed up");
}

#[tokio::test]
async fn a_node_that_echoes_another_signature_is_not_believed() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(
        Method::SendTransaction,
        Answer::ok(&json!("4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi")),
    );
    let error = deliver(&harness).await.unwrap().expect_err("a foreign signature");
    assert!(matches!(error, ChainError::Rpc(_)), "{error}");
    assert_eq!(harness.adapter.in_flight(), 1, "the outcome is unknown, so it is still followed");
}

#[tokio::test]
async fn a_transaction_the_network_already_has_counts_as_sent() {
    let harness = Harness::priced().await.unwrap();
    harness
        .node
        .always(Method::SendTransaction, Answer::preflight(&json!("AlreadyProcessed"), &[]));
    deliver(&harness).await.unwrap().expect("a transaction the network has is a success");
    assert_eq!(harness.adapter.in_flight(), 1);
    let blob = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::SolanaSends)
        .await
        .unwrap()
        .expect("a stored blob");
    let text = blob.with_bytes(|bytes| String::from_utf8(bytes.to_vec())).unwrap();
    assert!(text.contains("\"sent\":true"), "{text}");
}

#[tokio::test]
async fn an_expired_blockhash_is_reported_as_not_landed() {
    let harness = Harness::priced().await.unwrap();
    harness
        .node
        .always(Method::SendTransaction, Answer::preflight(&json!("BlockhashNotFound"), &[]));
    let error = deliver(&harness).await.unwrap().expect_err("a stale reference");
    assert_eq!(rejection(&error), Some((RejectionKind::StaleReference, RetryPolicy::DidNotLand)));
    assert_eq!(harness.adapter.in_flight(), 0, "a refused transaction is not followed");
}

#[tokio::test]
async fn a_failing_instruction_is_final_everywhere_and_keeps_its_log_line() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(
        Method::SendTransaction,
        Answer::preflight(
            &json!({ "InstructionError": [2, { "Custom": 1 }] }),
            &["Program log: Error: insufficient funds"],
        ),
    );
    let error = deliver(&harness).await.unwrap().expect_err("a reverted instruction");
    assert_eq!(rejection(&error), Some((RejectionKind::Reverted, RetryPolicy::FinalEverywhere)));
    assert!(error.to_string().contains("insufficient funds"), "{error}");
}

#[tokio::test]
async fn a_rent_shortfall_is_reported_as_a_dust_refusal() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(
        Method::SendTransaction,
        Answer::preflight(&json!({ "InsufficientFundsForRent": { "account_index": 1 } }), &[]),
    );
    let error = deliver(&harness).await.unwrap().expect_err("below the minimum");
    assert_eq!(rejection(&error), Some((RejectionKind::Dust, RetryPolicy::FinalEverywhere)));
}

#[tokio::test]
async fn a_refusal_with_no_name_did_not_land() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(
        Method::SendTransaction,
        Answer::preflight(&json!("SomethingTheRuntimeAddedLater"), &[]),
    );
    let error = deliver(&harness).await.unwrap().expect_err("an unclassified refusal");
    assert_eq!(rejection(&error), Some((RejectionKind::Unclassified, RetryPolicy::DidNotLand)));
    assert_eq!(harness.adapter.in_flight(), 0);
}

#[tokio::test]
async fn a_preflight_code_with_no_data_is_still_a_refusal() {
    let harness = Harness::priced().await.unwrap();
    harness
        .node
        .always(Method::SendTransaction, Answer::refused(-32002, "Transaction simulation failed"));
    let error = deliver(&harness).await.unwrap().expect_err("a refusal");
    assert!(matches!(error, ChainError::Rejected(_)), "{error}");
}

#[tokio::test]
async fn an_endpoint_that_never_answered_leaves_the_outcome_unknown() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(Method::SendTransaction, Answer::Unreachable);
    let error = deliver(&harness).await.unwrap().expect_err("no answer");
    assert!(matches!(error, ChainError::EndpointsDown { .. }), "{error}");
    assert_eq!(harness.adapter.in_flight(), 1, "unknown is not failed");
    assert_eq!(harness.node.count(Method::SendTransaction), 1, "one attempt, one endpoint");
}

#[tokio::test]
async fn a_node_that_is_behind_is_a_host_failure_rather_than_a_refusal() {
    let harness = Harness::priced().await.unwrap();
    harness
        .node
        .always(Method::SendTransaction, Answer::refused(-32005, "Node is behind by 500 slots"));
    let error = deliver(&harness).await.unwrap().expect_err("a host failure");
    assert!(rejection(&error).is_none(), "{error}");
    assert_eq!(harness.adapter.in_flight(), 1, "the outcome is unknown");
}

#[tokio::test]
async fn a_signed_transaction_from_another_chain_never_reaches_an_endpoint() {
    let harness = Harness::priced().await.unwrap();
    let foreign = SignedTx {
        raw: RawTx::from_bytes(vec![1, 2, 3]),
        txid: TxId::new(ChainId::Ethereum, "0xabc"),
    };
    let error = harness.adapter.broadcast(&foreign).await.expect_err("another chain");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error}");
    assert_eq!(harness.node.count(Method::SendTransaction), 0);
}

#[tokio::test]
async fn a_first_attempt_asks_the_node_to_simulate_it() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(Method::SendTransaction, Answer::Unreachable);
    let _ = deliver(&harness).await.unwrap();
    let call = harness.node.calls_to(Method::SendTransaction).into_iter().next().expect("a send");
    assert!(call.contains(r#""skipPreflight":false"#), "{}", call.params);
    assert!(call.contains(r#""encoding":"base64""#), "{}", call.params);
    assert!(!call.contains("maxRetries"), "the node keeps forwarding: {}", call.params);
}

/// The caller is told the send failed, so the bytes must never leave afterward,
/// or a second payment the user makes in its place lands beside it.
#[tokio::test]
async fn a_send_whose_record_could_not_be_written_is_never_sent_later() {
    let node = FakeNode::new();
    node.priced().unwrap();
    let clock = Arc::new(FakeClock::default());
    let store = Arc::new(RefusingCursorStore::default());
    let adapter = SolanaAdapter::builder(
        Arc::<FakeNode>::clone(&node),
        Arc::<RefusingCursorStore>::clone(&store),
        Arc::<FakeClock>::clone(&clock),
    )
    .build();
    adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
    let unsigned = adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
    node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));

    store.reservation_refusal().store(true, Ordering::SeqCst);
    adapter.broadcast(&signed).await.expect_err("the send could not be recorded");
    assert_eq!(node.count(Method::SendTransaction), 0, "the bytes did not leave");

    node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));
    adapter.sync(clock.deadline_in(Duration::from_secs(60)), &FakeEventSink::new()).await.unwrap();
    assert_eq!(
        node.count(Method::SendTransaction),
        0,
        "a send the caller was told failed was broadcast by a later pass"
    );
}

mod overlapping_pass {
    use std::sync::atomic::AtomicBool;

    use async_trait::async_trait;
    use jelly_core::test_util::MemoryCursorStore;
    use jelly_solana::{
        Answered, BroadcastReply, RpcCall, SolanaError, SolanaNode, test_util::endpoint,
    };
    use tokio::sync::Notify;

    use super::*;

    /// What a node behind the cluster answers a first attempt with.
    const BEHIND: &str = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32002,
        "message":"Transaction simulation failed: Blockhash not found",
        "data":{"err":"BlockhashNotFound","logs":[]}}}"#;

    /// Holds a send's first attempt until released and then refuses it, so a sync
    /// pass can run while that attempt is in flight. Every other call, a resend
    /// included, reaches the scripted node.
    #[derive(Debug)]
    struct SlowRefusal {
        inner: Arc<FakeNode>,
        is_armed: AtomicBool,
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl SolanaNode for SlowRefusal {
        async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
            self.inner.call(call).await
        }

        async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
            self.inner.batch(calls).await
        }

        async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
            let is_first_attempt = call.params().get().contains(r#""skipPreflight":false"#);
            if is_first_attempt && self.is_armed.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
                return BroadcastReply::read(&endpoint(), BEHIND);
            }
            self.inner.broadcast(call).await
        }
    }

    /// A pass that overlaps a first attempt resends its bytes without preflight.
    ///
    /// The send is marked sent before its first attempt leaves, so a refusal of
    /// that attempt says nothing about whether the resent bytes were forwarded.
    #[tokio::test]
    async fn a_refused_first_attempt_never_drops_a_send_an_overlapping_pass_forwarded() {
        let node = FakeNode::new();
        node.priced().unwrap();
        let held = Arc::new(SlowRefusal {
            inner: Arc::clone(&node),
            is_armed: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let clock = Arc::new(FakeClock::default());
        let adapter = SolanaAdapter::builder(
            Arc::<SlowRefusal>::clone(&held),
            Arc::new(MemoryCursorStore::new()),
            Arc::<FakeClock>::clone(&clock),
        )
        .build();
        adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
        let unsigned = adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        let signed = adapter.sign_tx(&unsigned, &[owner_key()]).unwrap();
        node.always(Method::SendTransaction, Answer::ok(&json!(signed.txid.value())));
        node.always(Method::GetSignatureStatuses, Answer::ok(&statuses(&[None])));

        let events = FakeEventSink::new();
        let deadline = clock.deadline_in(Duration::from_secs(60));
        let overlapping = async {
            held.entered.notified().await;
            let progress = adapter.sync(deadline, &events).await;
            held.release.notify_one();
            progress
        };
        let (sent, progress) = tokio::join!(adapter.broadcast(&signed), overlapping);
        progress.unwrap();

        let forwarded = node
            .calls_to(Method::SendTransaction)
            .iter()
            .filter(|call| call.contains(r#""skipPreflight":true"#))
            .count();
        let is_reported_unsent = sent.as_ref().is_err_and(|error| {
            rejection(error).is_some_and(|(_, retry)| {
                matches!(retry, RetryPolicy::DidNotLand | RetryPolicy::FinalEverywhere)
            })
        });
        assert!(
            forwarded == 0 || (adapter.in_flight() == 1 && !is_reported_unsent),
            "expected bytes a resend forwarded to stay followed and not be reported as never \
             sent; {forwarded} resend reached an endpoint, the broadcast answered {sent:?}, and \
             {} sends are still followed",
            adapter.in_flight()
        );
    }
}

#[tokio::test]
async fn a_send_is_persisted_as_sent_even_when_no_endpoint_answers() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(Method::SendTransaction, Answer::Unreachable);
    let before = harness.cursors.reserve_count();
    let _ = deliver(&harness).await.unwrap();
    assert!(harness.cursors.reserve_count() > before, "the send was reserved");
    let blob = harness
        .cursors
        .load_reservation(CursorScope::Address(7), ReservationSlot::SolanaSends)
        .await
        .unwrap()
        .expect("a stored blob");
    let text = blob.with_bytes(|bytes| String::from_utf8(bytes.to_vec())).unwrap();
    assert!(text.contains("\"sent\":true"), "{text}");
}
