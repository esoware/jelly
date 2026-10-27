use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, FeeSelection, SendAmount, TokenId,
    TxRequest, TxWarningKind, UnsignedTx,
};
use jelly_evm::Method;

use crate::common::{
    self, Answer, BASE, BSC, ETH, ETHEREUM, GWEI, Harness, L1_FEE_WEI, OWNER, PEER, POLYGON,
    SECOND, USDC, USDT, detail, evm_fee, pinned, quantity, request, usdt, watched, word,
};

/// Scripts the nonce and the balances a build reads.
fn ready(harness: &Harness, nonce: u64, native_wei: u128, token_units: Option<u128>) {
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(u128::from(nonce))));
    harness.node.always(Method::GetBalance, Answer::ok(&quantity(native_wei)));
    if let Some(units) = token_units {
        harness.node.always(Method::Call, Answer::ok(&word(units)));
    }
}

async fn build(harness: &Harness, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
    harness.adapter.build_tx(request).await
}

#[tokio::test]
async fn a_native_build_previews_the_transaction_fields_and_costs() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 7, 5 * ETH, None);

    let unsigned = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    let preview = &unsigned.preview;

    assert_eq!(preview.chain, ChainId::Ethereum);
    assert_eq!(preview.from, [OWNER]);
    assert_eq!(preview.to, PEER);
    assert_eq!(preview.amount.base_units(), ETH);
    assert_eq!(
        preview.fee.base_units(),
        13 * GWEI * 21_000,
        "the recorded 12 gwei base fee plus the 1 gwei tip, under the 30 gwei cap"
    );

    assert_eq!(detail(&preview.details, "Chain id"), "1");
    assert_eq!(detail(&preview.details, "Asset"), "ETH");
    assert_eq!(detail(&preview.details, "Amount"), "1 ETH");
    assert_eq!(detail(&preview.details, "Recipient"), PEER);
    assert_eq!(detail(&preview.details, "Transaction to"), PEER);
    assert_eq!(detail(&preview.details, "Value in wei"), "1000000000000000000");
    assert_eq!(detail(&preview.details, "Nonce"), "7");
    assert_eq!(detail(&preview.details, "Gas limit"), "21000");
    assert_eq!(detail(&preview.details, "Gas limit source"), "Set by hand");
    assert_eq!(detail(&preview.details, "Transaction type"), "EIP-1559 (type 2)");
    assert_eq!(detail(&preview.details, "Calldata"), "None");
    assert_eq!(detail(&preview.details, "Calldata bytes"), "0");
    assert_eq!(detail(&preview.details, "Max total fee"), "0.00063 ETH (630000000000000 wei)");
    assert_eq!(detail(&preview.details, "Maximum total cost"), "Up to 1.00063 ETH");
    assert_eq!(detail(&preview.details, "Native balance after"), "3.99937 ETH");
    assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
}

#[tokio::test]
async fn a_level_build_shows_the_expected_cost_beside_the_worst_case() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 5 * ETH, None);
    harness.node.always(
        Method::FeeHistory,
        Answer::ok(
            r#"{"baseFeePerGas":["0x2540be400","0x2540be400"],
                "reward":[["0x3b9aca00","0x3b9aca00","0x3b9aca00"]]}"#,
        ),
    );
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));

    let unsigned = build(&harness, &request(PEER, ETHEREUM.native(), ETH)).await.unwrap();
    let details = &unsigned.preview.details;
    assert_eq!(detail(details, "Base fee"), "10 gwei");
    // The normal level's cap is 120 percent of the base fee plus 97 percent of the
    // sampled tip, and a transaction that lands next block pays base plus tip.
    assert_eq!(detail(details, "Max fee per gas"), "12.97 gwei (12970000000 wei)");
    assert_eq!(detail(details, "Max priority fee per gas"), "0.97 gwei (970000000 wei)");
    assert!(
        detail(details, "Estimated fee").starts_with("0.00023037 ETH"),
        "{}",
        detail(details, "Estimated fee")
    );
    // The preview's fee is the one to expect, so it matches the quote, and the
    // worst case stays a detail row.
    assert_eq!(unsigned.preview.fee.base_units(), 10_970_000_000 * 21_000);
    assert!(detail(details, "Max total fee").starts_with("0.00027237 ETH"));
}

