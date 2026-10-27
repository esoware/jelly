use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, SendAmount, TokenId, TxWarningKind,
    test_util::WatchedAddressBuilder,
};
use jelly_solana::{Method, SolanaConfig, test_util::TokenState};

use crate::common::{
    self, ACCOUNTS_PER_OWNER, Answer, Fallible, Harness, OWNER, PEER, detail, missing_account,
    native, request, send_max, signed, stock_usdc, system_account, token_account_address,
    token_program_account, usdc, warns, watched,
};

/// The 450 units a simulated transfer consumes, plus the twenty percent margin.
const SIMULATED_LIMIT: u32 = 540;

mod native_transfers {
    use super::*;

    #[tokio::test]
    async fn a_transfer_previews_the_bytes_it_signs() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000_000)).await.unwrap();

        assert_eq!(unsigned.chain(), ChainId::Solana);
        assert_eq!(unsigned.preview.from, [OWNER]);
        assert_eq!(unsigned.preview.to, PEER);
        assert_eq!(unsigned.preview.asset, native());
        assert_eq!(unsigned.preview.amount, Amount::from_base_units(1_000_000));
        assert_eq!(
            detail(&unsigned.preview.details, "Valid until block height"),
            Some(common::EXPIRY_HEIGHT.to_string())
        );

        let transaction = signed(&harness, &request(PEER, native(), 1_000_000)).await.unwrap();
        let message = common::v0_of(&transaction).expect("a v0 message");
        assert_eq!(message.account_keys.first().map(ToString::to_string), Some(OWNER.to_owned()));
        assert_eq!(transaction.signatures.len(), 1);
    }

    #[tokio::test]
    async fn the_preview_names_every_instruction_the_message_carries() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000_000)).await.unwrap();
        let rows: Vec<String> = unsigned
            .preview
            .details
            .iter()
            .filter(|row| row.label.starts_with("Instruction "))
            .map(|row| row.value.clone())
            .collect();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("set compute unit limit")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("set compute unit price")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("transfer 0.001 SOL")), "{rows:?}");
    }

    #[tokio::test]
    async fn every_build_reads_a_blockhash_of_its_own() {
        let harness = Harness::priced().await.unwrap();
        harness.adapter.quote_fee(&request(PEER, native(), 1_000)).await.unwrap();
        let before = harness.node.count(Method::GetLatestBlockhash);
        harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(harness.node.count(Method::GetLatestBlockhash), before + 1);
    }

    #[tokio::test]
    async fn a_first_build_costs_three_round_trips_and_the_next_costs_two() {
        let harness = Harness::priced().await.unwrap();
        harness.node.forget_calls();
        harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(
            harness.node.documents(),
            vec![3, 2, 1],
            "the build reads, the optional reads, and one simulation"
        );

        harness.node.forget_calls();
        harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(
            harness.node.documents(),
            vec![3, 2],
            "the measurement and the rent figures are already known"
        );
    }

    #[tokio::test]
    async fn send_max_leaves_the_fee_behind_and_says_it_empties_the_account() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&send_max(PEER, native())).await.unwrap();
        let fee = unsigned.preview.fee.base_units();
        assert_eq!(
            unsigned.preview.amount,
            Amount::from_base_units(u128::from(common::BALANCE_LAMPORTS) - fee)
        );
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::RentExemption));
        assert_eq!(
            detail(&unsigned.preview.details, "Total SOL debited"),
            detail(&unsigned.preview.details, "SOL balance"),
            "a send-max spends the whole balance"
        );
    }

    #[tokio::test]
    async fn more_than_the_balance_is_refused_in_the_asset_it_falls_short_in() {
        let harness = Harness::priced().await.unwrap();
        let error = harness
            .adapter
            .build_tx(&request(PEER, native(), common::BALANCE_LAMPORTS + 1))
            .await
            .expect_err("more than the balance");
        let ChainError::InsufficientFunds { asset, available, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(asset, native());
        assert_eq!(available, Amount::from(common::BALANCE_LAMPORTS));
    }

    #[tokio::test]
    async fn an_amount_of_zero_or_beyond_sixty_four_bits_is_refused() {
        let harness = Harness::priced().await.unwrap();
        let mut oversized = request(PEER, native(), 1);
        oversized.amount =
            SendAmount::Exact { value: Amount::from_base_units(u128::from(u64::MAX) + 1) };
        let error = harness.adapter.build_tx(&oversized).await.expect_err("too large");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");

        let mut zero = request(PEER, native(), 0);
        zero.amount = SendAmount::Exact { value: Amount::ZERO };
        let error = harness.adapter.build_tx(&zero).await.expect_err("zero");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
    }

    #[tokio::test]
    async fn a_sender_outside_the_watch_set_never_reaches_the_network() {
        let harness = Harness::priced().await.unwrap();
        harness.node.forget_calls();
        let mut request = request(PEER, native(), 1_000);
        request.from_address_ids = vec![99];
        let error = harness.adapter.build_tx(&request).await.expect_err("an unwatched sender");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{error}");
        assert!(harness.node.calls().is_empty(), "nothing was asked of the node");
    }

    #[tokio::test]
    async fn a_recipient_that_is_not_an_address_never_reaches_the_network() {
        let harness = Harness::priced().await.unwrap();
        harness.node.forget_calls();
        let error = harness
            .adapter
            .build_tx(&request("not an address", native(), 1_000))
            .await
            .expect_err("bad");
        assert!(matches!(error, ChainError::InvalidAddress(_)), "{error}");
        assert!(harness.node.calls().is_empty());
    }

    #[tokio::test]
    async fn an_asset_from_another_chain_is_refused() {
        let harness = Harness::priced().await.unwrap();
        let mut foreign = request(PEER, native(), 1_000);
        foreign.asset = AssetId::Native(ChainId::Bitcoin);
        let error = harness.adapter.build_tx(&foreign).await.expect_err("a foreign asset");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error}");

        let mut token = request(PEER, native(), 1_000);
        token.asset = AssetId::Token(TokenId::UsdcErc20);
        let error = harness.adapter.build_tx(&token).await.expect_err("a foreign token");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error}");
    }
}

