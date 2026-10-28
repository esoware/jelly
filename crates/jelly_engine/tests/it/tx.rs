use std::slice::from_ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainError, ChainId, Clock as _, CoreError, FeeSelection, PendingTxId,
    RejectionKind, RetryPolicy, SendAmount, TxDirection, TxId, TxOutcome, TxRecord, TxRequest,
    TxWarningKind, WalletEvent,
};
use jelly_engine::{PreviewDigest, TxQuery, test_util};

use crate::common::{
    Fallible, Harness, OTHER_PASSWORD, PASSWORD, detected_so_far, harness, next_announced, secret,
};

const RECIPIENT: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
const CHAIN: ChainId = ChainId::Bitcoin;

/// Just past the time an authorisation stays submittable.
const PAST_EXPIRY: Duration = test_util::PENDING_TTL.saturating_add(Duration::from_secs(1));

/// A wallet with one address on a chain that has an adapter, and the address's id.
///
/// The address is added only once the unlock's first pass has finished. A pass still
/// running could otherwise hand the fake the watch set between two builds and change
/// the sender its previews name.
async fn ready() -> Fallible<(Harness, i64)> {
    let harness = harness().await?;
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("spender").await?;
    test_util::wait_for_pass(&harness.engine, CHAIN, 0).await;
    let added = harness.engine.add_address(CHAIN, None, None).await?;
    watched(&harness).await?;
    Ok((harness, added.id))
}

/// Runs a pass, which hands the adapter every address the wallet now holds, so a
/// preview names them.
async fn watched(harness: &Harness) -> Fallible<()> {
    let passes = test_util::sync_passes(&harness.engine, CHAIN);
    harness.engine.sync_now(Some(CHAIN)).await?;
    test_util::wait_for_pass(&harness.engine, CHAIN, passes).await;
    Ok(())
}

/// A utxo-shaped fee selection, which is what the fake adapter reads.
fn rate(sat_per_vb: &str) -> FeeSelection {
    FeeSelection::Rate { rate: String::from(sat_per_vb) }
}