#[tokio::test]
async fn a_token_build_calls_the_contract_and_previews_the_human_recipient() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 12, 5 * ETH, Some(10_000_000));

    let unsigned = build(&harness, &pinned(PEER, usdt(), 2_500_000, 40 * GWEI, 2 * GWEI, 65_000))
        .await
        .unwrap();
    let preview = &unsigned.preview;

    assert_eq!(preview.to, PEER, "the preview shows the person, not the contract");
    assert_eq!(detail(&preview.details, "Transaction to"), USDT);
    assert_eq!(detail(&preview.details, "Value in wei"), "0");
    assert_eq!(detail(&preview.details, "Asset"), "USDT (ERC-20)");
    assert_eq!(detail(&preview.details, "Amount"), "2.5 USDT");
    assert_eq!(detail(&preview.details, "Token contract"), USDT);
    assert_eq!(detail(&preview.details, "Token decimals"), "6");
    assert_eq!(detail(&preview.details, "Calldata bytes"), "68");
    assert!(detail(&preview.details, "Calldata").starts_with("0xa9059cbb"), "a transfer call");
    assert!(
        detail(&preview.details, "Maximum total cost").starts_with("2.5 USDT plus up to"),
        "{}",
        detail(&preview.details, "Maximum total cost")
    );
}

#[tokio::test]
async fn tokens_build_and_sign_at_their_own_scale() {
    for token in [
        TokenId::UsdgErc20,
        TokenId::DaiErc20,
        TokenId::UsdsErc20,
        TokenId::PyusdErc20,
        TokenId::PaxgErc20,
        TokenId::XautErc20,
        TokenId::XautBep20,
        TokenId::UsdtPolygon,
        TokenId::UsdcPolygon,
        TokenId::PyusdPolygon,
        TokenId::XautPolygon,
        TokenId::UsdtAvalanche,
        TokenId::UsdcAvalanche,
        TokenId::XautAvalanche,
        TokenId::UsdsAvalanche,
    ] {
        let network = common::NETWORKS
            .into_iter()
            .find(|network| network.chain == token.chain())
            .expect("a shipped network carries the token");
        let harness = Harness::new(common::Setup::on(network)).unwrap();
        harness.watch(&[watched(7, token.chain(), common::SIGNER)]).await.unwrap();
        let balance = Amount::parse("10", token.decimals()).unwrap().base_units();
        let amount = Amount::parse("2.5", token.decimals()).unwrap().base_units();
        ready(&harness, 12, ETH, Some(balance));
        let asset = AssetId::Token(token);
        let unsigned = build(&harness, &pinned(PEER, asset, amount, 40 * GWEI, 2 * GWEI, 65_000))
            .await
            .unwrap();
        assert_eq!(unsigned.preview.asset, asset);
        assert_eq!(unsigned.preview.amount.base_units(), amount);
        assert_eq!(detail(&unsigned.preview.details, "Token contract"), token.contract());
        assert_eq!(
            detail(&unsigned.preview.details, "Token decimals"),
            token.decimals().to_string()
        );
        assert_eq!(detail(&unsigned.preview.details, "Amount"), format!("2.5 {}", token.symbol()));
        assert!(detail(&unsigned.preview.details, "Calldata").starts_with("0xa9059cbb"));
        harness
            .adapter
            .sign_tx(&unsigned, &[common::key_material(common::TEST_KEY).unwrap()])
            .unwrap();
    }
}

