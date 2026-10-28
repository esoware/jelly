use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, ErrorCode, FeeLevel,
    FeeSelection, SendAmount, TokenId, TxWarningKind,
    test_util::{FakeClock, FakeEventSink, NullCursorStore, TxRequestBuilder},
};
use jelly_monero::{MoneroConfig, test_util::account_balance};

use crate::common::{
    ChainBuilder, ENDPOINT, FakeNode, Harness, PRIMARY_ID, account, config, fixture_adapter,
    owned_in, payment, recorded, recorded_block, recorded_height, row,
};

#[tokio::test]
async fn a_build_previews_the_transaction_it_will_sign() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();

    assert_eq!(unsigned.chain(), ChainId::Monero);
    assert_eq!(unsigned.preview.amount, Amount::from_base_units(1_000));
    assert_eq!(unsigned.preview.asset, AssetId::Native(ChainId::Monero));
    assert_eq!(unsigned.preview.to, harness.primary);
    assert!(unsigned.preview.fee > Amount::ZERO);
    assert!(
        unsigned.preview.warnings.iter().any(|warning| warning.kind == TxWarningKind::RemoteDecoys),
        "the preview has to say the ring came from a remote node"
    );
    for label in ["Ring size", "Inputs", "Outputs", "Change", "Priority", "Fee rate"] {
        assert!(row(&unsigned.preview.details, label).is_some(), "the preview is missing {label}");
    }
}

#[tokio::test]
async fn every_ring_in_one_transaction_comes_from_one_node() {
    let harness = Harness::new().await.unwrap();
    harness.adapter.build_tx(&harness.request(None)).await.unwrap();
    let endpoints = harness.node.ring_endpoints();
    assert!(!endpoints.is_empty(), "a ring was selected");
    assert!(
        endpoints.iter().all(|endpoint| endpoint == ENDPOINT),
        "one transaction's whole ring must come from one node: {endpoints:?}"
    );
}

#[tokio::test]
async fn a_sweep_spends_everything_it_selected_and_leaves_no_change() {
    let harness = Harness::new().await.unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(None)).await.unwrap();
    let held = u128::from(harness.recorded.output_amount.saturating_mul(2));
    let spent =
        unsigned.preview.amount.base_units().saturating_add(unsigned.preview.fee.base_units());
    assert_eq!(spent, held, "a sweep spends everything it selected");
    let change = row(&unsigned.preview.details, "Change").expect("a change row");
    assert!(change.starts_with("0 "), "{change}");
}

#[tokio::test]
async fn spending_more_than_the_wallet_holds_names_the_shortfall() {
    let harness = Harness::new().await.unwrap();
    let error = harness
        .adapter
        .build_tx(&harness.request(Some(1_000_000_000)))
        .await
        .expect_err("insufficient funds");
    let ChainError::InsufficientFunds { asset, needed, available } = error else {
        panic!("expected a shortfall, got {error}");
    };
    assert_eq!(asset, AssetId::Native(ChainId::Monero));
    assert_eq!(needed, Amount::from_base_units(1_000_000_000));
    assert!(available < needed);
}

#[tokio::test]
async fn a_wallet_with_nothing_to_spend_says_so_rather_than_building() {
    let recorded = recorded().unwrap();
    let adapter = fixture_adapter(
        Arc::new(FakeNode::new(recorded_height(&recorded).unwrap())),
        Arc::new(NullCursorStore),
        Arc::new(FakeClock::default()),
        config(),
    )
    .await
    .unwrap();
    let primary = account(&recorded).unwrap().address(0, 0).unwrap();

    let request = TxRequestBuilder::new(
        PRIMARY_ID,
        &primary,
        AssetId::Native(ChainId::Monero),
        Amount::from_base_units(10),
    )
    .build();
    let error = adapter.build_tx(&request).await.expect_err("nothing to spend");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
}

