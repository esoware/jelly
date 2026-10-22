use jelly_core::{
    Amount, ChainAdapter as _, ChainError, FeeLevel, FeeQuote, FeeSelection, SendAmount, TokenId,
    WalletEvent,
};
use jelly_evm::Method;

use crate::common::{
    self, ARBITRUM, AVALANCHE, Answer, BASE, ETH, ETHEREUM, Fallible, GWEI, Harness, L1_FEE_WEI,
    NETWORKS, OWNER, PEER, SHEET_ENTRIES, Setup, detail, evm_fee, quantity, request, sheet,
    sheet_index, usdt, word,
};

/// Scripts the nonce, native balance, fee history, gas estimate and L1 data fee a
/// quote reads. Only an OP Stack network asks for the data fee.
fn preflight(harness: &Harness, nonce: u64, native_wei: u128, key: &str) -> Fallible<()> {
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(u128::from(nonce))));
    harness.node.always(Method::GetBalance, Answer::ok(&quantity(native_wei)));
    harness.node.always(Method::FeeHistory, Answer::ok(&common::fixture("fee_history.json", key)?));
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));
    harness.node.always(Method::Call, Answer::ok(&word(L1_FEE_WEI)));
    Ok(())
}

/// The gas limit, cap and tip a quote's custom fee pays.
fn priced(quote: &FeeQuote) -> Fallible<(u64, u128, u128)> {
    let Some(FeeSelection::Evm {
        ref gas_limit,
        ref max_fee_per_gas,
        ref max_priority_fee_per_gas,
    }) = quote.custom
    else {
        return Err("an evm quote names an evm custom fee".into());
    };
    let read = |text: &Option<String>| text.as_deref().unwrap_or_default().parse::<u128>();
    Ok((
        gas_limit.as_deref().unwrap_or_default().parse()?,
        read(max_fee_per_gas)?,
        read(max_priority_fee_per_gas)?,
    ))
}

/// The recorded history's three columns have medians of 1, 2 and 3 gwei over a
/// predicted 12 gwei base fee.
const SLOW_TIP_WEI: u128 = 940_000_000;
const NORMAL_TIP_WEI: u128 = 1_940_000_000;
const FAST_TIP_WEI: u128 = 2_940_000_000;

#[tokio::test]
async fn a_normal_quote_pays_the_normal_level() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 7, 5 * ETH, "result").unwrap();

    let quote = harness.adapter.quote_fee(&request(PEER, ETHEREUM.native(), ETH)).await.unwrap();
    assert_eq!(
        priced(&quote).unwrap(),
        (21_000, 14_400_000_000 + NORMAL_TIP_WEI, NORMAL_TIP_WEI),
        "a transfer that runs no code uses exactly its intrinsic gas, so it is not padded"
    );
    assert_eq!(quote.fee.base_units(), 21_000 * (12 * GWEI + NORMAL_TIP_WEI));
    assert_eq!(quote.max_amount, None);
}

#[tokio::test]
async fn a_quote_prices_every_level_so_each_can_be_shown_with_its_cost() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 7, 5 * ETH, "result").unwrap();

    let quote = harness.adapter.quote_fee(&request(PEER, ETHEREUM.native(), ETH)).await.unwrap();
    let levels: Vec<(FeeLevel, u128)> =
        quote.levels.iter().map(|row| (row.level, row.fee.base_units())).collect();
    assert_eq!(
        levels,
        vec![
            (FeeLevel::Slow, 21_000 * (12 * GWEI + SLOW_TIP_WEI)),
            (FeeLevel::Normal, 21_000 * (12 * GWEI + NORMAL_TIP_WEI)),
            (FeeLevel::Fast, 21_000 * (12 * GWEI + FAST_TIP_WEI)),
        ]
    );
    assert_eq!(harness.node.calls_to(Method::FeeHistory).len(), 1, "one read prices them all");
    let asked = harness.node.calls_to(Method::FeeHistory);
    assert!(
        asked.first().is_some_and(|call| call.params.ends_with(r#""latest",[10,20,30]]"#)),
        "{asked:?}"
    );
}

#[tokio::test]
async fn a_level_quote_pays_that_levels_pair_and_a_level_build_matches_it() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 7, 5 * ETH, "result").unwrap();

    for (level, cap, tip) in [
        (FeeLevel::Slow, 13_200_000_000 + SLOW_TIP_WEI, SLOW_TIP_WEI),
        (FeeLevel::Fast, 15 * GWEI + FAST_TIP_WEI, FAST_TIP_WEI),
    ] {
        let mut request = request(PEER, ETHEREUM.native(), ETH);
        request.fee = FeeSelection::Level { level };
        let quote = harness.adapter.quote_fee(&request).await.unwrap();
        assert_eq!(priced(&quote).unwrap(), (21_000, cap, tip), "{level}");
        assert_eq!(
            quote.levels.iter().find(|row| row.level == level).map(|row| row.fee),
            Some(quote.fee),
            "{level}"
        );
        let unsigned = harness.adapter.build_tx(&request).await.unwrap();
        assert_eq!(unsigned.preview.fee, quote.fee, "{level}");
        harness.adapter.pending_tracker().unwrap().discard(&unsigned).await.unwrap();
    }
}