#[tokio::test]
async fn gold_send_max_keeps_token_precision_and_pays_gas_in_the_native_coin() {
    for (token, network, text) in [
        (TokenId::PaxgErc20, ETHEREUM, "1.234567890123456789"),
        (TokenId::XautErc20, ETHEREUM, "1.234567"),
        (TokenId::XautBep20, BSC, "1.234567"),
        (TokenId::XautPolygon, POLYGON, "1.234567"),
    ] {
        let harness = Harness::new(common::Setup::on(network)).unwrap();
        harness.watch(&[watched(7, token.chain(), common::SIGNER)]).await.unwrap();
        let balance = Amount::parse(text, token.decimals()).unwrap();
        ready(&harness, 0, ETH, Some(balance.base_units()));
        let mut request = pinned(PEER, AssetId::Token(token), 0, 40 * GWEI, 2 * GWEI, 65_000);
        request.amount = SendAmount::Max;
        let unsigned = build(&harness, &request).await.unwrap();
        assert_eq!(unsigned.preview.amount, balance);
        assert_eq!(unsigned.preview.chain, token.chain());
        assert_eq!(detail(&unsigned.preview.details, "Transaction to"), token.contract());
        assert_eq!(detail(&unsigned.preview.details, "Value in wei"), "0");
        assert_eq!(
            detail(&unsigned.preview.details, "Amount"),
            format!("{text} {}", token.symbol())
        );
        assert!(detail(&unsigned.preview.details, "Max total fee").contains(network.symbol()));
        harness
            .adapter
            .sign_tx(&unsigned, &[common::key_material(common::TEST_KEY).unwrap()])
            .unwrap();
    }
}

#[tokio::test]
async fn a_bsc_token_build_says_bep_20_and_uses_eighteen_decimals() {
    let harness = Harness::new(common::Setup::on(BSC)).unwrap();
    harness.watch(&[watched(7, ChainId::Bsc, OWNER)]).await.unwrap();
    ready(&harness, 0, 5 * ETH, Some(10 * ETH));

    let asset = AssetId::Token(TokenId::UsdtBep20);
    let unsigned =
        build(&harness, &pinned(PEER, asset, ETH, 5 * GWEI, GWEI, 65_000)).await.unwrap();
    assert_eq!(detail(&unsigned.preview.details, "Asset"), "USDT (BEP-20)");
    assert_eq!(detail(&unsigned.preview.details, "Chain id"), "56");
    assert_eq!(detail(&unsigned.preview.details, "Amount"), "1 USDT");
    assert_eq!(detail(&unsigned.preview.details, "Token decimals"), "18");
}

/// USDT0 and the other issuers' own deployments read as plain erc-20 coins, and
/// the chain id row says which network they are on.
#[tokio::test]
async fn a_polygon_token_build_names_the_chain_id_and_pays_gas_in_pol() {
    let harness = Harness::new(common::Setup::on(POLYGON)).unwrap();
    harness.watch(&[watched(7, ChainId::Polygon, OWNER)]).await.unwrap();
    ready(&harness, 0, 5 * ETH, Some(10_000_000));

    let asset = AssetId::Token(TokenId::UsdtPolygon);
    let unsigned = build(&harness, &pinned(PEER, asset, 1_000_000, 300 * GWEI, 30 * GWEI, 65_000))
        .await
        .unwrap();
    assert_eq!(detail(&unsigned.preview.details, "Asset"), "USDT (ERC-20)");
    assert_eq!(detail(&unsigned.preview.details, "Chain id"), "137");
    assert_eq!(detail(&unsigned.preview.details, "Amount"), "1 USDT");
    assert_eq!(detail(&unsigned.preview.details, "Token decimals"), "6");
    assert_eq!(
        detail(&unsigned.preview.details, "Token contract"),
        "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
    );
    assert_eq!(
        detail(&unsigned.preview.details, "Max total fee"),
        "0.0195 POL (19500000000000000 wei)"
    );
}