mod rent_and_warnings {
    use super::*;

    #[tokio::test]
    async fn a_recipient_that_does_not_exist_yet_is_warned_about_below_the_minimum() {
        let harness = Harness::priced().await.unwrap();
        harness.node.at(Method::GetAccountInfo, PEER, Answer::ok(&missing_account()));
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::RentExemption));
        let message = unsigned
            .preview
            .warnings
            .iter()
            .find(|warning| warning.kind == TxWarningKind::RentExemption)
            .map(|warning| warning.message.clone())
            .unwrap_or_default();
        assert!(message.contains("network will reject"), "{message}");
    }

    #[tokio::test]
    async fn a_recipient_above_the_minimum_is_not_warned_about() {
        let harness = Harness::priced().await.unwrap();
        harness.node.at(Method::GetAccountInfo, PEER, Answer::ok(&missing_account()));
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 2_000_000)).await.unwrap();
        assert!(!warns(&unsigned.preview.warnings, TxWarningKind::RentExemption));
    }

    #[tokio::test]
    async fn a_recipient_owned_by_a_program_is_warned_about() {
        let harness = Harness::priced().await.unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            PEER,
            Answer::ok(&token_program_account().unwrap()),
        );
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 2_000_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::AmbiguousAddress));

        harness.node.at(Method::GetAccountInfo, PEER, Answer::ok(&system_account()));
        let ordinary = harness.adapter.build_tx(&request(PEER, native(), 2_000_000)).await.unwrap();
        assert!(!warns(&ordinary.preview.warnings, TxWarningKind::AmbiguousAddress));
    }

    #[tokio::test]
    async fn an_off_curve_recipient_is_warned_about() {
        let harness = Harness::priced().await.unwrap();
        harness.node.at(Method::GetAccountInfo, common::DERIVED, Answer::ok(&system_account()));
        let unsigned =
            harness.adapter.build_tx(&request(common::DERIVED, native(), 2_000_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::AmbiguousAddress));
    }

    #[tokio::test]
    async fn a_leftover_below_the_minimum_is_warned_about() {
        let harness = Harness::priced().await.unwrap();
        // Leave under 200 lamports behind, below the rent-exempt minimum.
        let unsigned = harness
            .adapter
            .build_tx(&request(
                PEER,
                native(),
                common::BALANCE_LAMPORTS - common::BASE_FEE_LAMPORTS - 200,
            ))
            .await
            .unwrap();
        let message = unsigned
            .preview
            .warnings
            .iter()
            .find(|warning| warning.kind == TxWarningKind::RentExemption)
            .map(|warning| warning.message.clone())
            .unwrap_or_default();
        assert!(message.contains("rent minimum"), "{message}");
    }

    #[tokio::test]
    async fn a_rent_figure_that_could_not_be_read_is_declared_an_estimate() {
        let harness = Harness::priced().await.unwrap();
        harness.node.always(
            Method::GetMinimumBalanceForRentExemption,
            Answer::refused(-32602, "invalid params"),
        );
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::Estimated));
    }

    #[tokio::test]
    async fn a_compute_limit_that_could_not_be_measured_is_declared_an_estimate() {
        let harness = Harness::priced().await.unwrap();
        harness.node.always(
            Method::SimulateTransaction,
            Answer::ok(&common::contextual(&serde_json::json!({
                "err": "AccountNotFound",
                "unitsConsumed": serde_json::Value::Null,
            }))),
        );
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit").as_deref(),
            Some("1000")
        );
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit source").as_deref(),
            Some("Fallback estimate")
        );
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::Estimated));
    }

    #[tokio::test]
    async fn an_overflowing_simulation_measurement_uses_the_compiled_limit_without_caching() {
        let harness = Harness::priced().await.unwrap();
        harness.node.always(Method::SimulateTransaction, Answer::ok(&common::simulation(u64::MAX)));
        let request = request(PEER, native(), 1_000);
        let quote = harness.adapter.quote_fee(&request).await.unwrap();
        let unsigned = harness.adapter.build_tx(&request).await.unwrap();
        assert_eq!(quote.fee, unsigned.preview.fee);
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit").as_deref(),
            Some("1000")
        );
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit source").as_deref(),
            Some("Fallback estimate")
        );
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::Estimated));
        assert_eq!(harness.node.count(Method::SimulateTransaction), 2);
    }

    #[tokio::test]
    async fn a_simulation_that_reports_no_units_is_not_a_measurement() {
        let harness = Harness::priced().await.unwrap();
        harness.node.always(Method::SimulateTransaction, Answer::ok(&common::simulation(0)));
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit").as_deref(),
            Some("1000")
        );
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit source").as_deref(),
            Some("Fallback estimate")
        );
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::Estimated));
    }

    /// A custom fee built after a quote reuses the quote's measurement, so it is
    /// not reported as a failed simulation.
    #[tokio::test]
    async fn a_custom_fee_after_a_quote_reuses_its_measurement() {
        let harness = Harness::priced().await.unwrap();
        let quote = harness.adapter.quote_fee(&request(PEER, native(), 1_000)).await.unwrap();
        let mut chosen = request(PEER, native(), 1_000);
        chosen.fee = quote.custom.expect("a solana quote names its priority fee");

        let unsigned = harness.adapter.build_tx(&chosen).await.unwrap();
        assert!(
            !warns(&unsigned.preview.warnings, TxWarningKind::Estimated),
            "{:?}",
            unsigned.preview.warnings
        );
        assert_eq!(harness.node.count(Method::SimulateTransaction), 1);
    }

    #[tokio::test]
    async fn a_measured_limit_carries_the_margin_and_is_reused() {
        let harness = Harness::priced().await.unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit"),
            Some(SIMULATED_LIMIT.to_string()),
            "450 units plus the configured margin"
        );
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit source").as_deref(),
            Some("Measured by simulation")
        );

        let again = harness.adapter.build_tx(&request(PEER, native(), 2_000)).await.unwrap();
        assert_eq!(
            detail(&again.preview.details, "Compute unit limit"),
            Some(SIMULATED_LIMIT.to_string())
        );
        assert_eq!(
            detail(&again.preview.details, "Compute unit limit source").as_deref(),
            Some("Measured earlier this session")
        );
        assert_eq!(harness.node.count(Method::SimulateTransaction), 1);
    }
}