/// Every level is held at the C-Chain oracle's 150 wei.
///
/// Every recorded block tipped far above what the node's own oracle suggests, over
/// a base fee resting at its 2.5 gwei floor, and the node predicted 2.5035 gwei
/// next.
#[tokio::test]
async fn a_c_chain_quote_tips_what_its_own_oracle_would_over_its_base_fee() {
    let harness = Harness::new(Setup::on(AVALANCHE)).unwrap();
    harness.watch(&[common::watched(7, AVALANCHE.chain, OWNER)]).await.unwrap();
    preflight(&harness, 0, 5 * ETH, "avalanche").unwrap();

    let quote = harness.adapter.quote_fee(&request(PEER, AVALANCHE.native(), ETH)).await.unwrap();
    let base_fee = 2_503_533_220;
    let tip = 150;
    assert_eq!(priced(&quote).unwrap(), (21_000, 3_004_239_864 + tip, tip));
    assert_eq!(quote.fee.base_units(), 21_000 * (base_fee + tip));
    assert!(quote.levels.iter().all(|row| row.fee == quote.fee), "{:?}", quote.levels);
}

#[tokio::test]
async fn a_quote_costs_one_batch_and_one_estimate() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 7, 5 * ETH, "result").unwrap();
    harness.adapter.quote_fee(&request(PEER, ETHEREUM.native(), ETH)).await.unwrap();

    // The nonce, balance and fee history share one batch. The gas estimate needs
    // the amount, so it is the second and last round trip.
    assert_eq!(harness.node.calls_to(Method::GetTransactionCount).len(), 1);
    assert_eq!(harness.node.calls_to(Method::GetBalance).len(), 1);
    assert_eq!(harness.node.calls_to(Method::FeeHistory).len(), 1);
    assert_eq!(harness.node.calls_to(Method::EstimateGas).len(), 1);
    assert_eq!(harness.node.calls().len(), 4);
}

#[tokio::test]
async fn a_level_tip_falls_back_to_the_network_floor_when_none_was_sampled() {
    for network in NETWORKS {
        for key in ["empty_blocks", "no_reward_array"] {
            let harness = Harness::new(Setup::on(network)).unwrap();
            harness.watch(&[common::watched(7, network.chain, OWNER)]).await.unwrap();
            preflight(&harness, 0, 5 * ETH, key).unwrap();

            let quote =
                harness.adapter.quote_fee(&request(PEER, network.native(), ETH)).await.unwrap();
            let (_, _, tip) = priced(&quote).unwrap();
            assert_eq!(tip, network.min_priority_fee_wei, "{:?} {key}", network.chain);
        }
    }
}

#[tokio::test]
async fn a_node_that_omits_the_trailing_prediction_is_priced_at_its_newest_base_fee() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, 5 * ETH, "no_prediction").unwrap();
    let quote = harness.adapter.quote_fee(&request(PEER, ETHEREUM.native(), ETH)).await.unwrap();
    let floor = ETHEREUM.min_priority_fee_wei;
    assert_eq!(quote.fee.base_units(), 21_000 * (10 * GWEI + floor));
}