#[tokio::test]
async fn a_send_max_native_spends_the_balance_exactly() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, ETH, None);

    let mut request = pinned(PEER, ETHEREUM.native(), 0, 30 * GWEI, GWEI, 21_000);
    request.amount = SendAmount::Max;
    let preview = build(&harness, &request).await.unwrap().preview;

    assert_eq!(
        preview.amount.base_units(),
        ETH - 30 * GWEI * 21_000,
        "send max reserves the worst case, and what the base fee leaves unused is refunded"
    );
    assert!(
        preview.warnings.iter().any(|warning| warning.kind == TxWarningKind::Other
            && warning.message.starts_with("At the maximum gas fee, this leaves")),
        "{:?}",
        preview.warnings
    );
    assert!(
        preview.warnings.iter().all(|warning| warning.kind != TxWarningKind::HighFee),
        "what a send-max leaves says nothing about whether the fee is wrong: {:?}",
        preview.warnings
    );
}

#[tokio::test]
async fn a_send_max_the_fee_alone_exceeds_is_refused_with_both_amounts() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 1_000, None);

    let mut request = pinned(PEER, ETHEREUM.native(), 0, 30 * GWEI, GWEI, 21_000);
    request.amount = SendAmount::Max;
    let error =
        build(&harness, &request).await.expect_err("the fee alone is more than the balance");
    let ChainError::InsufficientFunds { needed, available, .. } = error else {
        panic!("unexpected {error:?}");
    };
    assert_eq!(needed.base_units(), 630_000_000_000_000);
    assert_eq!(available.base_units(), 1_000);
}

#[tokio::test]
async fn a_send_max_token_takes_the_whole_token_balance_and_still_costs_gas() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, ETH, Some(1_234_567));

    let mut request = pinned(PEER, usdt(), 0, 40 * GWEI, 2 * GWEI, 65_000);
    request.amount = SendAmount::Max;
    let preview = build(&harness, &request).await.unwrap().preview;
    assert_eq!(preview.amount.base_units(), 1_234_567);
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.message.contains("still requires ETH for gas")),
        "{:?}",
        preview.warnings
    );
}

#[tokio::test]
async fn a_short_balance_names_the_asset_that_is_short() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();

    // Not enough coin for the transfer plus its fee.
    ready(&harness, 0, 1_000, None);
    let error = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .expect_err("not enough coin");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error:?}");

    // Enough coin for the gas, not enough token for the transfer.
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, ETH, Some(10));
    match build(&harness, &pinned(PEER, usdt(), 11, 40 * GWEI, 2 * GWEI, 65_000)).await {
        Err(ChainError::InsufficientFunds { asset, needed, available }) => {
            assert_eq!(asset, usdt());
            assert_eq!((needed.base_units(), available.base_units()), (11, 10));
        }
        other => panic!("unexpected {other:?}"),
    }

    // Enough token for the transfer, not enough coin for the gas.
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 1, Some(10));
    match build(&harness, &pinned(PEER, usdt(), 5, 40 * GWEI, 2 * GWEI, 65_000)).await {
        Err(ChainError::InsufficientFunds { asset, needed, available }) => {
            assert_eq!(asset, ETHEREUM.native(), "gas is owed in the native coin");
            assert_eq!((needed.base_units(), available.base_units()), (40 * GWEI * 65_000, 1));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn the_hazards_a_build_can_see_become_typed_warnings() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 5 * ETH, None);

    // A zero amount, and a recipient that is the sender.
    let preview = build(&harness, &pinned(OWNER, ETHEREUM.native(), 0, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap()
        .preview;
    assert!(preview.warnings.iter().any(|warning| warning.message.contains("amount is zero")));
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.message.contains("sending to the same address"))
    );

    // A recipient that is a token contract of ours.
    let preview = build(&harness, &pinned(USDC, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap()
        .preview;
    assert!(preview.warnings.iter().any(|warning| warning.message.contains("unrecoverable")));

    let mut request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    request.fee = evm_fee(Some(21_000), Some(30 * GWEI), Some(GWEI));
    request.nonce = Some(41);
    let preview = build(&harness, &request).await.unwrap().preview;
    assert!(preview.warnings.iter().any(|warning| warning.message.contains("nonce 41 manually")));
    assert_eq!(detail(&preview.details, "Nonce"), "41");
}

#[tokio::test]
async fn a_calldata_override_replaces_the_transfer_and_says_so() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 5 * ETH, None);

    let mut request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    request.data = Some(String::from("0xdeadbeef"));
    let preview = build(&harness, &request).await.unwrap().preview;
    assert_eq!(detail(&preview.details, "Calldata"), "0xdeadbeef");
    assert_eq!(detail(&preview.details, "Calldata bytes"), "4");
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.message.contains("Custom calldata overrides the transfer"))
    );
}

