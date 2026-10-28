use std::sync::Arc;

use jelly_core::{Amount, ChainAdapter as _, FeeQuote, TxDetail, TxRequest};
use jelly_tron::{Route, test_util::RECIPIENT};

use crate::common::{
    Answer, FRESH, FakeNode, Fallible, TRX, USDT_ASSET, detail, funded, ready, request, resources,
    send_max,
};

async fn quote(fake: &Arc<FakeNode>, request: &TxRequest) -> Fallible<FeeQuote> {
    let harness = ready(fake).await?;
    Ok(harness.adapter.quote_fee(request).await?)
}

/// The review rows, which explain the charge a quote states.
async fn preview_details(fake: &Arc<FakeNode>, request: &TxRequest) -> Fallible<Vec<TxDetail>> {
    let harness = ready(fake).await?;
    Ok(harness.adapter.build_tx(request).await?.preview.details)
}

#[tokio::test]
async fn tron_offers_no_fee_choice_because_it_has_no_fee_market() {
    let fake = funded(100_000_000);
    let quote = quote(&fake, &request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert_eq!(quote.fee, Amount::ZERO, "the daily allowance covers this transfer");
    assert_eq!(quote.custom, None);
    assert!(quote.levels.is_empty());
    let rows = preview_details(&fake, &request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert!(detail(&rows, "Bandwidth charge").unwrap().contains("No TRX is burned."));
    assert_eq!(detail(&rows, "Estimated burn").unwrap(), "0 TRX");
}

#[tokio::test]
async fn a_token_transfer_spells_out_bandwidth_energy_and_the_burn() {
    let fake = funded(100_000_000);
    let ask = request(USDT_ASSET, RECIPIENT, 5_000_000);
    let quote = quote(&fake, &ask).await.unwrap();
    assert_eq!(quote.fee, Amount::from_base_units(13_026_600));

    let rows = preview_details(&fake, &ask).await.unwrap();
    assert_eq!(detail(&rows, "Energy").unwrap(), "130285 needed, 19 available");
    assert!(detail(&rows, "Energy charge").unwrap().contains("130266 energy short"));
    assert!(detail(&rows, "Bandwidth charge").unwrap().contains("free daily bandwidth"));
    let limit = detail(&rows, "Fee limit").unwrap();
    assert!(
        limit.starts_with("The fee limit is 100 TRX."),
        "the floor, above the measured need: {limit}"
    );
    assert!(limit.contains("ceiling, not a charge"), "{limit}");
    assert_eq!(detail(&rows, "Estimated burn").unwrap(), "13.0266 TRX");
}

#[tokio::test]
async fn a_transfer_to_a_fresh_address_quotes_the_activation_fee() {
    let fake = funded(100_000_000).on_matching(Route::Account, FRESH, Answer::body("{}"));
    let quote = quote(&fake, &request(TRX, FRESH, 1_000_000)).await.unwrap();
    assert_eq!(quote.fee, Amount::from_base_units(1_100_000));
    let rows = preview_details(&fake, &request(TRX, FRESH, 1_000_000)).await.unwrap();
    assert!(
        detail(&rows, "Bandwidth charge").unwrap().contains("account-creation fee"),
        "the charge line names the rule that applied"
    );
}

#[tokio::test]
async fn a_transfer_that_burns_says_the_pools_fell_short() {
    // Each pool holds fewer points than the transfer needs, and TRON doesn't
    // combine them, so the whole cost burns.
    let fake = funded(100_000_000).always(Route::AccountResource, resources(400, 200, 0));
    let quote = quote(&fake, &request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert!(quote.fee > Amount::ZERO);
    let rows = preview_details(&fake, &request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    let charge = detail(&rows, "Bandwidth charge").unwrap();
    assert!(charge.contains("Neither pool covers"), "{charge}");
    assert!(charge.contains("1000 sun"), "{charge}");
}

#[tokio::test]
async fn staked_energy_that_covers_a_call_costs_nothing() {
    let fake = funded(100_000_000).always(Route::AccountResource, resources(0, 100_000, 200_000));
    let ask = request(USDT_ASSET, RECIPIENT, 5_000_000);
    let quote = quote(&fake, &ask).await.unwrap();
    assert_eq!(quote.fee, Amount::ZERO);
    let rows = preview_details(&fake, &ask).await.unwrap();
    assert!(detail(&rows, "Energy charge").unwrap().contains("No TRX is burned."));
    assert!(detail(&rows, "Bandwidth charge").unwrap().contains("staked bandwidth"));
}

#[tokio::test]
async fn a_quote_and_the_build_after_it_agree_on_the_fee() {
    let fake = funded(100_000_000).always(Route::AccountResource, resources(600, 0, 0));
    let harness = ready(&fake).await.unwrap();
    let ask = request(TRX, RECIPIENT, 1_000_000);
    let quote = harness.adapter.quote_fee(&ask).await.unwrap();
    let unsigned = harness.adapter.build_tx(&ask).await.unwrap();
    assert_eq!(quote.fee, unsigned.preview.fee);
}

#[tokio::test]
async fn a_send_max_quote_names_the_amount_the_build_after_it_sends() {
    let fake = funded(100_000_000).always(Route::AccountResource, resources(600, 0, 0));
    let harness = ready(&fake).await.unwrap();
    let ask = send_max(TRX, RECIPIENT);
    let quote = harness.adapter.quote_fee(&ask).await.unwrap();
    let unsigned = harness.adapter.build_tx(&ask).await.unwrap();
    assert_eq!(quote.max_amount, Some(unsigned.preview.amount));
    assert_eq!(
        quote.max_amount.and_then(|amount| amount.checked_add(quote.fee)),
        Some(Amount::from_base_units(100_000_000))
    );

    let fixed = harness.adapter.quote_fee(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert_eq!(fixed.max_amount, None);
}
