use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, FeeSelection, TokenId, TxWarningKind,
};
use jelly_tron::{
    Route, TronConfig,
    test_util::{OWNER, RECIPIENT, USDT},
};

use crate::common::{
    Answer, FRESH, FakeNode, Harness, TRX, USDT_ASSET, account, base, detail, fixture, funded,
    has_detail, ready, request, resources, send_max, warned, watched,
};

#[tokio::test]
async fn a_trx_preview_describes_the_bytes_that_were_built() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();

    assert_eq!(unsigned.chain(), ChainId::Tron);
    assert_eq!(unsigned.preview.asset, TRX);
    assert_eq!(unsigned.preview.from, [OWNER]);
    assert_eq!(unsigned.preview.to, RECIPIENT);
    assert_eq!(unsigned.preview.amount, Amount::from_base_units(1_000_000));
    // 600 free bandwidth points cover a 266 point transfer, so nothing burns.
    assert_eq!(unsigned.preview.fee, Amount::ZERO);

    let rows = &unsigned.preview.details;
    assert_eq!(detail(rows, "Contract").unwrap(), "TRX transfer");
    assert!(detail(rows, "Expires").unwrap().contains("10 minutes"));
    assert!(detail(rows, "Bandwidth").unwrap().contains("points"));
    assert_eq!(detail(rows, "Estimated burn").unwrap(), "0 TRX");
    assert!(detail(rows, "Prices").unwrap().contains("reported by the node"));
    assert!(!has_detail(rows, "Fee limit"), "a native transfer has no fee limit");
    assert!(
        !warned(&unsigned.preview, TxWarningKind::Other),
        "the expiry window is a detail, not a warning"
    );
}

#[tokio::test]
async fn a_token_preview_shows_the_call_data_the_fee_limit_and_the_energy() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned =
        harness.adapter.build_tx(&request(USDT_ASSET, RECIPIENT, 5_000_000)).await.unwrap();

    assert_eq!(unsigned.preview.asset, USDT_ASSET);
    assert_eq!(unsigned.preview.amount, Amount::from_base_units(5_000_000));
    // The call needs 130285 energy at 100 sun each, and the account holds 19.
    assert_eq!(unsigned.preview.fee, Amount::from_base_units(13_026_600));

    let rows = &unsigned.preview.details;
    assert_eq!(detail(rows, "Token contract").unwrap(), USDT);
    assert_eq!(detail(rows, "Call data bytes").unwrap(), "68", "a selector and two argument words");
    assert!(detail(rows, "Fee limit").unwrap().contains("ceiling, not a charge"));
    assert!(
        detail(rows, "Fee limit").unwrap().starts_with("The fee limit is 100 TRX."),
        "the floor covers the call"
    );
    assert!(detail(rows, "Energy").unwrap().contains("130285 needed, 19 available"));
    assert!(
        warned(&unsigned.preview, TxWarningKind::Estimated),
        "the dynamic energy surcharge can rise before execution and must be called out"
    );
}

#[tokio::test]
async fn a_fresh_recipient_raises_the_activation_warning_and_its_charge() {
    let fake = funded(100_000_000).on_matching(Route::Account, FRESH, Answer::body("{}"));
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, FRESH, 1_000_000)).await.unwrap();

    assert!(warned(&unsigned.preview, TxWarningKind::AccountActivation));
    // One TRX creation fee plus the flat 0.1 TRX bandwidth charge. The free daily
    // allowance cannot pay for an activation, and the sender has no stake.
    assert_eq!(unsigned.preview.fee, Amount::from_base_units(1_100_000));
}

#[tokio::test]
async fn send_max_leaves_exactly_the_estimated_burn_behind() {
    // No free allowance and no stake, so every point is burned and the amount has to
    // settle against a fee that depends on its own size.
    let fake = funded(100_000_000).always(Route::AccountResource, resources(600, 0, 0));
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&send_max(TRX, RECIPIENT)).await.unwrap();

    let spent = unsigned.preview.amount.checked_add(unsigned.preview.fee).expect("a total");
    assert_eq!(spent, Amount::from_base_units(100_000_000));
    assert!(unsigned.preview.fee > Amount::ZERO, "a burned transfer has a fee");
}