fn request(from: i64) -> TxRequest {
    TxRequest {
        from_address_ids: vec![from],
        to: RECIPIENT.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset: AssetId::Native(CHAIN),
        amount: SendAmount::Exact { value: Amount::from_base_units(10_000) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}

#[tokio::test]
async fn a_prepared_transaction_is_signed_broadcast_and_recorded() {
    let (harness, from) = ready().await.unwrap();
    let quote = harness.engine.quote_fees(request(from)).await.unwrap();
    assert_eq!(quote.fee, Amount::from_base_units(1_000));

    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    assert_eq!(prepared.preview.to, RECIPIENT);
    assert_eq!(test_util::list_pending_txs(&harness.engine).await.unwrap().len(), 1);

    let mut events = harness.engine.subscribe();
    let txid = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();
    assert_eq!(
        detected_so_far(&mut events).len(),
        1,
        "the recorded transaction is announced once it is durable"
    );
    let state = harness.builder.state(CHAIN);
    assert_eq!(state.broadcast_count(), 1);
    assert_eq!(state.discards(), 0, "a sent transaction keeps its reservation");
    assert!(test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty());

    let page = harness.engine.list_txs(TxQuery { limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows.first().map(|row| row.txid.clone()), Some(txid.value().to_owned()));
    assert_eq!(
        page.rows.first().and_then(|row| row.counterparty.clone()).as_deref(),
        Some(RECIPIENT)
    );
}

#[tokio::test]
async fn a_sent_transaction_is_recorded_with_the_rent_its_review_showed() {
    let (harness, from) = ready().await.unwrap();
    harness.builder.state(CHAIN).charge_rent(Amount::from_base_units(2_039_280));
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    assert_eq!(prepared.preview.rent, Some(Amount::from_base_units(2_039_280)));
    harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await.unwrap();

    let page = harness.engine.list_txs(TxQuery { limit: 10, ..Default::default() }).await.unwrap();
    let row = page.rows.first().expect("the sent row");
    assert_eq!(row.fee, Some(Amount::from_base_units(1_000)));
    assert_eq!(row.rent, Some(Amount::from_base_units(2_039_280)));
}

#[tokio::test]
async fn unsupported_payment_fields_are_not_silently_ignored_on_other_chains() {
    let (harness, from) = ready().await.unwrap();
    for (request, wanted) in [
        (TxRequest { destination_tag: Some(0), ..request(from) }, "destinationTag"),
        (TxRequest { comment: Some(String::from("order 123")), ..request(from) }, "comment"),
        (TxRequest { nonce: Some(7), ..request(from) }, "nonce"),
        (TxRequest { data: Some(String::from("0xd0e30db0")), ..request(from) }, "data"),
    ] {
        for error in [
            harness.engine.prepare_tx(request.clone()).await.unwrap_err(),
            harness.engine.quote_fees(request).await.unwrap_err(),
        ] {
            assert!(matches!(
                error,
                CoreError::InvalidInput { field, .. } if field == wanted
            ));
        }
    }
}

#[tokio::test]
async fn an_account_deletion_is_refused_on_a_chain_without_one() {
    let (harness, from) = ready().await.unwrap();
    let mut request = request(from);
    request.should_delete_account = true;
    assert!(matches!(
        harness.engine.prepare_tx(request.clone()).await.unwrap_err(),
        CoreError::InvalidInput { field: "shouldDeleteAccount", .. }
    ));
    assert!(matches!(
        harness.engine.quote_fees(request).await.unwrap_err(),
        CoreError::InvalidInput { field: "shouldDeleteAccount", .. }
    ));
}

#[tokio::test]
async fn a_payment_uri_reaches_the_adapter_as_its_bare_address() {
    let (harness, from) = ready().await.unwrap();
    let to = format!("bitcoin:{}?amount=0.0001&label=Shop", RECIPIENT.to_uppercase());
    let prepared = harness.engine.prepare_tx(TxRequest { to, ..request(from) }).await.unwrap();
    assert_eq!(prepared.preview.to, RECIPIENT);
}

#[tokio::test]
async fn a_guessed_or_spent_token_is_refused() {
    let (harness, from) = ready().await.unwrap();
    let guessed = PendingTxId::from_bytes([0u8; PendingTxId::LEN]);
    let forged = PreviewDigest::parse(&"00".repeat(PreviewDigest::LEN)).unwrap();
    let refused = harness.engine.submit_tx(guessed, secret(PASSWORD), forged).await;
    assert!(matches!(refused, Err(CoreError::UnknownPendingTx(_))), "{refused:?}");

    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let digest = prepared.preview_digest;
    harness.engine.submit_tx(prepared.id, secret(PASSWORD), digest).await.unwrap();
    let again = harness.engine.submit_tx(prepared.id, secret(PASSWORD), digest).await;
    assert!(matches!(again, Err(CoreError::UnknownPendingTx(_))), "{again:?}");
    assert_eq!(harness.builder.state(CHAIN).broadcast_count(), 1);
}

#[tokio::test]
async fn a_wrong_password_leaves_the_token_unspent() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let digest = prepared.preview_digest;

    let refused = harness.engine.submit_tx(prepared.id, secret(OTHER_PASSWORD), digest).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    assert_eq!(harness.builder.state(CHAIN).broadcast_count(), 0);
    assert_eq!(test_util::list_pending_txs(&harness.engine).await.unwrap().len(), 1);

    harness.engine.submit_tx(prepared.id, secret(PASSWORD), digest).await.unwrap();
}

#[tokio::test]
async fn a_preview_the_user_did_not_see_needs_a_rebuild() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();

    for byte in ["00", "ff"] {
        let wrong = PreviewDigest::parse(&byte.repeat(PreviewDigest::LEN)).unwrap();
        let refused = harness.engine.submit_tx(prepared.id, secret(PASSWORD), wrong).await;
        assert!(matches!(refused, Err(CoreError::RebuildRequired)), "{wrong}: {refused:?}");
    }
    assert_eq!(harness.builder.state(CHAIN).broadcast_count(), 0);
    assert_eq!(test_util::list_pending_txs(&harness.engine).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_same_request_previews_to_the_same_digest() {
    let (harness, from) = ready().await.unwrap();
    let first = harness.engine.prepare_tx(request(from)).await.unwrap();
    let second = harness.engine.prepare_tx(request(from)).await.unwrap();
    assert_eq!(first.preview_digest, second.preview_digest);
    assert_ne!(first.id, second.id);

    let mut other = request(from);
    other.amount = SendAmount::Exact { value: Amount::from_base_units(10_001) };
    let third = harness.engine.prepare_tx(other).await.unwrap();
    assert_ne!(first.preview_digest, third.preview_digest);
}

/// The fake's signer refuses any preview but the one it built, so the warning the engine
/// adds cannot reach it.
#[tokio::test]
async fn a_fee_above_the_chains_limit_is_warned_about_and_still_signs() {
    let (harness, from) = ready().await.unwrap();
    let limit = jelly_chains::chains().spec(CHAIN).high_fee;
    harness.builder.state(CHAIN).charge(limit.checked_add(Amount::from_base_units(1)).unwrap());
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let kinds: Vec<TxWarningKind> =
        prepared.preview.warnings.iter().map(|warning| warning.kind).collect();
    assert_eq!(kinds, [TxWarningKind::HighFee]);
    harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await.unwrap();
}

#[tokio::test]
async fn locking_clears_every_authorisation() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    let created = harness.seed_wallet("spender").await.unwrap();
    let from = harness.engine.add_address(CHAIN, None, None).await.unwrap().id;
    watched(&harness).await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let digest = prepared.preview_digest;
    harness.engine.lock_wallet().await;

    let refused = test_util::list_pending_txs(&harness.engine).await;
    assert!(matches!(refused, Err(CoreError::Locked)), "{refused:?}");

    harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert!(test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty());

    let refused = harness.engine.submit_tx(prepared.id, secret(PASSWORD), digest).await;
    assert!(matches!(refused, Err(CoreError::UnknownPendingTx(_))), "{refused:?}");
}

#[tokio::test]
async fn discarding_an_authorisation_releases_its_reservation() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    harness.engine.delete_pending_tx(prepared.id).await.unwrap();
    assert!(test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty());
    assert_eq!(harness.builder.state(CHAIN).discards(), 1);

    let refused = harness.engine.delete_pending_tx(prepared.id).await;
    assert!(matches!(refused, Err(CoreError::UnknownPendingTx(_))), "{refused:?}");
}

#[tokio::test]
async fn an_expired_authorisation_releases_its_reservation() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    harness.clock.advance(PAST_EXPIRY);
    assert!(
        test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty(),
        "the authorisation should have expired"
    );
    assert_eq!(harness.builder.state(CHAIN).discards(), 1);

    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::RebuildRequired)), "{refused:?}");
}

