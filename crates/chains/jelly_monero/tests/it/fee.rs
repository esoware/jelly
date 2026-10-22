use std::time::Duration;

use jelly_core::{Amount, ChainAdapter as _, FeeLevel, FeeSelection, TxRequest};
use jelly_monero::test_util::estimated_fee;
use monero_interface::FeeRate;

use crate::common::{Harness, RATES, row};

fn at(mut request: TxRequest, level: FeeLevel) -> TxRequest {
    request.fee = FeeSelection::Level { level };
    request
}

/// The fake node cannot say how full the chain is, so the normal level pays the
/// normal tier and each level pays the tier at its own position.
fn rate(level: FeeLevel) -> Option<FeeRate> {
    let index = FeeLevel::ALL.iter().position(|each| *each == level)?;
    FeeRate::new(*RATES.get(index)?, 1)
}

#[tokio::test]
async fn a_normal_quote_prices_the_tier_the_node_settled_on_and_names_its_rate() {
    let harness = Harness::new().await.unwrap();
    let quote = harness
        .adapter
        .quote_fee(&at(harness.request(Some(1_000)), FeeLevel::Normal))
        .await
        .unwrap();
    assert_eq!(
        quote.custom,
        Some(FeeSelection::Rate { rate: String::from("4") }),
        "a custom fee starts from the rate the level paid"
    );
    assert_eq!(quote.fee, estimated_fee(rate(FeeLevel::Normal).unwrap(), 1));
}

#[tokio::test]
async fn each_level_is_priced_on_its_own_and_faster_ones_cost_more() {
    let harness = Harness::new().await.unwrap();
    let mut fees = Vec::new();
    for level in FeeLevel::ALL {
        let quote =
            harness.adapter.quote_fee(&at(harness.request(Some(1_000)), level)).await.unwrap();
        assert_eq!(
            quote.levels.iter().find(|row| row.level == level).map(|row| row.fee),
            Some(quote.fee),
            "{level}"
        );
        fees.push(quote.fee);
    }
    assert!(fees.windows(2).all(|pair| pair.first() < pair.get(1)), "{fees:?}");
}

#[tokio::test]
async fn every_level_comes_from_one_round_trip_until_the_window_closes() {
    let harness = Harness::new().await.unwrap();
    let before = harness.node.fee_calls();
    for level in FeeLevel::ALL {
        harness.adapter.quote_fee(&at(harness.request(Some(1_000)), level)).await.unwrap();
    }
    harness.adapter.quote_fee(&harness.request(Some(2_000))).await.unwrap();
    assert_eq!(
        harness.node.fee_calls().saturating_sub(before),
        1,
        "every level comes from one answer, reused inside its window"
    );

    harness.clock.advance(Duration::from_secs(121));
    harness.adapter.quote_fee(&harness.request(Some(1_000))).await.unwrap();
    assert_eq!(harness.node.fee_calls().saturating_sub(before), 2, "the window closed");
}

#[tokio::test]
async fn a_quote_counts_the_inputs_the_build_will_actually_spend() {
    let harness = Harness::new().await.unwrap();
    let request = harness.request(Some(1_000));
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    let preview = harness.adapter.build_tx(&request).await.unwrap().preview;

    assert_eq!(harness.outputs.len(), 2, "the wallet holds more than this payment needs");
    assert_eq!(row(&preview.details, "Inputs"), Some("1"));
    assert_eq!(quote.fee, estimated_fee(rate(FeeLevel::Normal).unwrap(), 1));
}

#[tokio::test]
async fn a_sweep_is_quoted_against_every_unlocked_output() {
    let harness = Harness::new().await.unwrap();
    let request = harness.request(None);
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    let preview = harness.adapter.build_tx(&request).await.unwrap().preview;

    assert_eq!(row(&preview.details, "Inputs"), Some("2"));
    assert_eq!(quote.fee, estimated_fee(rate(FeeLevel::Normal).unwrap(), 2));
}

#[tokio::test]
async fn a_sweep_quote_names_what_is_left_after_its_fee() {
    let harness = Harness::new().await.unwrap();
    let quote = harness.adapter.quote_fee(&harness.request(None)).await.unwrap();
    let held = Amount::from_base_units(
        harness
            .outputs
            .iter()
            .fold(0u128, |sum, output| sum.saturating_add(u128::from(output.commitment().amount))),
    );
    assert_eq!(quote.max_amount, held.checked_sub(quote.fee));

    let fixed = harness.adapter.quote_fee(&harness.request(Some(1_000))).await.unwrap();
    assert_eq!(fixed.max_amount, None);
}

#[tokio::test]
async fn a_custom_rate_is_quoted_and_built_as_given_and_refused_below_the_slow_rate() {
    let harness = Harness::new().await.unwrap();
    let mut request = harness.request(Some(1_000));
    request.fee = FeeSelection::Rate { rate: String::from("3") };
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.custom, Some(request.fee.clone()));
    assert_eq!(quote.fee, estimated_fee(FeeRate::new(3, 1).unwrap(), 1));
    assert_eq!(quote.levels.len(), FeeLevel::ALL.len(), "the levels are priced beside it");

    let preview = harness.adapter.build_tx(&request).await.unwrap().preview;
    assert_eq!(row(&preview.details, "Fee rate"), Some("3 piconero/B"));
    assert_eq!(row(&preview.details, "Priority"), None, "a custom rate pays no priority");

    request.fee = FeeSelection::Rate { rate: String::from("0") };
    let error = harness.adapter.quote_fee(&request).await.expect_err("below the slow rate");
    assert_eq!(
        error.to_string(),
        "The fee rate of 0 piconero/B is below the network minimum of 1 piconero/B."
    );
}

#[tokio::test]
async fn a_level_too_dear_for_one_coin_is_quoted_with_two() {
    let harness = Harness::new().await.unwrap();
    // A one-input transaction weighs over a thousand bytes, so the fast level costs
    // more than one of this wallet's two outputs holds.
    let one_output = Amount::from(harness.recorded.output_amount);
    let quote_at = async |level| {
        harness.adapter.quote_fee(&at(harness.request(Some(1_000)), level)).await.unwrap().fee
    };
    let cheap = quote_at(FeeLevel::Slow).await;
    let dear = quote_at(FeeLevel::Fast).await;
    assert!(cheap < one_output, "one output covers the slow level");
    assert!(dear > one_output, "and does not cover the fast one");
    assert!(
        dear > estimated_fee(rate(FeeLevel::Fast).unwrap(), 1),
        "so the fast level was quoted for two inputs"
    );
}