#[tokio::test]
async fn send_max_for_a_token_sends_the_whole_token_balance() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let unsigned = harness.adapter.build_tx(&send_max(USDT_ASSET, RECIPIENT)).await.unwrap();
    // constant_balance_of.json reports 0x71e934ebe base units.
    assert_eq!(unsigned.preview.amount, Amount::from_base_units(30_577_741_502));
}

#[tokio::test]
async fn a_balance_below_the_request_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&request(TRX, RECIPIENT, 200_000_000))
        .await
        .expect_err("100 TRX cannot send 200");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error:?}");
}

#[tokio::test]
async fn a_token_transfer_with_no_trx_to_burn_is_refused() {
    let harness = ready(&funded(0)).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&request(USDT_ASSET, RECIPIENT, 1_000_000))
        .await
        .expect_err("the energy has to be paid in TRX");
    let ChainError::InsufficientFunds { asset, .. } = error else {
        panic!("a token transfer that cannot pay its energy is an insufficient-funds refusal");
    };
    assert_eq!(asset, TRX, "the shortfall is in TRX, not in the token");
}

/// The call needs 130285 energy and the account holds 19, so the point burn is
/// 13.0266 TRX. The fee limit allows a fifth more for the surcharge, 156342
/// energy, so the balance has to hold 15.6323 TRX.
#[tokio::test]
async fn a_token_transfer_needs_the_trx_its_padded_energy_burns_not_just_the_point_burn() {
    let harness = ready(&funded(14_000_000)).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&request(USDT_ASSET, RECIPIENT, 1_000_000))
        .await
        .expect_err("enough for the measurement, not for the margin");
    let ChainError::InsufficientFunds { asset, needed, available } = error else {
        panic!("a padded burn the balance cannot cover is an insufficient-funds refusal");
    };
    assert_eq!(asset, TRX);
    assert_eq!(needed, Amount::from_base_units(15_632_300));
    assert_eq!(available, Amount::from_base_units(14_000_000));

    let harness = ready(&funded(15_632_300)).await.unwrap();
    let unsigned =
        harness.adapter.build_tx(&request(USDT_ASSET, RECIPIENT, 1_000_000)).await.unwrap();
    assert_eq!(
        unsigned.preview.fee,
        Amount::from_base_units(13_026_600),
        "the fee shown is still what the measurement burns"
    );
}

#[tokio::test]
async fn a_token_transfer_above_the_token_balance_is_refused_in_the_token() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&request(USDT_ASSET, RECIPIENT, 99_999_999_999))
        .await
        .expect_err("above the holding");
    let ChainError::InsufficientFunds { asset, available, .. } = error else {
        panic!("an amount above the holding is an insufficient-funds refusal");
    };
    assert_eq!(asset, USDT_ASSET);
    assert_eq!(available, Amount::from_base_units(30_577_741_502));
}

#[tokio::test]
async fn an_amount_of_zero_is_refused_whichever_asset_it_is() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    for asset in [TRX, USDT_ASSET] {
        let error =
            harness.adapter.build_tx(&request(asset, RECIPIENT, 0)).await.expect_err("zero");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error:?}");
    }
}

#[tokio::test]
async fn a_fee_selection_from_another_chain_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let mut elsewhere = request(TRX, RECIPIENT, 1_000_000);
    elsewhere.fee = FeeSelection::Rate { rate: String::from("5") };
    let error = harness.adapter.build_tx(&elsewhere).await.expect_err("not a tron selection");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
}

#[tokio::test]
async fn an_asset_from_another_chain_is_refused() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    for asset in [AssetId::Token(TokenId::UsdtErc20), AssetId::Native(ChainId::Ethereum)] {
        let error = harness
            .adapter
            .build_tx(&request(asset, RECIPIENT, 1))
            .await
            .expect_err("not a tron asset");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
    }
}