#[tokio::test]
async fn an_expired_authorisation_is_refused_before_anything_evicts_it() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    harness.clock.advance(PAST_EXPIRY);
    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::RebuildRequired)), "{refused:?}");
    let state = harness.builder.state(CHAIN);
    assert_eq!(state.broadcast_count(), 0);
    assert_eq!(state.discards(), 1);
}

#[tokio::test]
async fn an_authorisation_that_expires_while_signing_is_never_broadcast() {
    let (harness, from) = ready().await.unwrap();
    let state = harness.builder.state(CHAIN);
    state.sign_slowly(Arc::clone(&harness.clock), PAST_EXPIRY);

    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::RebuildRequired)), "{refused:?}");
    assert_eq!(state.signs.load(Ordering::SeqCst), 1, "it expired during signing, not before");
    assert_eq!(state.broadcast_count(), 0);
    assert_eq!(state.discards(), 1);
}

#[tokio::test]
async fn a_definitive_refusal_releases_and_an_ambiguous_one_does_not() {
    let (harness, from) = ready().await.unwrap();
    let state = harness.builder.state(CHAIN);

    state.refuse(RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand);
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::Chain(_))), "{refused:?}");
    assert_eq!(state.discards(), 1);

    state.refuse(RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint);
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::Chain(_))), "{refused:?}");
    assert_eq!(state.discards(), 1, "an ambiguous refusal keeps the reservation");
}