#[tokio::test]
async fn a_send_max_quote_leaves_the_worst_case_behind() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), 0);
    request.amount = SendAmount::Max;
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    let (limit, cap, _) = priced(&quote).unwrap();
    let worst = u128::from(limit).checked_mul(cap).unwrap();
    assert_eq!(quote.max_amount.map(Amount::base_units), ETH.checked_sub(worst));
}

/// A recipient whose code depends on the coin it receives, such as WETH's deposit
/// writing a fresh balance slot, uses far more gas for a payment than for nothing.
#[tokio::test]
async fn a_native_send_max_estimates_gas_with_its_value_attached() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), 0);
    request.amount = SendAmount::Max;
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert!(quote.max_amount.is_some_and(|amount| amount.base_units() > 0));

    let estimates = harness.node.calls_to(Method::EstimateGas);
    let estimate = estimates.first().expect("the limit was estimated");
    assert!(
        estimate.params.contains("\"value\""),
        "the transaction carries almost the whole balance, so the gas it needs has to be \
         estimated with coin attached, but the estimate asked about {}",
        estimate.params
    );
}

#[tokio::test]
async fn a_token_send_max_quote_names_the_whole_token_balance() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, ETH, "result").unwrap();
    harness.node.on(Method::Call, Answer::ok(&word(10_000_000)));

    let mut request = request(PEER, usdt(), 0);
    request.amount = SendAmount::Max;
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.max_amount.map(Amount::base_units), Some(10_000_000));
}

#[tokio::test]
async fn a_custom_pair_is_quoted_at_what_it_is_expected_to_pay() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 3, 5 * ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(None, Some(99 * GWEI), Some(3 * GWEI));
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(priced(&quote).unwrap(), (21_000, 99 * GWEI, 3 * GWEI));
    assert_eq!(
        quote.fee.base_units(),
        21_000 * 15 * GWEI,
        "the next base fee plus the tip, well under the cap"
    );
}

#[tokio::test]
async fn a_pinned_pair_is_quoted_at_its_worst_case_when_the_node_will_not_serve_fee_history() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 3, 5 * ETH, "result").unwrap();
    harness.node.always(Method::FeeHistory, Answer::refused("the method does not exist"));

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(None, Some(99 * GWEI), Some(3 * GWEI));
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(priced(&quote).unwrap(), (21_000, 99 * GWEI, 3 * GWEI));
    assert_eq!(quote.fee.base_units(), 21_000 * 99 * GWEI, "no base fee to price against");

    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.fee, quote.fee);

    request.fee = evm_fee(None, None, Some(3 * GWEI));
    harness.adapter.quote_fee(&request).await.expect_err("an automatic half needs the fee history");
}

#[tokio::test]
async fn half_a_fee_set_by_hand_takes_the_other_half_from_the_market() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, 5 * ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(None, Some(99 * GWEI), None);
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(priced(&quote).unwrap(), (21_000, 99 * GWEI, NORMAL_TIP_WEI));
}

#[tokio::test]
async fn a_gas_limit_set_by_hand_is_used_and_named() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, 5 * ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(Some(30_000), None, None);
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(priced(&quote).unwrap().0, 30_000);
    assert!(
        harness.node.calls_to(Method::EstimateGas).is_empty(),
        "a limit set by hand needs no estimate"
    );
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(detail(&unsigned.preview.details, "Gas limit source"), "Set by hand");
}

#[tokio::test]
async fn a_gas_limit_above_the_eip_7825_cap_is_refused() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, u128::MAX, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(Some(16_777_217), None, None);
    let error = harness.adapter.quote_fee(&request).await.expect_err("above the cap");
    assert!(matches!(error, ChainError::InvalidInput(ref text, _) if text.contains("16777216")));
}