mod token_transfers {
    use super::*;

    #[tokio::test]
    async fn tokens_build_sign_and_price_new_or_existing_accounts() {
        for token in [TokenId::UsdgSpl, TokenId::UsdsSpl, TokenId::PyusdSpl, TokenId::PaxgSpl] {
            for should_create in [false, true] {
                let harness = Harness::priced().await.unwrap();
                common::stock_token(&harness.node, token, 10_000_000, should_create).unwrap();
                let asset = AssetId::Token(token);
                let request = request(PEER, asset, 2_500_000);
                let unsigned = harness.adapter.build_tx(&request).await.unwrap();
                assert_eq!(unsigned.preview.asset, asset);
                assert_eq!(unsigned.preview.amount.base_units(), 2_500_000);
                let rent = detail(&unsigned.preview.details, "Token account rent");
                if should_create {
                    let expected = if token == TokenId::UsdsSpl { "2039280" } else { "2192400" };
                    assert!(rent.unwrap().contains(expected));
                } else {
                    assert!(rent.is_none());
                }
                let tx = signed(&harness, &request).await.unwrap();
                let instructions = common::instructions(&tx);
                let (program, data) = instructions.last().unwrap();
                if token == TokenId::UsdsSpl {
                    assert_eq!(program, common::TOKEN_PROGRAM);
                    assert_eq!(data, "0ca02526000000000006");
                } else {
                    assert_eq!(*program, spl_token_2022_interface::ID.to_string());
                    assert_eq!(data, "1a01a025260000000000060000000000000000");
                }
            }
        }
    }