#[tokio::test]
async fn a_malformed_custom_value_is_refused_before_the_node_is_asked_anything() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 5 * ETH, None);
    harness.node.forget_calls();

    let custom = |gas_limit: &str, max_fee_per_gas: &str| FeeSelection::Evm {
        gas_limit: Some(gas_limit.to_owned()),
        max_fee_per_gas: Some(max_fee_per_gas.to_owned()),
        max_priority_fee_per_gas: Some(GWEI.to_string()),
    };
    for (fee, data, message) in [
        (custom("21000", "30 gwei"), "0x", "max fee per gas"),
        (custom("21,000", "30000000000"), "0x", "gas limit"),
        (custom("21000", "30000000000"), "0xabc", "Enter hexadecimal data for calldata."),
    ] {
        let mut request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
        request.fee = fee;
        request.data = Some(data.to_owned());
        let quoted = harness.adapter.quote_fee(&request).await.expect_err("it is malformed");
        assert!(matches!(quoted, ChainError::InvalidInput(..)), "{quoted:?}");
        assert!(quoted.to_string().contains(message), "{quoted}");
        let built = build(&harness, &request).await.expect_err("it is malformed");
        assert_eq!(built.to_string(), quoted.to_string());
    }
    assert!(harness.node.calls().is_empty(), "{:?}", harness.node.calls());
    assert_eq!(harness.adapter.in_flight(), 0, "nothing was held");
}

#[tokio::test]
async fn two_builds_in_a_row_take_two_nonces() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    // The node keeps answering with the same pending count, because neither build
    // has been sent.
    ready(&harness, 5, 5 * ETH, None);

    let first = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(detail(&first.preview.details, "Nonce"), "5");
    assert_eq!(harness.adapter.in_flight(), 1);

    let second = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(
        detail(&second.preview.details, "Nonce"),
        "6",
        "a slot this adapter holds is one the node cannot know about"
    );
    assert_eq!(harness.adapter.in_flight(), 2);
}

#[tokio::test]
async fn discarding_a_build_releases_its_nonce_slot() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 5, 5 * ETH, None);

    let unsigned = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    let tracker = harness.adapter.pending_tracker().expect("the adapter tracks its sends");
    tracker.discard(&unsigned).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 0);

    let again = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(detail(&again.preview.details, "Nonce"), "5", "the slot came back");
}

/// The send form re-prepared before an earlier prepare returned.
///
/// The earlier build is discarded after the later one took the next nonce, and the
/// later one is sent. A node queues a transaction above a missing nonce and answers
/// with its hash, and its pending count stays below the hole.
#[tokio::test]
async fn a_build_discarded_below_a_sent_one_leaves_a_gap_the_next_build_fills() {
    let harness = Harness::signer(common::Setup::default(), 5).await.unwrap();
    let request = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    let superseded = build(&harness, &request).await.unwrap();
    assert_eq!(detail(&superseded.preview.details, "Nonce"), "5");
    let sent = harness.send(&request).await.unwrap();
    let tracker = harness.adapter.pending_tracker().expect("the adapter tracks its sends");
    tracker.discard(&superseded).await.unwrap();

    let next = build(&harness, &request).await.unwrap();
    assert_eq!(
        detail(&next.preview.details, "Nonce"),
        "5",
        "nonce 5 was given back and nothing holds it, so it is the first free slot, and the send \
         {} at nonce 6 cannot be mined until something fills it",
        sent.txid.value()
    );
}