/// A token contract that would revert the transfer, such as one whose issuer froze
/// the sender, makes the estimate revert, and the send is refused before review.
#[tokio::test]
async fn an_estimate_that_reverts_refuses_the_send_with_copy_the_user_can_act_on() {
    for revert in ["execution reverted: blacklisted", "gas required exceeds allowance (30000000)"] {
        let harness = Harness::ethereum().unwrap();
        harness.watch_owner().await.unwrap();
        preflight(&harness, 0, 5 * ETH, "result").unwrap();
        harness.node.on(Method::Call, Answer::ok(&word(10_000_000)));
        harness
            .node
            .always(Method::EstimateGas, Answer::Error { code: 3, message: revert.to_owned() });

        let ask = request(PEER, usdt(), 2_500_000);
        for refused in [
            harness.adapter.quote_fee(&ask).await.expect_err("a reverting quote"),
            harness.adapter.build_tx(&ask).await.expect_err("a reverting build"),
        ] {
            let ChainError::InvalidInput(text, _) = refused else {
                panic!("a revert is refused as copy: {refused:?}")
            };
            assert_eq!(
                text,
                "The network expects this transaction to fail, so Jelly will not send it. Check \
                 the recipient, the amount, and any custom data."
            );
            assert!(!text.contains("blacklisted"), "the node's own wording never reaches the user");
        }
        assert_eq!(harness.adapter.in_flight(), 0, "a refused build holds no nonce slot");
    }
}

#[tokio::test]
async fn an_estimate_the_node_cannot_answer_fails_the_quote_as_the_node_failed() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, 5 * ETH, "result").unwrap();
    harness.node.always(Method::EstimateGas, Answer::Unreachable);

    let error = harness
        .adapter
        .quote_fee(&request(PEER, ETHEREUM.native(), ETH))
        .await
        .expect_err("no estimate, no send");
    assert!(
        matches!(error, ChainError::EndpointsDown { .. }),
        "the failure keeps its own code: {error:?}"
    );

    harness
        .node
        .always(Method::EstimateGas, Answer::Error { code: -32603, message: "busy".into() });
    let error = harness
        .adapter
        .quote_fee(&request(PEER, ETHEREUM.native(), ETH))
        .await
        .expect_err("no estimate, no send");
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
}

#[tokio::test]
async fn a_token_quote_pads_the_estimate_and_reads_the_token_balance() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.on(Method::GetTransactionCount, Answer::ok(&quantity(0)));
    harness.node.on(Method::GetBalance, Answer::ok(&quantity(ETH)));
    harness.node.on(Method::Call, Answer::ok(&word(10_000_000)));
    harness.node.on(
        Method::FeeHistory,
        Answer::ok(&common::fixture("fee_history.json", "result").unwrap()),
    );
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(65_000)));

    let quote = harness.adapter.quote_fee(&request(PEER, usdt(), 2_500_000)).await.unwrap();
    assert_eq!(priced(&quote).unwrap().0, 78_000, "the estimate and a fifth again");
    assert_eq!(harness.node.calls_to(Method::Call).len(), 1, "one balanceOf, inside the batch");
}

#[tokio::test]
async fn a_tip_above_the_cap_is_refused() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    preflight(&harness, 0, 5 * ETH, "result").unwrap();

    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = evm_fee(None, Some(GWEI), Some(30 * GWEI));
    let error = harness.adapter.quote_fee(&request).await.expect_err("a tip above the cap");
    assert!(matches!(error, ChainError::InvalidInput(..)), "{error:?}");
}

#[tokio::test]
async fn a_fee_selection_from_another_chain_is_refused_rather_than_ignored() {
    let harness = Harness::ethereum().unwrap();
    harness.watch_owner().await.unwrap();
    let mut request = request(PEER, ETHEREUM.native(), ETH);
    request.fee = FeeSelection::Rate { rate: String::from("5") };
    let error = harness.adapter.quote_fee(&request).await.expect_err("another chain's controls");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
    assert!(harness.node.calls().is_empty(), "nothing was read");
}

#[tokio::test]
async fn a_quote_for_an_address_that_is_not_watched_is_refused() {
    let harness = Harness::ethereum().unwrap();
    harness.node.quiet(&sheet(ETH));
    let error = harness
        .adapter
        .quote_fee(&request(PEER, ETHEREUM.native(), ETH))
        .await
        .expect_err("an unknown sender");
    assert!(
        matches!(error, ChainError::InvalidInput(ref text, _) if text.contains("no longer available in this wallet"))
    );
}

