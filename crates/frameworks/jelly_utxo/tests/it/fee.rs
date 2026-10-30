//! Fee quotes through the adapter, from every source the reading falls through.

use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, FeeLevel, FeeQuote, FeeRate, FeeSelection,
    ScriptKind, TxRequest, test_util::TxRequestBuilder,
};
use jelly_electrum::{Method, test_util::Answer};
use jelly_utxo::{BumpMethod, RecommendedFees, UtxoParams};

use crate::common::{
    AUCTIONED, BITCOIN, BLOCK_UNIX_SECONDS, FakeFeeApi, Fallible, Harness, Setup, TIP, Wallet,
    drain, every_level_at, fee_rate_vectors, kwu, number, quoted_rate, sats, synced, text,
};

const FUNDING_SAT: u64 = 500_000;

/// A busy mempool, every level above both chains' wallet floors.
const fn recommended() -> RecommendedFees {
    RecommendedFees {
        hour: FeeRate::from_sat_per_kwu(2_750),
        half_hour: FeeRate::from_sat_per_kwu(3_000),
        fastest: FeeRate::from_sat_per_kwu(4_000),
        minimum: None,
    }
}

const fn native(params: &UtxoParams) -> AssetId {
    AssetId::Native(params.chain)
}

fn payment(wallet: &Wallet, params: &UtxoParams, fee: FeeSelection) -> TxRequest {
    TxRequestBuilder::new(7, wallet.theirs.address(), native(params), sats(100_000))
        .fee(fee)
        .build()
}

/// A quote over one confirmed coin, which is the shape most sends have.
async fn quote_over(harness: &Harness, wallet: &Wallet) -> Fallible<FeeQuote> {
    let request = payment(wallet, &harness.params, FeeSelection::NORMAL);
    Ok(harness.adapter.quote_fee(&request).await?)
}

#[tokio::test]
async fn the_fee_services_half_hour_rate_is_preferred_over_the_servers_own_estimator() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, Arc::clone(&api)), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();

        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), recommended().half_hour);
        assert_eq!(api.calls(), 1);
        assert_eq!(harness.calls(Method::EstimateFee), 0, "the estimator was never asked");
    }
}

#[tokio::test]
async fn a_quote_prices_every_level_at_the_fee_it_builds_at() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        let tracker = harness.adapter.pending_tracker().expect("the adapter tracks pending sends");

        let quote = quote_over(&harness, &wallet).await.unwrap();
        let levels: Vec<FeeLevel> = quote.levels.iter().map(|row| row.level).collect();
        assert_eq!(levels, FeeLevel::ALL, "{}", params.chain);
        assert!(
            quote.levels.windows(2).all(|pair| match pair {
                [slower, faster] => slower.fee < faster.fee,
                _ => false,
            }),
            "{}: {:?}",
            params.chain,
            quote.levels
        );
        for row in &quote.levels {
            let level = FeeSelection::Level { level: row.level };
            let built = harness.adapter.build_tx(&payment(&wallet, &params, level)).await.unwrap();
            assert_eq!(built.preview.fee, row.fee, "{} {}", params.chain, row.level);
            tracker.discard(&built).await.unwrap();
        }
    }
}

/// A reading that ranks a faster level below a slower one is raised, so picking a
/// faster level never pays less.
#[tokio::test]
async fn a_faster_level_never_pays_less_than_a_slower_one() {
    for params in AUCTIONED {
        let inverted = RecommendedFees {
            hour: kwu(5_000),
            half_hour: kwu(4_000),
            fastest: kwu(3_000),
            minimum: None,
        };
        let api = Arc::new(FakeFeeApi::answering(inverted));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), kwu(5_000), "{}", params.chain);
        assert!(quote.levels.iter().all(|row| row.fee == quote.fee), "{:?}", quote.levels);
    }
}

