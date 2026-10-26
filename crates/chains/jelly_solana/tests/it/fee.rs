use jelly_core::{
    Amount, ChainAdapter as _, ChainError, FeeLevel, FeeSelection, TokenId, TxWarningKind,
};
use jelly_solana::{Method, SolanaConfig};
use serde_json::json;

use crate::common::{
    self, Answer, Harness, PEER, detail, native, request, send_max, stock_usdc,
    token_account_address, usdc, with_chosen_fee,
};

/// The normal priority fee of a native transfer in the priced harness.
///
/// A transfer simulated at 450 units runs under a 540 unit limit, and the recorded
/// mainnet sample reads below the floor price of 300000 micro-lamports per unit.
const NORMAL_PRIORITY_LAMPORTS: u64 = 162;

#[tokio::test]
async fn a_level_names_its_priority_fee_and_a_build_charges_the_same() {
    let harness = Harness::priced().await.unwrap();
    let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000_000)).await.unwrap();
    assert_eq!(
        quote.custom,
        Some(FeeSelection::Solana { priority_fee_lamports: NORMAL_PRIORITY_LAMPORTS.to_string() })
    );
    assert_eq!(quote.fee, Amount::from(5_162u64));

    let normal = harness.adapter.build_tx(&request(PEER, native(), 1_000_000)).await.unwrap();
    assert_eq!(normal.preview.fee, quote.fee);

    let mut chosen = request(PEER, native(), 1_000_000);
    chosen.fee = quote.custom.clone().expect("a priority fee");
    let custom = harness.adapter.build_tx(&chosen).await.unwrap();
    assert_eq!(custom.preview.fee, quote.fee, "the custom fee reproduces the level's");
}

/// The recorded sample reads below the floor at every percentile, so every level
/// pays the floor.
#[tokio::test]
async fn a_quote_prices_every_level_over_the_same_compute_units() {
    let harness = Harness::priced().await.unwrap();
    let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000_000)).await.unwrap();
    let levels: Vec<FeeLevel> = quote.levels.iter().map(|row| row.level).collect();
    assert_eq!(levels, FeeLevel::ALL);
    assert!(quote.levels.iter().all(|row| row.fee == quote.fee), "{:?}", quote.levels);

    harness.node.always(
        Method::GetRecentPrioritizationFees,
        Answer::ok(&json!([
            { "slot": 1, "prioritizationFee": 400_000 },
            { "slot": 2, "prioritizationFee": 1_000_000 },
            { "slot": 3, "prioritizationFee": 2_000_000 },
            { "slot": 4, "prioritizationFee": 3_000_000 },
        ])),
    );
    let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000_000)).await.unwrap();
    // Each price over the 540 unit limit, in micro-lamports per unit.
    let fees: Vec<Amount> = quote.levels.iter().map(|row| row.fee).collect();
    assert_eq!(
        fees,
        [540u64, 1_080, 1_620].map(|priority| Amount::from(common::BASE_FEE_LAMPORTS + priority))
    );
    for row in &quote.levels {
        let mut at_level = request(PEER, native(), 1_000_000);
        at_level.fee = FeeSelection::Level { level: row.level };
        let unsigned = harness.adapter.build_tx(&at_level).await.unwrap();
        assert_eq!(unsigned.preview.fee, row.fee, "{}", row.level);
    }
}

#[tokio::test]
async fn a_send_max_quote_names_what_the_build_moves() {
    let harness = Harness::priced().await.unwrap();
    let quote = harness.adapter.quote_fee(&send_max(PEER, native())).await.unwrap();
    let unsigned = harness.adapter.build_tx(&send_max(PEER, native())).await.unwrap();
    assert_eq!(quote.max_amount, Some(unsigned.preview.amount));

    let fixed = harness.adapter.quote_fee(&request(PEER, native(), 1_000)).await.unwrap();
    assert_eq!(fixed.max_amount, None);
}

#[tokio::test]
async fn a_token_send_max_quote_names_the_whole_spendable_balance() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, false).unwrap();
    let quote = harness.adapter.quote_fee(&send_max(PEER, usdc())).await.unwrap();
    assert_eq!(quote.max_amount, Some(Amount::from_base_units(50_000_000)));
}

#[tokio::test]
async fn a_chosen_fee_charges_exactly_what_was_chosen() {
    let harness = Harness::priced().await.unwrap();
    let quote =
        harness.adapter.quote_fee(&with_chosen_fee(PEER, native(), 1_000, 7)).await.unwrap();
    assert_eq!(quote.fee, Amount::from(common::BASE_FEE_LAMPORTS + 7));
    assert!(quote.levels.iter().all(|row| row.fee != quote.fee), "{:?}", quote.levels);

    let unsigned =
        harness.adapter.build_tx(&with_chosen_fee(PEER, native(), 1_000, 0)).await.unwrap();
    assert_eq!(
        unsigned.preview.fee,
        Amount::from(common::BASE_FEE_LAMPORTS),
        "a priority fee of nothing is a choice too"
    );
}

