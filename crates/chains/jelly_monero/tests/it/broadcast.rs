use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter as _, ChainError, CoreError, CursorBytes, CursorScope, CursorStore,
    ErrorCode, HeldReservation, RawTx, RejectionKind, ReservationSlot, RetryPolicy, ScanAdvance,
    ScanStream, SignedTx,
    test_util::{FakeClock, MemoryCursorStore},
};
use jelly_monero::test_util::{account_balance, in_flight, refusal, stored_scan, stored_sends};
use parking_lot::Mutex;

use crate::common::{ENDPOINT, FakeNode, Harness, config, fixture_adapter, images_in};

#[tokio::test]
async fn a_broadcast_reserves_its_inputs_and_persists_before_the_bytes_go_out() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();

    assert_eq!(harness.node.published().len(), 1);
    assert_eq!(in_flight(&harness.adapter), 1);
    let balance = account_balance(&harness.adapter, 0);
    assert_eq!(balance.total(), held, "an unspent coin is still held, not spent");
    assert!(balance.reserved() > Amount::ZERO, "and it is committed to the transaction");
    assert_eq!(
        balance.spendable().saturating_add(balance.reserved()),
        held,
        "the coins it did not touch are still spendable"
    );

    // The reservation is on disk before the bytes go out and carries them. The scan
    // blob only records progress and holds no signed transaction.
    let held_blob = stored_sends(&*harness.cursors)
        .await
        .unwrap()
        .expect("the send was recorded before it went out");
    let images = images_in(&signed).unwrap();
    assert_eq!(images.len(), 1);
    let image = images.first().expect("a key image");
    assert!(
        held_blob.with_bytes(|bytes| bytes.windows(32).any(|window| window == image.as_slice())),
        "the reservation names the key image a second send must not reuse"
    );
    let raw = signed.raw.as_bytes();
    assert!(
        held_blob.with_bytes(|bytes| bytes.windows(raw.len()).any(|window| window == raw)),
        "and the bytes a rebroadcast would resend"
    );
    let scan = stored_scan(&*harness.cursors).await.unwrap().expect("the scan blob was written");
    assert!(
        scan.with_bytes(|bytes| !bytes.windows(raw.len()).any(|window| window == raw)),
        "a signed transaction is not part of scan progress"
    );
}

#[tokio::test]
async fn an_accepted_transaction_reports_the_id_of_its_own_bytes() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();

    let txid = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(txid, signed.txid);
    assert_eq!(harness.node.published().first().map(Vec::as_slice), Some(signed.raw.as_bytes()));
}

#[tokio::test]
async fn the_same_signed_bytes_may_be_offered_twice() {
    let harness = Harness::new().await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let reserved = account_balance(&harness.adapter, 0).reserved();

    let again = harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(again, signed.txid);
    assert_eq!(in_flight(&harness.adapter), 1, "one transaction, one reservation");
    assert_eq!(account_balance(&harness.adapter, 0).reserved(), reserved);
    assert_eq!(
        harness.node.broadcast_endpoints(),
        vec![Some(ENDPOINT.to_owned()), Some(ENDPOINT.to_owned())],
        "both offers go to the node that served the ring"
    );
}

#[tokio::test]
async fn a_broadcast_leaves_the_ring_node_only_when_it_cannot_be_reached() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    harness.node.set_named_unreachable(true);
    harness.adapter.broadcast(&signed).await.unwrap();
    assert_eq!(harness.node.broadcast_endpoints(), vec![Some(ENDPOINT.to_owned()), None]);
    assert_eq!(harness.node.published().len(), 1);

    let refused = Harness::new().await.unwrap();
    refused.node.refuse_with(Some(refusal("fee too low")));
    let unsigned = refused.adapter.build_tx(&refused.request(Some(1_000))).await.unwrap();
    let signed = refused.adapter.sign_tx(&unsigned, &[refused.spend_key()]).unwrap();
    refused.adapter.broadcast(&signed).await.unwrap_err();
    assert_eq!(
        refused.node.broadcast_endpoints(),
        vec![Some(ENDPOINT.to_owned())],
        "a refusal is the ring node's answer, not a reason to ask another"
    );
}

#[derive(Debug)]
struct FailOnceReservationStore {
    stored: Arc<MemoryCursorStore>,
    node: Arc<FakeNode>,
    should_fail_reserve: AtomicBool,
    reserved_before_broadcast: Mutex<Option<CursorBytes>>,
}

#[async_trait]
impl CursorStore for FailOnceReservationStore {
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        self.stored.load_scans(scopes, streams).await
    }

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        self.stored.load_reservations(scopes, slots).await
    }

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        self.stored.advance(scope, stream, cursor).await
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        if self.should_fail_reserve.swap(false, Ordering::SeqCst) {
            return Err(CoreError::internal_without_source("the reservation store is unavailable"));
        }
        self.stored.reserve(scope, slot, payload.clone()).await?;
        if self.node.broadcast_endpoints().is_empty() {
            *self.reserved_before_broadcast.lock() = Some(payload);
        }
        Ok(())
    }
}