#[tokio::test]
async fn a_silent_fee_service_falls_through_to_the_server_estimator() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::silent());
        let (harness, wallet) =
            synced(Setup::with_fees(&params, Arc::clone(&api)), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        harness.serve(Method::EstimateFee, "0.00015");

        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), kwu(3_750), "{}", params.chain);
        assert_eq!(api.calls(), 1);

        // Only a send that can be replaced later may be estimated economically.
        let mode = if params.bump_method == Some(BumpMethod::Replace) {
            "ECONOMICAL"
        } else {
            "CONSERVATIVE"
        };
        let asked = harness.chain.server.calls_to(Method::EstimateFee);
        assert!(asked.iter().all(|call| call.params.contains(mode)), "{}: {asked:?}", params.chain);
        assert!(!asked.is_empty());
    }
}

/// The same backlog clears inside different block counts on each chain.
#[tokio::test]
async fn a_node_with_no_estimate_falls_through_to_the_mempool_histogram_at_each_chains_target() {
    for params in AUCTIONED {
        let (harness, wallet) =
            synced(Setup::on(&params), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        harness.serve(Method::EstimateFee, "-1");
        harness.serve(Method::FeeHistogram, "[[30,2500000],[15,3000000],[1,20000000]]");

        let quote = quote_over(&harness, &wallet).await.unwrap();
        // Bitcoin's three blocks of backlog clear at fifteen. Litecoin's six only
        // clear at one, which its wallet floor raises.
        let expected = if params.chain == BITCOIN.chain { kwu(3_750) } else { kwu(2_500) };
        assert_eq!(quoted_rate(&quote).unwrap(), expected, "{}", params.chain);
    }
}

#[tokio::test]
async fn with_nothing_to_go_on_the_floor_is_all_that_is_offered() {
    for params in AUCTIONED {
        let (harness, wallet) =
            synced(Setup::on(&params), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        harness.serve(
            Method::MempoolInfo,
            r#"{"mempoolminfee":0.00002,"minrelaytxfee":0.00001,"incrementalrelayfee":0.00001}"#,
        );
        harness.serve(Method::EstimateFee, "-1");
        harness.serve(Method::FeeHistogram, "[]");

        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(
            quoted_rate(&quote).unwrap(),
            kwu(500).max(params.min_tx_fee),
            "a mempool charging more is the floor, and the wallet floor still holds"
        );
    }
}

#[tokio::test]
async fn a_level_never_drops_below_the_wallet_minimum() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(every_level_at(kwu(1), Some(kwu(1)))));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), params.min_tx_fee, "{}", params.chain);
    }
}

#[tokio::test]
async fn an_unreadable_server_floor_falls_back_to_the_fee_services_floor() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(every_level_at(kwu(100), Some(kwu(3_000)))));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        harness.chain.server.always(
            Method::MempoolInfo,
            Answer::Error { code: 1, message: "unknown method".to_owned() },
        );

        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), kwu(3_000));
    }
}

#[tokio::test]
async fn a_quote_is_priced_by_building_the_transaction_it_would_produce() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        let quote = quote_over(&harness, &wallet).await.unwrap();
        let tracker = harness.adapter.pending_tracker().expect("the adapter tracks pending sends");

        let custom = quote.custom.clone().expect("a utxo quote names its rate");
        for fee in [FeeSelection::NORMAL, custom] {
            let built = harness.adapter.build_tx(&payment(&wallet, &params, fee)).await.unwrap();
            assert_eq!(built.preview.fee, quote.fee);
            tracker.discard(&built).await.unwrap();
        }
        assert_eq!(quote.max_amount, None);
    }
}

#[tokio::test]
async fn a_manual_rate_is_quoted_as_given_and_refused_below_the_relay_floor() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();
        let chosen = FeeSelection::Rate { rate: "20".to_owned() };
        let quote =
            harness.adapter.quote_fee(&payment(&wallet, &params, chosen.clone())).await.unwrap();
        assert_eq!(quote.custom, Some(chosen.clone()));
        let built = harness.adapter.build_tx(&payment(&wallet, &params, chosen)).await.unwrap();
        assert_eq!(built.preview.fee, quote.fee);

        let below = FeeSelection::Rate { rate: "0.5".to_owned() };
        let error = harness
            .adapter
            .quote_fee(&payment(&wallet, &params, below))
            .await
            .expect_err("below the relay floor");
        assert!(error.to_string().contains("network minimum"), "{error}");
    }
}