#[tokio::test]
async fn a_request_this_adapter_cannot_serve_is_refused_by_the_quote_and_the_build() {
    let harness = Harness::new().await.unwrap();

    let mut too_large = harness.request(Some(1));
    too_large.amount = SendAmount::Exact {
        value: Amount::from_base_units(u128::from(u64::MAX).saturating_add(1)),
    };

    let mut unwatched = harness.request(Some(1_000));
    unwatched.from_address_ids = vec![99];

    let mut token = harness.request(Some(1_000));
    token.asset = AssetId::Token(TokenId::UsdtErc20);

    let mut foreign_fee = harness.request(Some(1_000));
    foreign_fee.fee = FeeSelection::Solana { priority_fee_lamports: "3".to_owned() };

    let mut bad_address = harness.request(Some(1_000));
    bad_address.to = "4not_a_real_address_at_all_but_long_enough".to_owned();

    let cases = [
        ("out of range", too_large, ErrorCode::InvalidInput),
        ("not watched", unwatched, ErrorCode::InvalidInput),
        ("no tokens", token, ErrorCode::Unsupported),
        ("another chain's fee", foreign_fee, ErrorCode::InvalidInput),
        ("garbage", bad_address, ErrorCode::InvalidAddress),
    ];
    for (what, request, expected) in cases {
        let built = harness.adapter.build_tx(&request).await.expect_err(what);
        let quoted = harness.adapter.quote_fee(&request).await.expect_err(what);
        assert_eq!(built.code(), expected, "{what}: {built}");
        assert_eq!(quoted.code(), expected, "{what}: {quoted}");
        assert!(!built.to_string().contains("not_a_real_address"), "{built}");
    }
}

#[tokio::test]
async fn one_output_distribution_serves_every_build_inside_its_window() {
    let harness = Harness::new().await.unwrap();
    let before = harness.node.distribution_calls();
    harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    harness.adapter.build_tx(&harness.request(Some(1_100))).await.unwrap();
    assert_eq!(
        harness.node.distribution_calls().saturating_sub(before),
        1,
        "the distribution is tens of megabytes; it is fetched once per window"
    );

    harness.clock.advance(Duration::from_secs(601));
    harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    assert_eq!(harness.node.distribution_calls().saturating_sub(before), 2);
}

#[tokio::test]
async fn a_build_with_every_coin_in_flight_reports_insufficient_funds() {
    let harness = Harness::new().await.unwrap();
    // One send puts both outputs in flight, so a second build has nothing left and
    // says why.
    harness.send(None).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&harness.request(Some(100)))
        .await
        .expect_err("everything is committed to the transaction in flight");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
    assert_eq!(account_balance(&harness.adapter, 0).spendable(), Amount::ZERO);
    assert!(account_balance(&harness.adapter, 0).reserved() > Amount::ZERO);
}

#[tokio::test]
async fn a_payment_only_the_input_cap_prevents_says_split_it_rather_than_short_of_funds() {
    let harness = Harness::with(MoneroConfig { max_inputs: 1, ..config() }).await.unwrap();
    let held = account_balance(&harness.adapter, 0).spendable();
    assert_eq!(held, Amount::from_base_units(20_000), "two outputs of 10_000 each");

    // Two inputs cover 12_000 and the slow priority's fee of a few thousand, but one
    // doesn't.
    let mut request = harness.request(Some(12_000));
    request.fee = FeeSelection::Level { level: FeeLevel::Slow };
    let error = harness.adapter.build_tx(&request).await.expect_err("one input cannot pay it");
    assert!(
        !matches!(error, ChainError::InsufficientFunds { .. }),
        "the wallet holds {held:?} unlocked, which covers the payment: {error}"
    );
    assert_eq!(error.code(), ErrorCode::InvalidInput, "{error}");
    assert!(error.to_string().contains("Split it into smaller payments"), "{error}");
}

/// Outputs on chain before the block that pays this wallet.
const EARLIER_OUTPUTS: u64 = 1_000;