    #[tokio::test]
    async fn paxg_send_max_uses_its_entire_balance_and_rechecks_the_mint() {
        let harness = Harness::priced().await.unwrap();
        let token = TokenId::PaxgSpl;
        common::stock_token(&harness.node, token, 1_234_567, true).unwrap();
        let request = send_max(PEER, AssetId::Token(token));
        let unsigned = harness.adapter.build_tx(&request).await.unwrap();
        assert_eq!(unsigned.preview.amount.base_units(), 1_234_567);
        assert!(
            detail(&unsigned.preview.details, "Token account rent").unwrap().contains("2192400")
        );
        let tx = signed(&harness, &request).await.unwrap();
        let instructions = common::instructions(&tx);
        let (program, data) = instructions.last().unwrap();
        assert_eq!(*program, spl_token_2022_interface::ID.to_string());
        assert_eq!(data, "1a0187d6120000000000060000000000000000");
        harness.node.at(Method::GetAccountInfo, token.contract(), Answer::ok(&missing_account()));
        assert!(matches!(
            harness.adapter.build_tx(&request).await,
            Err(ChainError::Unsupported(_))
        ));
        assert!(matches!(
            harness.adapter.quote_fee(&request).await,
            Err(ChainError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn usdg_rechecks_its_mint_after_a_successful_cached_build() {
        let harness = Harness::priced().await.unwrap();
        common::stock_token(&harness.node, TokenId::UsdgSpl, 10_000_000, false).unwrap();
        let request = request(PEER, AssetId::Token(TokenId::UsdgSpl), 1_000_000);
        harness.adapter.build_tx(&request).await.unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            TokenId::UsdgSpl.contract(),
            Answer::ok(&missing_account()),
        );
        assert!(matches!(
            harness.adapter.build_tx(&request).await,
            Err(ChainError::Unsupported(_))
        ));
        assert!(matches!(
            harness.adapter.quote_fee(&request).await,
            Err(ChainError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn usdg_requires_sol_for_the_larger_account_even_when_sending_all_tokens() {
        let harness = Harness::priced().await.unwrap();
        common::stock_token(&harness.node, TokenId::UsdgSpl, 10_000_000, true).unwrap();
        harness.node.at(
            Method::GetBalance,
            OWNER,
            Answer::ok(&common::contextual(&serde_json::json!(2_100_000u64))),
        );
        let error = harness
            .adapter
            .build_tx(&send_max(PEER, AssetId::Token(TokenId::UsdgSpl)))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ChainError::InsufficientFunds { asset: AssetId::Native(ChainId::Solana), .. }
        ));
    }

    #[tokio::test]
    async fn a_classic_token_measurement_is_not_reused_for_usdg() {
        let harness = Harness::priced().await.unwrap();
        common::stock_token(&harness.node, TokenId::UsdsSpl, 10_000_000, false).unwrap();
        harness
            .adapter
            .build_tx(&request(PEER, AssetId::Token(TokenId::UsdsSpl), 1_000_000))
            .await
            .unwrap();
        common::stock_token(&harness.node, TokenId::UsdgSpl, 10_000_000, false).unwrap();
        harness.node.always(Method::SimulateTransaction, Answer::ok(&common::simulation(30_000)));
        let unsigned = harness
            .adapter
            .build_tx(&request(PEER, AssetId::Token(TokenId::UsdgSpl), 1_000_000))
            .await
            .unwrap();
        assert_eq!(harness.node.count(Method::SimulateTransaction), 2);
        assert_eq!(
            detail(&unsigned.preview.details, "Compute unit limit").as_deref(),
            Some("36000")
        );
    }

    #[tokio::test]
    async fn a_transfer_to_an_existing_account_does_not_create_one() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
        assert_eq!(unsigned.preview.asset, usdc());
        assert!(!warns(&unsigned.preview.warnings, TxWarningKind::AccountActivation));
        assert!(detail(&unsigned.preview.details, "Token account rent").is_none());
        assert_eq!(
            detail(&unsigned.preview.details, "Destination token account"),
            Some(token_account_address(PEER, TokenId::UsdcSpl).unwrap())
        );
    }

    #[tokio::test]
    async fn a_transfer_to_a_new_account_creates_it_and_prices_the_deposit() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::AccountActivation));
        let rent = detail(&unsigned.preview.details, "Token account rent").unwrap();
        assert!(rent.contains("2039280"), "{rent}");
        assert!(
            detail(&unsigned.preview.details, "Destination token account")
                .unwrap()
                .contains("created here")
        );
    }

    #[tokio::test]
    async fn a_destination_that_only_holds_lamports_is_still_created() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(PEER, TokenId::UsdcSpl).unwrap(),
            Answer::ok(&system_account()),
        );