#[tokio::test]
async fn a_store_failure_after_a_broadcast_is_not_an_error() {
    let (harness, from) = ready().await.unwrap();
    harness.builder.state(CHAIN).break_record_after_broadcast(harness.engine.clone(), from);
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let mut events = harness.engine.subscribe();

    harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await.unwrap();
    assert_eq!(harness.builder.state(CHAIN).broadcast_count(), 1);
    // Any announcement is published before `submit_tx` returns, so an empty drain
    // here is the whole assertion.
    assert!(detected_so_far(&mut events).is_empty(), "a row that was never written was announced");
}

/// The clock is read for the sent record while a failure still leaves the authorisation to
/// retry, because nothing after the broadcast may fail the send.
#[tokio::test]
async fn a_clock_failure_after_a_broadcast_still_reports_the_payment_as_sent() {
    let (harness, from) = ready().await.unwrap();
    harness.builder.state(CHAIN).break_clock_on_broadcast(Arc::clone(&harness.clock));
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let mut events = harness.engine.subscribe();

    let outcome =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(
        outcome.is_ok(),
        "the network accepted the payment, so submit must answer with its txid, but it \
         answered {outcome:?}"
    );
    assert!(detected_so_far(&mut events).is_empty(), "a row that was never written was announced");
}

#[tokio::test]
async fn a_build_that_previews_another_chain_is_refused() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let sent = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();

    harness.builder.state(CHAIN).preview_chain(ChainId::Solana);
    let refused = harness.engine.prepare_tx(request(from)).await;
    assert!(matches!(refused, Err(CoreError::Internal { .. })), "{refused:?}");

    let refused = harness.engine.bump_fee(&sent, &rate("20")).await;
    assert!(matches!(refused, Err(CoreError::Internal { .. })), "{refused:?}");

    assert!(
        test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty(),
        "nothing was authorised"
    );
}

#[tokio::test]
async fn an_asset_from_another_chain_is_refused() {
    let (harness, from) = ready().await.unwrap();
    let mut wrong = request(from);
    wrong.asset = AssetId::Native(ChainId::Solana);
    let refused = harness.engine.prepare_tx(wrong).await;
    assert!(matches!(refused, Err(CoreError::InvalidInput { field: "asset", .. })), "{refused:?}");
}

#[tokio::test]
async fn a_send_from_a_chain_with_no_adapter_is_refused() {
    let (harness, from) = ready().await.unwrap();
    harness.engine.set_chain_enabled(CHAIN, false).await.unwrap();
    let refused = harness.engine.prepare_tx(request(from)).await;
    assert!(matches!(refused, Err(CoreError::NoAdapter { reason: None, .. })), "{refused:?}");
}