/// A build may reuse an earlier build's distribution while inside the distribution window.
///
/// It can't once the scan's newer tip has unlocked an input that was still locked at the
/// distribution's height. A ring chosen there would hold a real input newer than every decoy
/// it could draw, which marks it as the real spend to anyone reading the chain.
#[tokio::test]
async fn a_ring_is_chosen_against_a_chain_on_which_its_real_input_is_unlocked() {
    let recorded = recorded().unwrap();
    let height = recorded_height(&recorded).unwrap();
    let mut paid = recorded_block(&recorded).unwrap();
    paid.output_index_for_first_ringct_output = Some(EARLIER_OUTPUTS);
    let chain = |tip: u64| {
        ChainBuilder::new([0; 32])
            .recorded(paid.clone())
            .run(height.saturating_add(1), tip.saturating_sub(height), 1)
            .finish()
            .unwrap()
    };

    // Two blocks short of the ten-block lock.
    let locked_tip = height.saturating_add(7);
    let fake = Arc::new(FakeNode::new(locked_tip));
    fake.serve(chain(locked_tip));
    for output in owned_in(&recorded, paid.clone(), &[]).unwrap() {
        fake.register_real(&output);
    }
    // Every `RingCT` output before the paying block sits in block zero, and the two this
    // wallet owns are the only ones in the paying block.
    fake.distribute(|block| {
        if block < height { EARLIER_OUTPUTS } else { EARLIER_OUTPUTS.saturating_add(2) }
    });
    let clock = Arc::new(FakeClock::default());
    let adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&fake),
        Arc::new(NullCursorStore),
        Arc::<FakeClock>::clone(&clock),
        config(),
    )
    .await
    .unwrap();
    let events = FakeEventSink::new();
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();

    // The person tries to pay while the coins are still locked, which is refused.
    let request = payment(&account(&recorded).unwrap().address(0, 0).unwrap(), 1_000);
    let refused = adapter.build_tx(&request).await;
    assert!(
        matches!(refused, Err(ChainError::InsufficientFunds { .. })),
        "the coins are still locked: {refused:?}"
    );

    // A few blocks later the coins have unlocked, and the person tries again.
    let unlocked_tip = height.saturating_add(15);
    fake.serve(chain(unlocked_tip));
    fake.set_tip(unlocked_tip);
    adapter.sync(clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(account_balance(&adapter, 0).spendable() > Amount::ZERO, "the coins unlocked");

    let built = adapter.build_tx(&request).await.expect("the unlocked coins pay");
    let chosen_at: u64 = row(&built.preview.details, "Decoys chosen at height")
        .expect("the preview names the ring height")
        .parse()
        .expect("a height");
    let spendable_from = height.saturating_add(9);
    assert!(
        chosen_at >= spendable_from,
        "expected the ring to be chosen at height {spendable_from} or later, where its real \
         input from block {height} is unlocked, but it was chosen at height {chosen_at}"
    );
}

/// A distribution whose node stopped answering ring requests leaves the cache, so a
/// retry fetches a fresh one instead of failing the same way until the window closes.
#[tokio::test]
async fn a_retry_after_the_ring_node_dropped_chooses_its_ring_from_a_node_that_answers() {
    let harness = Harness::new().await.unwrap();
    harness.node.time_out_rings_until_distribution(2);
    let request = harness.request(Some(1_000));

    let first = harness.adapter.build_tx(&request).await;
    assert!(first.is_err(), "the node that served the distribution timed out on the ring");

    // The person tries again a moment later.
    harness.clock.advance(Duration::from_secs(5));
    let retried = harness.adapter.build_tx(&request).await;
    assert!(
        retried.is_ok(),
        "expected a retry to choose its ring from a node that answers, but it failed again \
         after {} distribution fetch(es): {:?}",
        harness.node.distribution_calls(),
        retried.err()
    );
}