        let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
        assert!(warns(&unsigned.preview.warnings, TxWarningKind::AccountActivation));
        assert!(
            detail(&unsigned.preview.details, "Destination token account")
                .unwrap()
                .contains("created here")
        );

        let transaction = signed(&harness, &request(PEER, usdc(), 1_000_000)).await.unwrap();
        assert_eq!(
            common::instructions(&transaction).len(),
            4,
            "limit, price, create, transferChecked"
        );
    }

    /// Pasting a token account's own address names the account to credit, so the
    /// tokens go straight into it rather than into a new account that token
    /// account would hold the authority over.
    #[tokio::test]
    async fn a_recipient_that_is_itself_a_token_account_of_the_mint_is_credited_directly() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            PEER,
            Answer::ok(&common::token_account(common::RECORDED, common::USDC_MINT, 7).unwrap()),
        );

        let request = request(PEER, usdc(), 1_000_000);
        let unsigned = harness.adapter.build_tx(&request).await.unwrap();
        assert_eq!(
            detail(&unsigned.preview.details, "Destination token account").as_deref(),
            Some(PEER)
        );
        assert!(
            detail(&unsigned.preview.details, "Token account rent").is_none(),
            "an account that exists is never created"
        );
        assert!(!warns(&unsigned.preview.warnings, TxWarningKind::AmbiguousAddress));

        let transaction = signed(&harness, &request).await.unwrap();
        let instructions = common::instructions(&transaction);
        assert!(
            instructions.iter().all(|(program, _)| program != common::ASSOCIATED_TOKEN_PROGRAM),
            "no associated account is derived from a token account: {instructions:?}"
        );
        let transfer = instructions.len() - 1;
        assert_eq!(
            common::accounts_of(&transaction, transfer).get(2).map(String::as_str),
            Some(PEER)
        );
    }

    /// A classic token account's owner can be reassigned, so the account at the
    /// recipient's associated address can belong to someone else, and a transfer
    /// into it would credit them.
    #[tokio::test]
    async fn an_associated_account_the_recipient_does_not_own_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(PEER, TokenId::UsdcSpl).unwrap(),
            Answer::ok(&common::token_account(common::RECORDED, common::USDC_MINT, 0).unwrap()),
        );

        let error = harness
            .adapter
            .build_tx(&request(PEER, usdc(), 1_000_000))
            .await
            .expect_err("it would credit someone else");
        assert!(
            matches!(error, ChainError::InvalidInput(ref text, _) if text.contains("belongs to another address")),
            "{error:?}"
        );
    }

    /// The copy a build refuses `to` with, which a quote has to refuse too.
    async fn refusal(harness: &Harness, to: &str, asset: AssetId) -> Fallible<String> {
        let built = harness.adapter.build_tx(&request(to, asset, 1_000)).await;
        let Err(ChainError::InvalidInput(text, _)) = built else {
            return Err(format!("a refusal the interface shows as written: {built:?}").into());
        };
        let quoted = harness.adapter.quote_fee(&request(to, asset, 1_000)).await;
        if quoted.is_ok() {
            return Err(format!("a quote refuses it too: {quoted:?}").into());
        }
        Ok(text)
    }

    #[tokio::test]
    async fn a_frozen_source_account_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        let frozen = TokenState { is_frozen: true, ..TokenState::default() };
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(OWNER, TokenId::UsdcSpl).unwrap(),
            Answer::ok(
                &common::token_account_in(OWNER, common::USDC_MINT, 50_000_000, frozen).unwrap(),
            ),
        );
        assert_eq!(
            refusal(&harness, PEER, usdc()).await.unwrap(),
            "Your USDC token account is frozen, so it cannot send. Only the token's issuer can \
             unfreeze it."
        );
    }

    #[tokio::test]
    async fn a_frozen_recipient_account_is_refused_whether_associated_or_named() {
        let frozen = TokenState { is_frozen: true, ..TokenState::default() };
        let associated = token_account_address(PEER, TokenId::UsdcSpl).unwrap();
        for (address, holder) in [(associated.as_str(), PEER), (PEER, common::RECORDED)] {
            let harness = Harness::priced().await.unwrap();
            stock_usdc(&harness.node, 50_000_000, false).unwrap();
            harness.node.at(
                Method::GetAccountInfo,
                address,
                Answer::ok(
                    &common::token_account_in(holder, common::USDC_MINT, 0, frozen).unwrap(),
                ),
            );
            assert_eq!(
                refusal(&harness, PEER, usdc()).await.unwrap(),
                "The recipient's USDC token account is frozen, so it cannot receive tokens.",
                "{address}"
            );
        }
    }

    #[tokio::test]
    async fn a_recipient_account_that_requires_a_memo_is_refused_because_jelly_cannot_attach_one() {
        let harness = Harness::priced().await.unwrap();
        let token = TokenId::UsdgSpl;
        common::stock_token(&harness.node, token, 10_000_000, false).unwrap();
        let memo = TokenState { is_memo_required: true, ..TokenState::default() };
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(PEER, token).unwrap(),
            Answer::ok(&common::token_account_in(PEER, token.contract(), 0, memo).unwrap()),
        );
        assert_eq!(
            refusal(&harness, PEER, AssetId::Token(token)).await.unwrap(),
            "The recipient's USDG token account only accepts transfers that carry a memo, and \
             Jelly can't attach a memo."
        );
    }

    #[tokio::test]
    async fn a_recipient_token_account_of_another_token_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        for mint in [TokenId::UsdtSpl.contract(), TokenId::PyusdSpl.contract()] {
            harness.node.at(
                Method::GetAccountInfo,
                PEER,
                Answer::ok(&common::token_account(common::RECORDED, mint, 0).unwrap()),
            );
            assert_eq!(
                refusal(&harness, PEER, usdc()).await.unwrap(),
                "This address is a token account for a different token, not USDC.",
                "{mint}"
            );
        }
    }

    /// Tokens sent to an address with no private key would land in an account
    /// that only the program behind it could move.
    #[tokio::test]
    async fn an_off_curve_recipient_that_is_not_a_token_account_is_refused() {
        for account in [system_account(), missing_account()] {
            let harness = Harness::priced().await.unwrap();
            stock_usdc(&harness.node, 50_000_000, true).unwrap();
            harness.node.at(Method::GetAccountInfo, common::DERIVED, Answer::ok(&account));
            assert_eq!(
                refusal(&harness, common::DERIVED, usdc()).await.unwrap(),
                "This address belongs to a program, not a wallet. Paste the recipient's USDC \
                 token account address instead."
            );
        }
    }

    #[tokio::test]
    async fn a_mint_named_as_the_recipient_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        let mint = common::contextual(&serde_json::json!({
            "owner": common::TOKEN_PROGRAM,
            "data": [BASE64.encode([0u8; 82]), "base64"],
        }));
        harness.node.at(Method::GetAccountInfo, common::USDC_MINT, Answer::ok(&mint));
        let text = refusal(&harness, common::USDC_MINT, usdc()).await.unwrap();
        assert!(text.contains("not a wallet or a token account"), "{text}");
    }

    #[tokio::test]
    async fn the_rent_deposit_is_never_folded_into_the_fee() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        let unsigned = harness.adapter.build_tx(&request(PEER, usdc(), 1_000_000)).await.unwrap();
        assert!(unsigned.preview.fee.base_units() < 2_039_280, "{}", unsigned.preview.fee);
        let debited = detail(&unsigned.preview.details, "Total SOL debited").unwrap();
        let fee = u64::try_from(unsigned.preview.fee.base_units()).unwrap();
        assert!(
            debited.contains(&(2_039_280 + fee).to_string()),
            "the deposit leaves the wallet on top of the fee: {debited}"
        );
    }

    #[tokio::test]
    async fn send_max_moves_the_whole_spendable_balance() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        let unsigned = harness.adapter.build_tx(&send_max(PEER, usdc())).await.unwrap();
        assert_eq!(unsigned.preview.amount, Amount::from_base_units(50_000_000));
        let priority = unsigned.preview.fee.base_units() - u128::from(common::BASE_FEE_LAMPORTS);
        assert!(priority > 0, "a fee level always carries the floor price");
    }

    /// The associated account is read by address, so an owner who never held the
    /// token has no account and nothing to send from it.
    #[tokio::test]
    async fn a_token_with_no_associated_account_is_refused_as_insufficient() {
        let harness = Harness::priced().await.unwrap();
        let error =
            harness.adapter.build_tx(&request(PEER, usdc(), 1_000)).await.expect_err("none held");
        let ChainError::InsufficientFunds { asset, available, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(asset, usdc());
        assert_eq!(available, Amount::ZERO);
        let associated = token_account_address(OWNER, TokenId::UsdcSpl).unwrap();
        assert!(
            harness
                .node
                .calls_to(Method::GetAccountInfo)
                .iter()
                .any(|call| call.contains(&associated)),
            "the associated account was read by address"
        );
    }

    #[tokio::test]
    async fn more_tokens_than_the_spendable_account_holds_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, false).unwrap();
        let error = harness
            .adapter
            .build_tx(&request(PEER, usdc(), 60_000_000))
            .await
            .expect_err("too many");
        let ChainError::InsufficientFunds { asset, available, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(asset, usdc());
        assert_eq!(available, Amount::from_base_units(50_000_000));
    }

    #[tokio::test]
    async fn a_token_transfer_with_no_lamports_for_the_deposit_is_refused() {
        let harness = Harness::priced().await.unwrap();
        stock_usdc(&harness.node, 50_000_000, true).unwrap();
        harness.node.at(
            Method::GetBalance,
            OWNER,
            Answer::ok(&common::contextual(&serde_json::json!(1u64))),
        );
        let error =
            harness.adapter.build_tx(&request(PEER, usdc(), 1_000)).await.expect_err("no lamports");
        let ChainError::InsufficientFunds { asset, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(asset, native());
    }
}