#[tokio::test]
async fn a_bump_below_the_minimum_is_refused_and_the_normal_quote_is_the_minimum() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let txid = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();

    // Ten sat/vB is 2500 sat/kwu.
    harness.builder.state(CHAIN).bump_floor_sat_per_kwu.store(2_500, Ordering::SeqCst);
    let quote = harness.engine.quote_bump(&txid, &FeeSelection::NORMAL).await.unwrap();
    assert_eq!(
        quote.custom,
        Some(FeeSelection::Rate { rate: String::from("10") }),
        "the normal replacement is the floor itself"
    );

    let refused = harness.engine.bump_fee(&txid, &rate("5")).await;
    assert!(matches!(refused, Err(CoreError::Chain(ChainError::Rejected(_)))), "{refused:?}");

    let planned = harness.engine.bump_fee(&txid, &rate("20")).await.unwrap();
    assert!(planned.preview.to.starts_with("replacement-of-"), "{}", planned.preview.to);
    assert_eq!(test_util::list_pending_txs(&harness.engine).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_bump_of_an_unknown_transaction_is_not_found() {
    let (harness, _from) = ready().await.unwrap();
    let unknown = TxId::new(CHAIN, "deadbeef");
    let refused = harness.engine.bump_fee(&unknown, &rate("20")).await;
    assert!(matches!(refused, Err(CoreError::NotFound { .. })), "{refused:?}");

    let elsewhere = TxId::new(ChainId::Monero, "deadbeef");
    let refused = harness.engine.quote_bump(&elsewhere, &FeeSelection::NORMAL).await;
    assert!(
        matches!(refused, Err(CoreError::NoAdapter { chain: ChainId::Monero, .. })),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_cancel_is_quoted_planned_and_signed_like_a_bump() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let sent = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();

    let quote = harness.engine.quote_cancel(&sent, &FeeSelection::NORMAL).await.unwrap();
    assert_eq!(quote.fee, Amount::from_base_units(1_000));
    assert!(
        test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty(),
        "a quote reserves nothing"
    );

    let cancel = harness.engine.cancel_tx(&sent, &FeeSelection::NORMAL).await.unwrap();
    assert_eq!(cancel.preview.amount, Amount::ZERO);
    assert_eq!(
        cancel.preview.from.as_slice(),
        from_ref(&cancel.preview.to),
        "a cancel pays its own sender"
    );
    let cancelled =
        harness.engine.submit_tx(cancel.id, secret(PASSWORD), cancel.preview_digest).await.unwrap();

    let query = TxQuery { txid: Some(cancelled.value().to_owned()), ..Default::default() };
    let rows = harness.engine.list_txs(query).await.unwrap().rows;
    let row = rows.first().expect("the cancel is recorded");
    assert_eq!(row.direction, TxDirection::SelfTransfer, "the cancel is a send to itself");
    assert_eq!(row.amount, Amount::ZERO);
    assert_eq!(row.sender_ids, vec![from]);
}

#[tokio::test]
async fn a_cancel_of_an_unknown_transaction_is_not_found() {
    let (harness, _from) = ready().await.unwrap();
    let unknown = TxId::new(CHAIN, "deadbeef");
    let refused = harness.engine.cancel_tx(&unknown, &FeeSelection::NORMAL).await;
    assert!(matches!(refused, Err(CoreError::NotFound { .. })), "{refused:?}");

    let elsewhere = TxId::new(ChainId::Monero, "deadbeef");
    let refused = harness.engine.quote_cancel(&elsewhere, &FeeSelection::NORMAL).await;
    assert!(
        matches!(refused, Err(CoreError::NoAdapter { chain: ChainId::Monero, .. })),
        "{refused:?}"
    );
}

/// A replaced send never ran, so it moved nothing and paid nothing, and the
/// history says it was replaced rather than that it failed.
#[tokio::test]
async fn a_send_another_transaction_replaced_is_listed_as_replaced_with_nothing_paid() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    let sent = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();

    let mut events = harness.engine.subscribe();
    let replaced = TxRecord::builder(
        AssetId::Native(CHAIN),
        from,
        sent.clone(),
        TxDirection::Outgoing,
        Amount::ZERO,
    )
    .outcome(TxOutcome::Replaced)
    .fee(Amount::ZERO)
    .build();
    test_util::event_sink(&harness.engine).emit(WalletEvent::TxDetected(replaced)).await;
    assert_eq!(next_announced(&mut events).await.unwrap(), sent.value());

    for query in [
        TxQuery { txid: Some(sent.value().to_owned()), ..Default::default() },
        TxQuery {
            txid: Some(sent.value().to_owned()),
            address_id: Some(from),
            ..Default::default()
        },
    ] {
        let rows = harness.engine.list_txs(query).await.unwrap().rows;
        let row = rows.first().expect("the send is listed");
        assert_eq!(row.outcome, TxOutcome::Replaced);
        assert_eq!(row.amount, Amount::ZERO);
        assert_eq!(row.fee, Some(Amount::ZERO), "the fee the send was previewed at was never paid");
    }
}

#[tokio::test]
async fn a_zero_limit_means_the_default_page() {
    let (harness, from) = ready().await.unwrap();
    for _ in 0..3 {
        let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
        harness
            .engine
            .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
            .await
            .unwrap();
    }
    let page = harness.engine.list_txs(TxQuery::default()).await.unwrap();
    assert_eq!(page.rows.len(), 3);
}

#[tokio::test]
async fn a_page_token_this_build_did_not_issue_is_refused() {
    let (harness, _from) = ready().await.unwrap();
    let query = TxQuery { cursor: Some(String::from("not-a-token")), ..Default::default() };
    let refused = harness.engine.list_txs(query).await;
    assert!(matches!(refused, Err(CoreError::InvalidInput { field: "cursor", .. })), "{refused:?}");
}

#[tokio::test]
async fn a_rebuild_evicts_the_authorisations_of_the_adapter_it_replaces() {
    let (harness, from) = ready().await.unwrap();
    let prepared = harness.engine.prepare_tx(request(from)).await.unwrap();
    assert!(harness.builder.weak(CHAIN).upgrade().is_some(), "the fixture never built an adapter");

    test_util::rebuild_adapters(&harness.engine).await.unwrap();
    assert!(
        test_util::list_pending_txs(&harness.engine).await.unwrap().is_empty(),
        "an authorisation outlived the adapter holding its reservation"
    );
    let refused =
        harness.engine.submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest).await;
    assert!(matches!(refused, Err(CoreError::RebuildRequired)), "{refused:?}");
    assert_eq!(harness.builder.state(CHAIN).broadcast_count(), 0);
}