#[tokio::test]
async fn an_address_the_engine_has_not_pushed_cannot_spend() {
    let harness = Harness::new(&funded(100_000_000));
    let error = harness
        .adapter
        .build_tx(&request(TRX, RECIPIENT, 1))
        .await
        .expect_err("the watch set is empty");
    assert!(error.to_string().contains("no longer available in this wallet"), "{error}");
}

#[tokio::test]
async fn a_recipient_that_is_not_a_tron_address_is_refused_before_any_read() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    harness.fake.forget_calls();
    // Twenty bytes of hex is exactly an Ethereum address.
    let error = harness
        .adapter
        .build_tx(&request(TRX, "0x9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f", 1))
        .await
        .expect_err("a bare twenty byte address");
    assert!(matches!(error, ChainError::InvalidAddress(_)), "{error:?}");
    assert_eq!(harness.fake.count(Route::NowBlock), 0, "nothing is read for a bad recipient");
}

#[tokio::test]
async fn the_governance_parameters_are_read_once_per_reuse_window() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    for _ in 0..3u8 {
        harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    }
    assert_eq!(
        harness.fake.count(Route::ChainParameters),
        1,
        "the prices are per network, not per quote"
    );
}

#[tokio::test]
async fn a_quote_says_when_it_is_running_on_the_compiled_figures() {
    let fake = funded(100_000_000)
        .always(Route::ChainParameters, Answer::body(r#"{"chainParameter":[]}"#));
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert!(detail(&unsigned.preview.details, "Prices").unwrap().contains("Jelly's defaults"));
    assert!(warned(&unsigned.preview, TxWarningKind::Estimated));
}

#[tokio::test]
async fn a_node_that_will_not_report_the_parameters_still_quotes_from_the_compiled_ones() {
    let fake = funded(100_000_000).always(Route::ChainParameters, Answer::Transport);
    let harness = ready(&fake).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert!(detail(&unsigned.preview.details, "Prices").unwrap().contains("Jelly's defaults"));
}

#[tokio::test]
async fn an_expiration_window_a_configuration_got_wrong_is_clamped_not_signed() {
    let config = TronConfig { expiration: Duration::from_secs(1), ..TronConfig::default() };
    let fake = funded(100_000_000);
    let harness = Harness::with(&fake, config);
    harness.adapter.set_watch_set(&[watched(1, OWNER)]).await.unwrap();
    let unsigned = harness.adapter.build_tx(&request(TRX, RECIPIENT, 1_000_000)).await.unwrap();
    assert_eq!(detail(&unsigned.preview.details, "Expires").unwrap(), "15 seconds after building");
}

/// java-tron's `TransferActuator.validate` refuses every `TransferContract` whose
/// `to_address` equals its `owner_address` with "Cannot transfer TRX to yourself.",
/// so such bytes can never land.
#[tokio::test]
async fn a_trx_transfer_to_the_sending_address_is_refused_before_anything_is_signed() {
    let harness = ready(&funded(100_000_000)).await.unwrap();
    let built = harness
        .adapter
        .build_tx(&request(TRX, OWNER, 1_000_000))
        .await
        .map(|unsigned| (unsigned.preview.to, unsigned.preview.fee));
    assert!(
        matches!(built, Err(ChainError::InvalidInput(..))),
        "a TRX transfer from {OWNER} to itself is refused as invalid input before anything is \
         signed, because the network always refuses it: {built:?}"
    );
}

#[tokio::test]
async fn a_contract_call_whose_measurement_reverts_is_refused_with_the_nodes_reason() {
    let fake = FakeNode::new();
    let fake = base(&fake)
        .on_matching(Route::Account, OWNER, account(OWNER, 100_000_000))
        .on_matching(
            Route::ConstantContract,
            "balanceOf(address)",
            Answer::body(fixture!("constant_balance_of")),
        )
        .on_matching(
            Route::ConstantContract,
            "transfer(address,uint256)",
            Answer::body(fixture!("constant_reverted")),
        );
    let harness = ready(&fake).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&request(USDT_ASSET, RECIPIENT, 5_000_000))
        .await
        .expect_err("the measurement reverted");
    assert!(error.to_string().contains("REVERT opcode executed"), "{error}");
}