#[tokio::test]
async fn an_aggregate_entry_that_failed_reads_as_unknown_rather_than_zero() {
    let harness = Harness::new(Setup::default().one_window()).unwrap();
    harness.watch_owner().await.unwrap();
    let mut balances = sheet(5 * ETH);
    balances[sheet_index(ETHEREUM, TokenId::UsdtErc20).unwrap()] = None;
    balances[sheet_index(ETHEREUM, TokenId::UsdcErc20).unwrap()] = Some(7);
    harness.node.quiet(&balances);

    harness.sync().await.unwrap();
    let balances = harness
        .drain()
        .into_iter()
        .filter(|event| matches!(*event, WalletEvent::BalanceChanged(_)))
        .count();
    assert_eq!(
        balances,
        SHEET_ENTRIES.saturating_sub(1),
        "the failed entry is absent, not reported as zero"
    );
}

/// Base charges the L1 data fee its oracle quotes on top of the gas, so a quote
/// adds it and asks for it with the size of the transaction it priced.
#[tokio::test]
async fn an_op_stack_quote_adds_the_oracles_l1_data_fee_to_the_gas() {
    let harness = Harness::new(Setup::on(BASE)).unwrap();
    harness.watch(&[common::watched(7, BASE.chain, OWNER)]).await.unwrap();
    preflight(&harness, 7, 5 * ETH, "result").unwrap();

    let quote = harness.adapter.quote_fee(&request(PEER, BASE.native(), ETH)).await.unwrap();
    assert_eq!(quote.fee.base_units(), 21_000 * (12 * GWEI + NORMAL_TIP_WEI) + L1_FEE_WEI);
    assert!(
        quote.levels.iter().all(|row| row.fee.base_units() > L1_FEE_WEI),
        "every level carries the data fee: {:?}",
        quote.levels
    );

    let asked = harness.node.calls_to(Method::Call);
    let [oracle] = asked.as_slice() else { panic!("one oracle read: {asked:?}") };
    let params = oracle.params.to_ascii_lowercase();
    assert!(params.contains(r#""to":"0x420000000000000000000000000000000000000f""#), "{params}");
    assert!(
        params.contains(&format!("0xf1c7a58b{:064x}", 52)),
        "the oracle prices the 52 unsigned bytes of the payment: {params}"
    );
}

#[tokio::test]
async fn an_op_stack_send_max_leaves_the_l1_data_fee_behind_with_the_gas() {
    let harness = Harness::new(Setup::on(BASE)).unwrap();
    harness.watch(&[common::watched(7, BASE.chain, OWNER)]).await.unwrap();
    preflight(&harness, 0, ETH, "result").unwrap();

    let mut request = request(PEER, BASE.native(), 0);
    request.amount = SendAmount::Max;
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    let (limit, cap, _) = priced(&quote).unwrap();
    let reserved =
        u128::from(limit).checked_mul(cap).and_then(|gas| gas.checked_add(2 * L1_FEE_WEI)).unwrap();
    assert_eq!(
        quote.max_amount.map(Amount::base_units),
        ETH.checked_sub(reserved),
        "twice the oracle's bound, since L1 prices can rise before the send lands"
    );
}

#[tokio::test]
async fn a_network_that_prices_its_data_in_gas_never_asks_the_oracle() {
    for network in [ETHEREUM, ARBITRUM, AVALANCHE] {
        let harness = Harness::new(Setup::on(network)).unwrap();
        harness.watch(&[common::watched(7, network.chain, OWNER)]).await.unwrap();
        preflight(&harness, 0, 5 * ETH, "result").unwrap();
        harness.adapter.quote_fee(&request(PEER, network.native(), ETH)).await.unwrap();
        assert!(harness.node.calls_to(Method::Call).is_empty(), "{:?}", network.chain);
    }
}

/// Arbitrum charges its L1 data in gas, so even a payment is estimated above the
/// intrinsic 21,000 and padded like any call that runs code.
#[tokio::test]
async fn an_arbitrum_payment_pays_the_gas_its_estimate_says_its_l1_data_costs() {
    let harness = Harness::new(Setup::on(ARBITRUM)).unwrap();
    harness.watch(&[common::watched(7, ARBITRUM.chain, OWNER)]).await.unwrap();
    preflight(&harness, 0, 5 * ETH, "result").unwrap();
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(300_000)));

    let quote = harness.adapter.quote_fee(&request(PEER, ARBITRUM.native(), ETH)).await.unwrap();
    assert_eq!(priced(&quote).unwrap().0, 360_000);
}