#[tokio::test]
async fn retrying_after_reservation_persistence_fails_never_sends_untracked_bytes() {
    let mut harness = Harness::new().await.unwrap();
    let cursors = Arc::new(FailOnceReservationStore {
        stored: Arc::clone(&harness.cursors),
        node: Arc::clone(&harness.node),
        should_fail_reserve: AtomicBool::new(false),
        reserved_before_broadcast: Mutex::new(None),
    });
    harness.adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&harness.node),
        Arc::<FailOnceReservationStore>::clone(&cursors),
        Arc::<FakeClock>::clone(&harness.clock),
        config(),
    )
    .await
    .unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    cursors.should_fail_reserve.store(true, Ordering::SeqCst);

    let error = harness.adapter.broadcast(&signed).await.expect_err("reservation storage failed");
    assert_eq!(error.code(), ErrorCode::Internal, "{error:?}");
    assert!(!cursors.should_fail_reserve.load(Ordering::SeqCst));
    assert!(harness.node.broadcast_endpoints().is_empty());
    let raw = signed.raw.as_bytes();
    let stored = stored_sends(&*cursors).await.unwrap().expect("the build stored its ring");
    assert!(
        stored.with_bytes(|bytes| !bytes.windows(raw.len()).any(|window| window == raw)),
        "the failed reservation stored no send"
    );
    *cursors.reserved_before_broadcast.lock() = None;

    let retry = harness.adapter.broadcast(&signed).await;
    let published = harness.node.published();
    assert!(
        published.is_empty() || cursors.reserved_before_broadcast.lock().is_some(),
        "retry delivered {} transaction(s) without a successful reservation before delivery: {retry:?}",
        published.len()
    );
    if published.is_empty() {
        assert!(retry.is_err(), "a refused retry must report its failure");
        assert!(harness.node.broadcast_endpoints().is_empty());
    } else {
        assert_eq!(retry.unwrap(), signed.txid);
        assert_eq!(published, vec![signed.raw.as_bytes().to_vec()]);
        assert_eq!(in_flight(&harness.adapter), 1);
        assert!(account_balance(&harness.adapter, 0).reserved() > Amount::ZERO);
        let before = cursors.reserved_before_broadcast.lock().clone().unwrap();
        let after = stored_sends(&*cursors).await.unwrap().expect("the send is durable");
        let raw = signed.raw.as_bytes();
        for held in [before, after] {
            assert!(held.with_bytes(|bytes| bytes.windows(raw.len()).any(|window| window == raw)));
            for image in images_in(&signed).unwrap() {
                assert!(held.with_bytes(|bytes| bytes.windows(32).any(|window| window == image)));
            }
        }
    }
}

#[tokio::test]
async fn a_refusal_the_network_will_never_accept_releases_the_inputs_at_once() {
    let harness = Harness::new().await.unwrap();
    let spendable = account_balance(&harness.adapter, 0).spendable();
    harness.node.refuse_with(Some(refusal("double spend")));

    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("a double spend");
    assert_eq!(error.code(), ErrorCode::Rejected, "{error}");
    assert!(
        matches!(&error, ChainError::Rejected(rejection)
            if rejection.kind == RejectionKind::DoubleSpend),
        "{error}"
    );

    assert_eq!(in_flight(&harness.adapter), 0, "nothing is in flight");
    assert_eq!(
        account_balance(&harness.adapter, 0).spendable(),
        spendable,
        "the inputs went straight back"
    );
    // The release is on disk too, so an unlock doesn't bring back the reservation
    // for a transaction the network already refused.
    let raw = signed.raw.as_bytes();
    if let Some(blob) = stored_sends(&*harness.cursors).await.unwrap() {
        assert!(
            blob.with_bytes(|bytes| !bytes.windows(raw.len()).any(|window| window == raw)),
            "a refused transaction is still reserved on disk"
        );
    }
}

#[tokio::test]
async fn a_refusal_another_node_might_accept_keeps_the_reservation() {
    let harness = Harness::new().await.unwrap();
    harness.node.refuse_with(Some(refusal("not relayed")));

    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("this node declined");
    assert_eq!(error.code(), ErrorCode::Rejected, "{error}");
    assert_eq!(
        in_flight(&harness.adapter),
        1,
        "another node may accept it, so its inputs stay committed"
    );

    harness.node.refuse_with(None);
    assert_eq!(harness.adapter.broadcast(&signed).await.unwrap(), signed.txid);
    assert_eq!(
        harness.node.broadcast_endpoints(),
        vec![Some(ENDPOINT.to_owned()), Some(ENDPOINT.to_owned())]
    );
    assert_eq!(in_flight(&harness.adapter), 1);
}

#[tokio::test]
async fn bytes_that_are_not_one_monero_transaction_never_reach_a_node() {
    let harness = Harness::new().await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let sent = harness.node.published().len();

    let mut trailing = signed.raw.as_bytes().to_vec();
    trailing.push(0);
    let extra = SignedTx { raw: RawTx::from_bytes(trailing), txid: signed.txid.clone() };
    let error = harness.adapter.broadcast(&extra).await.expect_err("trailing bytes");
    assert_eq!(error.code(), ErrorCode::Signing, "{error}");
    assert_eq!(harness.node.published().len(), sent, "nothing went out");
}

#[tokio::test]
async fn a_refusal_that_says_it_did_not_land_releases_the_inputs() {
    let harness = Harness::new().await.unwrap();
    let spendable = account_balance(&harness.adapter, 0).spendable();
    harness.node.refuse_with(Some(refusal("transaction fee is too low")));

    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    let error = harness.adapter.broadcast(&signed).await.expect_err("a fee below the floor");
    assert!(
        matches!(&error, ChainError::Rejected(rejection)
            if rejection.retry == RetryPolicy::DidNotLand),
        "{error}"
    );

    assert_eq!(
        in_flight(&harness.adapter),
        0,
        "the transaction is in no mempool, so nothing may follow it up and resend it"
    );
    assert_eq!(
        account_balance(&harness.adapter, 0).spendable(),
        spendable,
        "the rebuild the refusal asks for needs these inputs back"
    );
}