mod watch_set {
    use super::*;

    #[tokio::test]
    async fn an_address_from_another_chain_is_refused() {
        let harness = Harness::new();
        let foreign = WatchedAddressBuilder::new(
            1,
            ChainId::Ethereum,
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        )
        .build();
        let error = harness.watch(&[foreign]).await.expect_err("another chain");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error}");
    }

    #[tokio::test]
    async fn one_unreadable_row_refuses_the_whole_set() {
        let harness = Harness::new();
        let error = harness
            .watch(&[watched(1, OWNER), watched(2, "not-an-address")])
            .await
            .expect_err("a malformed address");
        assert!(matches!(error, ChainError::InvalidAddress(_)), "{error}");
    }
}

mod construction {
    use super::*;

    #[tokio::test]
    async fn a_zeroed_configuration_still_builds_an_adapter_that_works() {
        let config = SolanaConfig {
            signature_page_size: 0,
            page_budget: 0,
            max_batch: 0,
            ..SolanaConfig::default()
        };
        let harness = Harness::with(config);
        harness.node.priced().unwrap();
        harness.watch_owner().await.unwrap();
        harness.adapter.build_tx(&request(PEER, native(), 1_000)).await.unwrap();
    }

    #[tokio::test]
    async fn a_narrow_batch_width_reads_the_watch_set_one_address_at_a_time() {
        let config = SolanaConfig { max_batch: 1, ..SolanaConfig::default() };
        let harness = Harness::with(config);
        harness.node.quiet().unwrap();
        harness.watch(&[watched(7, OWNER), watched(8, common::RECORDED)]).await.unwrap();
        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(
            harness.node.documents(),
            [vec![2, ACCOUNTS_PER_OWNER, ACCOUNTS_PER_OWNER], vec![1; 2 * ACCOUNTS_PER_OWNER]]
                .concat(),
            "the tip pair, one document per address, then one signature page for each owner \
             and each of its token accounts"
        );
    }
}