#[tokio::test]
async fn a_send_max_quote_names_what_the_build_moves() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[150_000, 90_000])
                .await
                .unwrap();
        let tracker = harness.adapter.pending_tracker().expect("the adapter tracks pending sends");
        for fee in [FeeSelection::NORMAL, FeeSelection::Rate { rate: "20".to_owned() }] {
            let quote = harness
                .adapter
                .quote_fee(&drain(&params, wallet.theirs.address(), fee.clone()))
                .await
                .unwrap();
            let built = harness
                .adapter
                .build_tx(&drain(&params, wallet.theirs.address(), fee))
                .await
                .unwrap();
            assert_eq!(quote.max_amount, Some(built.preview.amount));
            assert_eq!(built.preview.amount.checked_add(quote.fee), Some(sats(wallet.total())));
            tracker.discard(&built).await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_send_max_quote_refuses_when_its_fee_leaves_dust() {
    let api = Arc::new(FakeFeeApi::answering(every_level_at(kwu(250), None)));
    let (harness, wallet) =
        synced(Setup::with_fees(&BITCOIN, api), ScriptKind::P2wpkh, &[400]).await.unwrap();
    let error = harness
        .adapter
        .quote_fee(&drain(&BITCOIN, wallet.theirs.address(), FeeSelection::NORMAL))
        .await
        .expect_err("what is left is dust");
    assert!(error.to_string().contains("network minimum"), "{error}");
}

/// Right after unlock no pass has run yet, so a send-max is priced against the
/// coins the last session's pass left in the cursor rather than against none.
#[tokio::test]
async fn a_send_max_before_this_sessions_first_pass_spends_the_stored_coins() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let (earlier, wallet) = synced(
            Setup::with_fees(&params, Arc::clone(&api)),
            ScriptKind::P2wpkh,
            &[150_000, 90_000],
        )
        .await
        .unwrap();
        let later =
            Harness::over(Setup::with_fees(&params, api), Arc::clone(&earlier.cursors)).unwrap();
        later.chain.tip(TIP, BLOCK_UNIX_SECONDS);
        later.watch(&[wallet.address()]).await.unwrap();

        let quote = later
            .adapter
            .quote_fee(&drain(&params, wallet.theirs.address(), FeeSelection::NORMAL))
            .await
            .unwrap();
        let moved = quote.max_amount.expect("a send-max moves something");
        assert_eq!(moved.checked_add(quote.fee), Some(sats(wallet.total())));
    }
}

#[tokio::test]
async fn a_rate_the_balance_cannot_cover_is_still_priced() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(every_level_at(kwu(250_000), None)));
        let (harness, wallet) =
            synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[110_000]).await.unwrap();
        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert!(quote.fee > Amount::ZERO);
        assert_eq!(quoted_rate(&quote).unwrap(), kwu(250_000));
    }
}