#[tokio::test]
async fn a_bump_of_a_send_between_own_addresses_signs_with_the_payer() {
    let (harness, from) = ready().await.unwrap();
    let own = harness.engine.add_address(CHAIN, None, None).await.unwrap();
    watched(&harness).await.unwrap();
    let to_own = TxRequest { to: own.address.clone(), ..request(from) };
    let prepared = harness.engine.prepare_tx(to_own).await.unwrap();
    let sent = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();

    // The next pass finds the same transaction arriving at the receiving address.
    let mut events = harness.engine.subscribe();
    let arrival = TxRecord::builder(
        AssetId::Native(CHAIN),
        own.id,
        sent.clone(),
        TxDirection::Incoming,
        Amount::from_base_units(10_000),
    )
    .seen_at(harness.clock.now_secs().unwrap())
    .build();
    test_util::event_sink(&harness.engine).emit(WalletEvent::TxDetected(arrival)).await;
    assert_eq!(next_announced(&mut events).await.unwrap(), sent.value());

    let bump = harness.engine.bump_fee(&sent, &rate("20")).await.unwrap();
    let replaced =
        harness.engine.submit_tx(bump.id, secret(PASSWORD), bump.preview_digest).await.unwrap();
    let query = TxQuery { txid: Some(replaced.value().to_owned()), ..Default::default() };
    let rows = harness.engine.list_txs(query).await.unwrap().rows;
    assert_eq!(
        rows.iter().flat_map(|row| row.sender_ids.clone()).collect::<Vec<_>>(),
        vec![from],
        "the replacement belongs to the address that paid for the original"
    );
}