#[tokio::test]
async fn a_held_slot_the_interface_abandoned_is_released_by_its_own_timeout() {
    let mut setup = common::Setup::default();
    setup.config.reservation_ttl = Duration::from_secs(60);
    let harness = Harness::new(setup).unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 5, 5 * ETH, None);

    build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000)).await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1);

    harness.clock.advance(Duration::from_secs(120));
    let after = build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(
        detail(&after.preview.details, "Nonce"),
        "5",
        "a slot no send ever claimed does not push the next nonce up for ever"
    );
    assert_eq!(harness.adapter.in_flight(), 1);
}

#[tokio::test]
async fn reusing_a_nonce_is_a_replacement_and_has_to_outbid_what_it_replaces() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 5, 5 * ETH, None);

    build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000)).await.unwrap();

    // The same nonce at the same price cannot replace it.
    let mut same = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    same.nonce = Some(5);
    let error = build(&harness, &same).await.expect_err("a replacement has to pay more");
    let ChainError::InvalidInput(ref text, _) = error else {
        panic!("unexpected {error:?}");
    };
    assert!(text.contains("nonce 5"), "{text}");
    assert!(text.contains("33000000000"), "a tenth above thirty gwei: {text}");

    // Ten percent higher on both halves is accepted and flagged as a replacement.
    let mut higher = pinned(PEER, ETHEREUM.native(), ETH, 33 * GWEI, GWEI, 21_000);
    higher.fee =
        evm_fee(Some(21_000), Some(33 * GWEI), Some(GWEI.saturating_mul(11).saturating_div(10)));
    higher.nonce = Some(5);
    let preview = build(&harness, &higher).await.unwrap().preview;
    let replacement = preview
        .warnings
        .iter()
        .find(|warning| warning.kind == TxWarningKind::Replacement)
        .expect("a replacement says so");
    assert!(replacement.message.contains("nonce 5"), "{}", replacement.message);
    assert!(replacement.message.contains("Only one can confirm"), "{}", replacement.message);
}

#[tokio::test]
async fn a_replacement_that_raises_only_one_half_is_refused() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 5, 5 * ETH, None);
    build(&harness, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000)).await.unwrap();

    let mut cap_only = pinned(PEER, ETHEREUM.native(), ETH, 99 * GWEI, GWEI, 21_000);
    cap_only.nonce = Some(5);
    let error = build(&harness, &cap_only).await.expect_err("the tip did not rise");
    assert!(matches!(error, ChainError::InvalidInput(..)), "{error:?}");
}

#[tokio::test]
async fn each_watched_address_has_its_own_nonce_slots() {
    let harness = Harness::ethereum().unwrap();
    harness
        .watch(&[watched(7, ChainId::Ethereum, OWNER), watched(8, ChainId::Ethereum, SECOND)])
        .await
        .unwrap();
    ready(&harness, 5, 5 * ETH, None);

    let mut first = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    first.from_address_ids = vec![7];
    build(&harness, &first).await.unwrap();

    let mut second = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000);
    second.from_address_ids = vec![8];
    let preview = build(&harness, &second).await.unwrap().preview;
    assert_eq!(
        detail(&preview.details, "Nonce"),
        "5",
        "another sender starts from the node's count"
    );
    assert_eq!(detail(&preview.details, "Recipient"), PEER);
    assert_eq!(preview.from, [SECOND]);
}