#[tokio::test]
async fn a_quote_preserves_dust_weight_and_fee_ceiling_refusals() {
    for params in AUCTIONED {
        let (harness, wallet) =
            synced(Setup::on(&params), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let request =
            TxRequestBuilder::new(7, wallet.theirs.address(), native(&params), sats(1)).build();
        let error = harness.adapter.quote_fee(&request).await.expect_err("the payment is dust");
        assert!(error.to_string().contains("network minimum"), "{error}");

        let limited = UtxoParams { fee_ceiling: sats(1), ..params };
        let (harness, wallet) =
            synced(Setup::on(&limited), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let error = quote_over(&harness, &wallet).await.expect_err("the fee exceeds the ceiling");
        assert!(error.to_string().contains("exceeds Jelly's limit"), "{error}");

        let limited = UtxoParams { max_standard_weight: bitcoin::Weight::from_wu(1), ..params };
        let (harness, wallet) =
            synced(Setup::on(&limited), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let error = quote_over(&harness, &wallet).await.expect_err("the transaction is too heavy");
        assert!(error.to_string().contains("weight units"), "{error}");
    }
}

#[tokio::test]
async fn a_rate_finer_than_a_rate_can_carry_or_another_chains_fee_is_refused() {
    for params in AUCTIONED {
        let (harness, wallet) =
            synced(Setup::on(&params), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let precise = FeeSelection::Rate { rate: "1.00001".to_owned() };
        let error = harness
            .adapter
            .build_tx(&payment(&wallet, &params, precise))
            .await
            .expect_err("more precision than a rate can carry is refused rather than rounded");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
        assert_eq!(error.to_string(), format!("Enter a fee rate in {}.", params.fee_rate_unit));

        let solana = FeeSelection::Solana { priority_fee_lamports: "5000".to_owned() };
        let error = harness
            .adapter
            .build_tx(&payment(&wallet, &params, solana))
            .await
            .expect_err("not a utxo fee");
        assert_eq!(
            error.to_string(),
            format!("Choose a {} fee rate.", params.chain.display_name())
        );
    }
}

#[tokio::test]
async fn one_reading_serves_a_burst_of_quotes_and_expires_with_its_window() {
    for params in AUCTIONED {
        let api = Arc::new(FakeFeeApi::answering(recommended()));
        let ttl = Duration::from_secs(30);
        let (harness, wallet) =
            synced(Setup::with_fees(&params, Arc::clone(&api)), ScriptKind::P2wpkh, &[FUNDING_SAT])
                .await
                .unwrap();

        quote_over(&harness, &wallet).await.unwrap();
        quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(api.calls(), 1, "a keystroke in the send form is not a round trip");

        harness.clock.advance(ttl.saturating_add(Duration::from_secs(1)));
        quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(api.calls(), 2);
    }
}

/// Each chain's recorded fee answers, driven through the adapter one source at a
/// time.
#[tokio::test]
async fn the_recorded_answers_produce_the_recorded_rates() {
    for params in AUCTIONED {
        let file = fee_rate_vectors(&params).unwrap();
        let node = |pointer: &str| file.pointer(pointer).cloned().unwrap_or_default();
        let expected = |pointer: &str| kwu(number(&node(pointer), "kwu"));

        for pointer in ["/recommended", "/quiet"] {
            let answer = node(pointer).pointer("/answer").cloned().unwrap_or_default();
            let fees: RecommendedFees = serde_json::from_value(answer).unwrap();
            let api = Arc::new(FakeFeeApi::answering(fees));
            let (served, wallet) =
                synced(Setup::with_fees(&params, api), ScriptKind::P2wpkh, &[FUNDING_SAT])
                    .await
                    .unwrap();
            let quote = quote_over(&served, &wallet).await.unwrap();
            assert_eq!(
                quoted_rate(&quote).unwrap(),
                expected(pointer),
                "{}{pointer}",
                params.chain
            );
        }

        // One adapter for the rest, with the cached reading expired between rows so
        // each one asks the server.
        let (harness, wallet) =
            synced(Setup::on(&params), ScriptKind::P2wpkh, &[FUNDING_SAT]).await.unwrap();
        let answer_at = format!("{}PerKvb", params.chain.coin().as_str());
        for pointer in ["/estimate", "/estimateBelowTheFloor"] {
            harness.clock.advance(Duration::from_secs(60));
            harness.serve(Method::EstimateFee, &text(&node(pointer), &answer_at));
            let quote = quote_over(&harness, &wallet).await.unwrap();
            assert_eq!(
                quoted_rate(&quote).unwrap(),
                expected(pointer),
                "{}{pointer}",
                params.chain
            );
        }

        let buckets = node("/histogram/buckets");
        harness.clock.advance(Duration::from_secs(60));
        harness.serve(Method::EstimateFee, "-1");
        harness.serve(Method::FeeHistogram, &serde_json::to_string(&buckets).unwrap());
        let quote = quote_over(&harness, &wallet).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), expected("/histogram"), "{}", params.chain);
    }
}