#[tokio::test]
async fn a_sample_that_could_not_be_read_prices_at_the_floor() {
    let harness = Harness::priced().await.unwrap();
    harness
        .node
        .always(Method::GetRecentPrioritizationFees, Answer::refused(-32602, "invalid params"));
    let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000)).await.unwrap();
    assert_eq!(quote.fee, Amount::from(common::BASE_FEE_LAMPORTS + NORMAL_PRIORITY_LAMPORTS));
}

#[tokio::test]
async fn a_sample_above_the_ceiling_is_capped_and_the_preview_says_so() {
    let config =
        SolanaConfig { max_level_priority_fee_lamports: 10_000, ..SolanaConfig::default() };
    let harness = Harness::with(config);
    harness.node.priced().unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(
        Method::GetRecentPrioritizationFees,
        Answer::ok(&json!([{ "slot": 1, "prioritizationFee": 500_000_000_000u64 }])),
    );
    let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000)).await.unwrap();
    assert!(
        quote.fee.base_units() <= u128::from(common::BASE_FEE_LAMPORTS + 10_000),
        "{}",
        quote.fee
    );

    let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    assert!(
        unsigned.preview.warnings.iter().any(|warning| warning.kind == TxWarningKind::Estimated
            && warning.message.starts_with("The quoted priority fee exceeds Jelly's limit"))
    );
}

#[tokio::test]
async fn a_chosen_fee_is_never_capped() {
    let config = SolanaConfig { max_level_priority_fee_lamports: 1, ..SolanaConfig::default() };
    let harness = Harness::with(config);
    harness.node.priced().unwrap();
    harness.watch_owner().await.unwrap();
    let quote =
        harness.adapter.quote_fee(&with_chosen_fee(PEER, native(), 1_000, 1_000)).await.unwrap();
    assert_eq!(quote.fee, Amount::from(common::BASE_FEE_LAMPORTS + 1_000));
    let chosen =
        harness.adapter.build_tx(&with_chosen_fee(PEER, native(), 1_000, 1_000)).await.unwrap();
    assert_eq!(chosen.preview.fee, quote.fee);
    assert!(
        !chosen
            .preview
            .warnings
            .iter()
            .any(|warning| warning.message.starts_with("The quoted priority fee exceeds")),
        "{:?}",
        chosen.preview.warnings
    );
}

#[tokio::test]
async fn a_fee_selection_from_another_chain_is_refused() {
    let harness = Harness::priced().await.unwrap();
    let mut request = request(PEER, native(), 1_000);
    request.fee = FeeSelection::Rate { rate: String::from("3") };
    let error = harness.adapter.quote_fee(&request).await.expect_err("another chain's fee");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error}");
}

#[tokio::test]
async fn a_priority_fee_that_is_not_whole_lamports_or_above_one_sol_is_refused() {
    let harness = Harness::priced().await.unwrap();
    for (text, message) in [
        ("1.5", "Enter a whole number for the priority fee in lamports."),
        ("1000000001", "The priority fee can be at most 1 SOL."),
    ] {
        let mut request = request(PEER, native(), 1_000);
        request.fee = FeeSelection::Solana { priority_fee_lamports: text.to_owned() };
        let error = harness.adapter.quote_fee(&request).await.expect_err("an unusable fee");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
        assert_eq!(error.to_string(), message);
    }
}

#[tokio::test]
async fn a_token_quote_leaves_the_rent_deposit_out_of_the_fee() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, true).unwrap();
    let quote = harness.adapter.quote_fee(&request(PEER, usdc(), 1_000_000)).await.unwrap();
    assert!(quote.fee.base_units() < 2_039_280, "{}", quote.fee);
    let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
    let rent = detail(&unsigned.preview.details, "Token account rent").unwrap();
    assert!(rent.contains("2039280"), "{rent}");
}

#[tokio::test]
async fn a_quote_and_a_build_agree_on_the_fee() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, false).unwrap();
    let quote = harness.adapter.quote_fee(&request(PEER, usdc(), 1_000_000)).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
    assert_eq!(unsigned.preview.fee, quote.fee);
    assert_eq!(
        detail(&unsigned.preview.details, "Source token account"),
        Some(token_account_address(common::OWNER, TokenId::UsdcSpl).unwrap())
    );
}