#[tokio::test]
async fn a_build_for_an_asset_from_another_network_is_refused_without_a_read() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    let asset = AssetId::Token(TokenId::UsdtBep20);
    let error = build(&harness, &pinned(PEER, asset, 1, 30 * GWEI, GWEI, 21_000))
        .await
        .expect_err("the other network's token");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
    assert!(harness.node.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn two_builds_at_once_take_different_nonces() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 5, 5 * ETH, None);
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));
    harness.node.slow_by(Duration::from_millis(10));

    let mut to_peer = request(PEER, ETHEREUM.native(), ETH);
    to_peer.fee = evm_fee(None, Some(30 * GWEI), Some(GWEI));
    let mut to_second = request(SECOND, ETHEREUM.native(), ETH);
    to_second.fee = evm_fee(None, Some(30 * GWEI), Some(GWEI));

    let (first, second) = tokio::join!(build(&harness, &to_peer), build(&harness, &to_second));
    let first_nonce = detail(&first.unwrap().preview.details, "Nonce");
    let second_nonce = detail(&second.unwrap().preview.details, "Nonce");
    assert_ne!(
        first_nonce, second_nonce,
        "two payments built side by side must not claim one nonce slot"
    );
}

#[tokio::test]
async fn a_native_send_with_calldata_previews_only_the_coin_it_carries() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    ready(&harness, 0, 5 * ETH, None);

    let mut deposit = pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 50_000);
    deposit.data = Some(String::from("0xd0e30db0"));
    let unsigned = build(&harness, &deposit).await.unwrap();

    let document: serde_json::Value = serde_json::from_slice(unsigned.payload.as_bytes()).unwrap();
    let value_hex = document["tx"]["value"].as_str().unwrap();
    let carried = u128::from_str_radix(value_hex.trim_start_matches("0x"), 16).unwrap();
    assert_eq!(
        unsigned.preview.amount.base_units(),
        carried,
        "the amount the user approves is the coin the signed transaction carries"
    );
}

#[tokio::test]
async fn a_recipient_that_is_not_an_address_is_refused_without_a_read() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    for bad in ["", "0x1234", "not an address", &OWNER.replacen("d8dA", "D8dA", 1)] {
        let error = build(&harness, &pinned(bad, ETHEREUM.native(), 1, GWEI, 1, 21_000))
            .await
            .expect_err("not an address");
        assert!(matches!(error, ChainError::InvalidAddress(_)), "{bad}: {error:?}");
    }
    assert!(harness.node.calls().is_empty());
}

#[tokio::test]
async fn an_op_stack_preview_names_the_l1_data_fee_and_counts_it_in_both_fees() {
    let harness = Harness::new(common::Setup::on(BASE)).unwrap();
    harness.watch(&[watched(7, ChainId::Base, OWNER)]).await.unwrap();
    ready(&harness, 7, 5 * ETH, None);
    harness.node.always(Method::Call, Answer::ok(&word(L1_FEE_WEI)));

    let unsigned =
        build(&harness, &pinned(PEER, BASE.native(), ETH, 30 * GWEI, GWEI, 21_000)).await.unwrap();
    let preview = &unsigned.preview;
    assert_eq!(detail(&preview.details, "Chain id"), "8453");
    assert_eq!(detail(&preview.details, "L1 data fee"), "0.000000000973558459 ETH (973558459 wei)");
    assert_eq!(preview.fee.base_units(), 13 * GWEI * 21_000 + L1_FEE_WEI);
    assert_eq!(
        detail(&preview.details, "Max total fee"),
        "0.000630001947116918 ETH (630001947116918 wei)",
        "the gas cap and twice the data fee's bound"
    );

    let ethereum = Harness::ethereum().unwrap();
    ethereum.watch_owner().await.unwrap();
    ready(&ethereum, 7, 5 * ETH, None);
    let unsigned = build(&ethereum, &pinned(PEER, ETHEREUM.native(), ETH, 30 * GWEI, GWEI, 21_000))
        .await
        .unwrap();
    assert_eq!(detail(&unsigned.preview.details, "L1 data fee"), "(no detail row L1 data fee)");
}
