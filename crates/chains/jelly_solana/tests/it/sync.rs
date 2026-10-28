use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Deadline, TokenId, TxDetection,
    TxDirection, TxOutcome,
};
use jelly_solana::{Method, SolanaConfig};
use serde_json::json;

use crate::common::{
    self, ACCOUNTS_PER_OWNER, Answer, Harness, OWNER, PEER, RECORDED, TX_NATIVE_TRANSFER,
    TX_SPL_TRANSFER, TX_USDC_TRANSFER, TX_USDT_TRANSFER_NEW_ACCOUNT, native, token_account_address,
    usdc, watched,
};

fn paged() -> SolanaConfig {
    SolanaConfig { signature_page_size: 4, page_budget: 2, ..SolanaConfig::default() }
}

mod balances {
    use super::*;

    #[tokio::test]
    async fn a_pass_reports_every_asset_of_every_address() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.watch(&[watched(7, OWNER), watched(8, RECORDED)]).await.unwrap();
        let progress = harness.sync().await.unwrap();

        assert_eq!(progress.chain(), ChainId::Solana);
        assert_eq!(progress.tip_height(), Some(common::SLOT));
        assert_eq!(progress.progress_permille(), 1_000);
        assert_eq!(progress.synced_height(), Some(common::SLOT));

        let balances = harness.balances();
        assert_eq!(
            balances.len(),
            2 * ACCOUNTS_PER_OWNER,
            "two addresses, every asset each: {balances:?}"
        );
        for id in [7i64, 8] {
            assert!(balances.contains(&(id, native(), Amount::from(common::BALANCE_LAMPORTS))));
            assert!(balances.contains(&(id, usdc(), Amount::ZERO)));
            assert!(balances.contains(&(id, AssetId::Token(TokenId::UsdtSpl), Amount::ZERO)));
        }
    }

    #[tokio::test]
    async fn a_pass_over_two_addresses_costs_three_documents() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.watch(&[watched(7, OWNER), watched(8, RECORDED)]).await.unwrap();
        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(
            harness.node.documents(),
            vec![2, 2 * ACCOUNTS_PER_OWNER, 2 * ACCOUNTS_PER_OWNER],
            "the tip, a balance read for each owner and each of its token accounts, then one \
             signature page for each"
        );
    }

    #[tokio::test]
    async fn a_recorded_balance_reads_back_exactly() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.at(Method::GetBalance, OWNER, Answer::raw(common::BALANCE_BODY));
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();
        assert!(
            harness.balances().contains(&(
                7,
                native(),
                Amount::from_base_units(9_755_444_179_483_239)
            )),
            "the recorded figure"
        );
    }

    #[tokio::test]
    async fn a_token_balance_is_read_from_the_associated_account() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(RECORDED, TokenId::UsdcSpl).unwrap(),
            Answer::raw(common::TOKEN_ACCOUNT_USDC),
        );
        harness.watch(&[watched(8, RECORDED)]).await.unwrap();
        harness.sync().await.unwrap();
        assert!(
            harness.balances().contains(&(8, usdc(), Amount::from_base_units(1_095_076_785))),
            "the recorded figure"
        );
    }

    #[tokio::test]
    async fn a_paxg_balance_is_read_from_its_token_2022_account() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        let token = TokenId::PaxgSpl;
        let account = token_account_address(OWNER, token).unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &account,
            Answer::ok(&common::token_account(OWNER, token.contract(), 1_234_567).unwrap()),
        );
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();
        assert!(harness.balances().contains(&(
            7,
            AssetId::Token(token),
            Amount::from_base_units(1_234_567),
        )));
    }

    /// Anyone can fund an owner's uncreated associated token address with lamports,
    /// which leaves a system account there that holds no tokens.
    #[tokio::test]
    async fn a_lamport_funded_token_account_address_reads_as_no_tokens() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(OWNER, TokenId::UsdcSpl).unwrap(),
            Answer::ok(&common::contextual(&json!({
                "data": ["", "base64"],
                "executable": false,
                "lamports": 890_880u64,
                "owner": common::SYSTEM_PROGRAM,
                "rentEpoch": u64::MAX,
                "space": 0
            }))),
        );
        harness.watch_owner().await.unwrap();

        harness.sync().await.expect("a pass that reads the balance sheet");
        assert!(harness.balances().contains(&(7, usdc(), Amount::ZERO)));
    }

    /// A classic token account's owner can be reassigned, after which the account
    /// at the owner's associated address is no longer the owner's to spend from.
    #[tokio::test]
    async fn an_associated_account_reassigned_to_another_owner_holds_none_of_the_owners_tokens() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.at(
            Method::GetAccountInfo,
            &token_account_address(OWNER, TokenId::UsdcSpl).unwrap(),
            Answer::ok(&common::token_account(RECORDED, common::USDC_MINT, 5_000_000).unwrap()),
        );
        harness.watch_owner().await.unwrap();

        harness.sync().await.expect("a pass that reads the balance sheet");
        let reported: Vec<Amount> = harness
            .balances()
            .into_iter()
            .filter(|(id, asset, _)| *id == 7 && *asset == usdc())
            .map(|(_, _, balance)| balance)
            .collect();
        assert_eq!(
            reported,
            vec![Amount::ZERO],
            "the account's tokens belong to {RECORDED}, its owner now"
        );
    }

    #[tokio::test]
    async fn an_empty_watch_set_is_a_complete_pass_that_reads_only_the_tip() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        let progress = harness.sync().await.unwrap();
        assert_eq!(progress.progress_permille(), 1_000);
        assert!(harness.drain().is_empty());
        assert_eq!(harness.node.documents(), vec![2]);
    }
}

mod records {
    use super::*;

    #[tokio::test]
    async fn a_recorded_native_transfer_becomes_one_outgoing_record() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(RECORDED, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch(&[watched(8, RECORDED)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("one record");
        assert_eq!(record.asset, native());
        assert_eq!(record.address_id, 8);
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.outcome, TxOutcome::Success);
        assert_eq!(record.detection, TxDetection::Full);
        assert_eq!(record.amount, Amount::from_base_units(1_000_000_000_000_000));
        assert_eq!(record.fee, Some(Amount::from_base_units(11_360)));
        assert_eq!(record.counterparty.as_deref(), Some(PEER));
        assert_eq!(record.height, Some(438_915_421));
    }

    #[tokio::test]
    async fn the_recipient_of_a_recorded_transfer_sees_it_as_incoming() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(PEER, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch(&[watched(9, PEER)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("one record");
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount, Amount::from_base_units(1_000_000_000_000_000));
        assert_eq!(record.fee, None, "only the fee payer paid it");
    }

    #[tokio::test]
    async fn a_recorded_usdc_transfer_becomes_one_token_record() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(PEER, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_USDC_TRANSFER));
        harness.watch(&[watched(9, PEER)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("one record");
        assert_eq!(record.asset, usdc());
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.detection, TxDetection::Full);
        assert_eq!(record.amount, Amount::from_base_units(17_313_300));
    }

    #[tokio::test]
    async fn a_transfer_of_an_uncarried_token_leaves_only_the_fee() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(RECORDED, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_SPL_TRANSFER));
        harness.watch(&[watched(8, RECORDED)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("one record");
        assert_eq!(record.asset, native());
        assert_eq!(record.amount, Amount::ZERO);
        assert_eq!(record.fee, Some(Amount::from_base_units(5_333)));
        assert_eq!(record.detection, TxDetection::BalanceDelta);
    }

    #[tokio::test]
    async fn a_token_send_that_opens_the_recipients_account_is_one_record_with_its_rent() {
        const SENDER: &str = "AvxmrCrmPxWVeQ7DWUmc6WsJnSU6CMzZsDNevxuMxnKX";
        const RECIPIENT: &str = "2ARtzy35wXV1nvsoWqb5YHqYiYxrSLiZnfZ56q4Qf6CM";
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.always(Method::GetAccountInfo, Answer::ok(&common::missing_account()));
        harness.node.seed_signatures(SENDER, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_USDT_TRANSFER_NEW_ACCOUNT));
        harness.watch(&[watched(8, SENDER), watched(9, RECIPIENT)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 2, "a USDT record for each side and no SOL record: {records:?}");
        let usdt = AssetId::Token(TokenId::UsdtSpl);
        let sent = records.iter().find(|record| record.address_id == 8).expect("the send");
        assert_eq!((sent.asset, sent.direction), (usdt, TxDirection::Outgoing));
        assert_eq!(sent.amount, Amount::from_base_units(1_131_686));
        assert_eq!(sent.counterparty.as_deref(), Some(RECIPIENT));
        assert_eq!(sent.detection, TxDetection::Full);
        assert_eq!(sent.fee, Some(Amount::from_base_units(10_013)));
        assert_eq!(sent.rent, Some(Amount::from_base_units(1_488_440)));
        let received = records.iter().find(|record| record.address_id == 9).expect("the receipt");
        assert_eq!((received.asset, received.direction), (usdt, TxDirection::Incoming));
        assert_eq!(received.amount, Amount::from_base_units(1_131_686));
        assert_eq!(received.fee, None, "the sender paid the fee");
        assert_eq!(received.rent, Some(Amount::ZERO), "and the rent");
    }

    #[tokio::test]
    async fn one_transaction_is_fetched_once_and_reported_for_every_side() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(RECORDED, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch(&[watched(8, RECORDED), watched(9, PEER)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 2, "{records:?}");
        assert_eq!(harness.node.count(Method::GetTransaction), 1, "fetched once");
        let directions: Vec<TxDirection> = records.iter().map(|record| record.direction).collect();
        assert!(directions.contains(&TxDirection::Outgoing), "{directions:?}");
        assert!(directions.contains(&TxDirection::Incoming), "{directions:?}");
    }

    /// A node indexes a signature under the account keys its transaction names. A
    /// transfer into an existing token account names that account and never its
    /// owner, so only the token account's history lists it.
    #[tokio::test]
    async fn an_incoming_token_transfer_naming_only_the_token_account_is_recorded() {
        let destination = token_account_address(PEER, TokenId::UsdcSpl).unwrap();
        let transaction =
            TX_USDC_TRANSFER.replace("2ocS3orPq3jyszjsJ4NozKWyhdotr3csDjAizmkj65aH", &destination);
        assert!(!transaction.contains(&format!(r#""pubkey": "{PEER}""#)), "the owner is no key");
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(&destination, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(&transaction));
        harness.watch(&[watched(9, PEER)]).await.unwrap();

        harness.sync().await.unwrap();
        let records = harness.records();
        assert!(
            records
                .iter()
                .any(|record| record.asset == usdc() && record.direction == TxDirection::Incoming),
            "{records:?}"
        );
    }

    #[tokio::test]
    async fn incoming_paxg_history_is_found_through_its_associated_token_account() {
        let token = TokenId::PaxgSpl;
        let destination = token_account_address(PEER, token).unwrap();
        let transaction = TX_USDC_TRANSFER
            .replace(TokenId::UsdcSpl.contract(), token.contract())
            .replace(common::TOKEN_PROGRAM, &spl_token_2022_interface::ID.to_string())
            .replace("spl-token", "spl-token-2022")
            .replace("2ocS3orPq3jyszjsJ4NozKWyhdotr3csDjAizmkj65aH", &destination);
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(&destination, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(&transaction));
        harness.watch(&[watched(9, PEER)]).await.unwrap();
        harness.sync().await.unwrap();
        let records = harness.records();
        assert!(
            records.iter().any(|record| record.asset == AssetId::Token(token)
                && record.direction == TxDirection::Incoming
                && record.detection == TxDetection::Full
                && record.amount == Amount::from_base_units(17_313_300)),
            "{records:?}"
        );
    }

    /// `jsonParsed` renders a memo instruction's `parsed` member as a bare string.
    #[tokio::test]
    async fn a_transaction_carrying_a_memo_is_recorded() {
        let mut transaction: serde_json::Value = serde_json::from_str(TX_NATIVE_TRANSFER).unwrap();
        transaction["transaction"]["message"]["instructions"].as_array_mut().unwrap().push(json!({
            "parsed": "invoice 42",
            "program": "spl-memo",
            "programId": "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr",
            "stackHeight": 1
        }));
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(PEER, 1);
        harness.node.always(Method::GetTransaction, Answer::ok(&transaction));
        harness.watch(&[watched(9, PEER)]).await.unwrap();

        harness.sync().await.expect("a pass over a transaction with a memo");
        let records = harness.records();
        assert!(
            records.iter().any(|record| record.direction == TxDirection::Incoming),
            "{records:?}"
        );
    }

    /// The node answers `null` for metadata lists it did not record, which is how
    /// older transactions arrive.
    #[tokio::test]
    async fn a_transaction_whose_metadata_lists_are_null_is_recorded() {
        let mut transaction: serde_json::Value = serde_json::from_str(TX_NATIVE_TRANSFER).unwrap();
        for list in ["innerInstructions", "preTokenBalances", "postTokenBalances"] {
            transaction["meta"][list] = serde_json::Value::Null;
        }
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(PEER, 1);
        harness.node.always(Method::GetTransaction, Answer::ok(&transaction));
        harness.watch(&[watched(9, PEER)]).await.unwrap();

        harness.sync().await.expect("a pass over a transaction with null metadata lists");
        let records = harness.records();
        assert!(
            records.iter().any(|record| record.direction == TxDirection::Incoming),
            "{records:?}"
        );
    }

    /// A signature listed at the tip cannot have been pruned.
    ///
    /// A `null` for it means the endpoint that answered has not rooted it yet,
    /// which happens when a load-balanced pool lists the signature on one node and
    /// fetches it from another.
    #[tokio::test]
    async fn a_transaction_not_yet_served_at_the_tip_is_recorded_once_an_endpoint_serves_it() {
        let mut transaction: serde_json::Value = serde_json::from_str(TX_NATIVE_TRANSFER).unwrap();
        transaction["slot"] = json!(common::SLOT);
        let signature = transaction["transaction"]["signatures"][0].as_str().unwrap().to_owned();
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.at(
            Method::GetSignaturesForAddress,
            RECORDED,
            Answer::ok(&json!([{
                "signature": signature,
                "slot": common::SLOT,
                "err": null,
                "blockTime": 1_786_581_185i64,
                "confirmationStatus": "finalized",
            }])),
        );
        harness.node.at(Method::GetTransaction, &signature, Answer::ok(&serde_json::Value::Null));
        harness.watch(&[watched(8, RECORDED)]).await.unwrap();
        harness.sync().await.unwrap();

        harness.node.at(Method::GetTransaction, &signature, Answer::ok(&transaction));
        harness.sync().await.unwrap();
        let records = harness.records();
        assert!(
            records.iter().any(|record| record.txid.value() == signature && record.address_id == 8),
            "expected a record for {signature} once an endpoint serves it, got {records:?}"
        );
    }

    #[tokio::test]
    async fn a_pruned_transaction_is_skipped() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 1);
        harness.node.always(Method::GetTransaction, Answer::ok(&json!(serde_json::Value::Null)));
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();
        assert!(harness.records().is_empty());
    }
}

/// Recorded mainnet version 1 transactions. Each carries a `transactionConfig`
/// member, names every account inline, and counts its priority fee in
/// `meta.fee` as a lamport total.
mod version_one {
    use jelly_core::TxRecord;

    use super::*;
    use crate::common::{Fallible, TX_V1_FAILED, TX_V1_NATIVE_TRANSFER, TX_V1_USDC_TRANSFER};

    const NATIVE_SENDER: &str = "4k8mxqdkh8sNK2gt8pR5ofjk3zgVV5x5Uf7zNgVkuVGX";
    const NATIVE_RECIPIENT: &str = "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY";
    /// The token authority of the USDC transfer. It signed the transfer and left
    /// the fee to [`FEE_PAYER`].
    const USDC_SENDER: &str = "7r39Edt7kKogwCrifgKGm9raJ39XTuthdmGxVXv5gRMW";
    /// The transfer names only this owner's token account.
    const USDC_RECIPIENT: &str = "43DbAvKxhXh1oSxkJSqGosNw3HpBnmsWiak6tB5wpecN";
    const USDC_RECIPIENT_ACCOUNT: &str = "ee5rfA35bixErvFunMw5pBym5N7rrQKPCf6LJodNVgg";
    const FEE_PAYER: &str = "JB2qSwkrCUxUuxat5uEKcAJC8hreFJvxgfWQQ8WQqJmi";
    const FAILED_PAYER: &str = "UpDAs5c2KjEFzoX9xEgjvEWfZdgaXJCLj6b9bea3VgF";
    const FAILED_PEER: &str = "HR8y1evkXdkaFzsJTUVpL54FDMRqgTAyDZKSSB5DSESK";

    /// A node that answers every fetch with `transaction`, over addresses none
    /// of which has a token account.
    fn serving(transaction: &str) -> Fallible<Harness> {
        let harness = Harness::with(paged());
        harness.node.quiet()?;
        harness.node.always(Method::GetAccountInfo, Answer::ok(&common::missing_account()));
        harness.node.always(Method::GetTransaction, Answer::raw(transaction));
        Ok(harness)
    }

    /// The one record for `address_id`, or `None` when there is none or more than
    /// one.
    fn only(records: &[TxRecord], address_id: i64) -> Option<&TxRecord> {
        let mut found = records.iter().filter(|record| record.address_id == address_id);
        let record = found.next()?;
        found.next().is_none().then_some(record)
    }

    #[tokio::test]
    async fn a_native_transfer_reads_from_both_sides() {
        let harness = serving(TX_V1_NATIVE_TRANSFER).unwrap();
        harness.node.seed_signatures(NATIVE_SENDER, 1);
        harness.watch(&[watched(7, NATIVE_SENDER), watched(8, NATIVE_RECIPIENT)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 2, "{records:?}");
        let sent = only(&records, 7).unwrap();
        assert_eq!(sent.asset, native());
        assert_eq!(sent.direction, TxDirection::Outgoing);
        assert_eq!(sent.outcome, TxOutcome::Success);
        assert_eq!(sent.detection, TxDetection::Full);
        assert_eq!(sent.amount, Amount::from_base_units(5_065));
        assert_eq!(sent.fee, Some(Amount::from_base_units(5_000)), "no priority fee was set");
        assert_eq!(sent.counterparty.as_deref(), Some(NATIVE_RECIPIENT));
        assert_eq!(sent.height, Some(456_882_235));
        assert_eq!(sent.timestamp, Some(1_791_990_408));

        let received = only(&records, 8).unwrap();
        assert_eq!(received.asset, native());
        assert_eq!(received.direction, TxDirection::Incoming);
        assert_eq!(received.outcome, TxOutcome::Success);
        assert_eq!(received.detection, TxDetection::Full);
        assert_eq!(received.amount, Amount::from_base_units(5_065));
        assert_eq!(received.fee, None, "only the fee payer paid it");
        assert_eq!(received.counterparty.as_deref(), Some(NATIVE_SENDER));
    }

    /// Two signatures at five thousand lamports each, plus a priority fee of 502
    /// lamports, all charged to a fee payer that moved no USDC.
    #[tokio::test]
    async fn a_usdc_transfer_with_a_separate_fee_payer_reads_for_every_party() {
        assert_eq!(
            token_account_address(USDC_RECIPIENT, TokenId::UsdcSpl).unwrap(),
            USDC_RECIPIENT_ACCOUNT,
            "the transfer credits the recipient's associated account"
        );
        let harness = serving(TX_V1_USDC_TRANSFER).unwrap();
        harness.node.seed_signatures(USDC_RECIPIENT_ACCOUNT, 1);
        harness
            .watch(&[watched(7, USDC_SENDER), watched(8, USDC_RECIPIENT), watched(9, FEE_PAYER)])
            .await
            .unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 3, "{records:?}");
        let sent = only(&records, 7).unwrap();
        assert_eq!(sent.asset, usdc());
        assert_eq!(sent.direction, TxDirection::Outgoing);
        assert_eq!(sent.outcome, TxOutcome::Success);
        assert_eq!(sent.detection, TxDetection::Full);
        assert_eq!(sent.amount, Amount::from_base_units(46_743_270));
        assert_eq!(sent.fee, None, "a signer that is not the fee payer paid nothing");
        assert_eq!(sent.counterparty.as_deref(), Some(USDC_RECIPIENT));
        assert_eq!(sent.height, Some(456_882_643));

        let received = only(&records, 8).unwrap();
        assert_eq!(received.asset, usdc());
        assert_eq!(received.direction, TxDirection::Incoming);
        assert_eq!(received.detection, TxDetection::Full);
        assert_eq!(received.amount, Amount::from_base_units(46_743_270));
        assert_eq!(received.fee, None);
        assert_eq!(received.counterparty.as_deref(), Some(USDC_SENDER));

        let paid = only(&records, 9).unwrap();
        assert_eq!(paid.asset, native());
        assert_eq!(paid.direction, TxDirection::Outgoing);
        assert_eq!(paid.outcome, TxOutcome::Success);
        assert_eq!(paid.amount, Amount::ZERO);
        assert_eq!(paid.fee, Some(Amount::from_base_units(10_502)));
    }

    #[tokio::test]
    async fn a_failed_transaction_costs_its_fee_payer_the_fee_with_its_priority_fee() {
        let harness = serving(TX_V1_FAILED).unwrap();
        harness.node.seed_signatures(FAILED_PAYER, 1);
        harness.watch(&[watched(7, FAILED_PAYER), watched(8, FAILED_PEER)]).await.unwrap();
        harness.sync().await.unwrap();

        let records = harness.records();
        assert_eq!(records.len(), 1, "a failed transaction moved nothing of the peer's");
        let paid = only(&records, 7).unwrap();
        assert_eq!(paid.asset, native());
        assert_eq!(paid.outcome, TxOutcome::Failed);
        assert_eq!(paid.amount, Amount::ZERO);
        assert_eq!(paid.fee, Some(Amount::from_base_units(5_010)));
        assert_eq!(paid.height, Some(456_882_036));
    }

    /// A node refuses a transaction newer than the version cap with `-32015`.
    /// The pass fails on it rather than skip it or read it in a shape it was
    /// never checked against, and the next pass asks again.
    #[tokio::test]
    async fn a_transaction_newer_than_the_cap_fails_the_pass_and_is_asked_for_again() {
        let newer = TX_V1_NATIVE_TRANSFER.replace(r#""version": 1"#, r#""version": 2"#);
        assert_ne!(newer, TX_V1_NATIVE_TRANSFER, "the fixture names its version");
        let harness = serving(&newer).unwrap();
        harness.node.seed_signatures(NATIVE_SENDER, 1);
        harness.watch(&[watched(7, NATIVE_SENDER)]).await.unwrap();

        let error = harness.sync().await.unwrap_err();
        assert!(matches!(error, ChainError::Rpc(_)), "{error}");
        assert!(error.to_string().contains("-32015"), "{error}");
        assert!(harness.records().is_empty());
        assert_eq!(harness.cursors.advance_count(), 0, "the walk did not move past it");

        harness.node.always(Method::GetTransaction, Answer::raw(TX_V1_NATIVE_TRANSFER));
        harness.sync().await.unwrap();
        let records = harness.records();
        assert_eq!(only(&records, 7).unwrap().amount, Amount::from_base_units(5_065));
    }
}

mod walking {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn repeated_passes_cover_a_history_longer_than_one_pass() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 30);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();

        let mut seen: Vec<String> = Vec::new();
        for _ in 0..20u8 {
            let progress = harness.sync().await.unwrap();
            for call in harness.node.calls_to(Method::GetTransaction) {
                if let Some(signature) = call.first() {
                    seen.push(signature);
                }
            }
            harness.node.forget_calls();
            harness.drain();
            if progress.progress_permille() == 1_000 {
                break;
            }
        }
        assert_eq!(seen.len(), 30, "no signature is fetched twice: {seen:?}");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 30, "every signature exactly once: {seen:?}");
    }

    /// A new wallet's first pass finds no history, and what arrives afterwards
    /// must still be walked.
    #[tokio::test]
    async fn an_address_with_no_history_at_its_first_pass_records_what_arrives_later() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch(&[watched(9, PEER)]).await.unwrap();
        harness.sync().await.unwrap();
        harness.drain();

        harness.node.seed_signatures(PEER, 1);
        harness.sync().await.unwrap();
        let records = harness.records();
        assert!(
            records.iter().any(|record| record.direction == TxDirection::Incoming),
            "the transfer that arrived after the first pass: {records:?}"
        );
    }

    #[tokio::test]
    async fn a_saved_frontier_missing_from_the_node_does_not_block_history() {
        let held = token_account_address(OWNER, TokenId::UsdcSpl).unwrap();
        for account in [OWNER.to_owned(), held.clone()] {
            let original = Harness::with(paged());
            original.node.quiet().unwrap();
            original.node.seed_signatures(&account, 2);
            original.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
            original.watch_owner().await.unwrap();
            original.sync().await.unwrap();

            let restarted = Harness::over(paged(), Arc::clone(&original.cursors));
            restarted.node.quiet().unwrap();
            restarted.node.at(
                Method::GetAccountInfo,
                &held,
                Answer::ok(&common::token_account(OWNER, common::USDC_MINT, 1).unwrap()),
            );
            restarted.node.prepend_signatures(&account, 11);
            restarted.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
            restarted.watch_owner().await.unwrap();

            let first =
                restarted.sync().await.expect("the missing frontier does not fail the pass");
            assert_eq!(first.synced_height(), None, "catch-up exceeds the page budget");
            let last = restarted.sync().await.expect("catch-up resumes across passes");
            assert_eq!(last.synced_height(), Some(common::SLOT));
            let fetched: Vec<String> = restarted
                .node
                .calls_to(Method::GetTransaction)
                .into_iter()
                .filter_map(|call| call.first())
                .collect();
            let expected: Vec<String> = (0..11).map(|index| format!("new{index:05}")).collect();
            assert_eq!(fetched, expected);
            for call in restarted.node.calls_to(Method::GetSignaturesForAddress) {
                assert!(!call.contains("until"), "{}", call.params);
            }

            restarted.node.forget_calls();
            let writes = restarted.cursors.advance_count();
            restarted.sync().await.unwrap();
            assert_eq!(restarted.node.count(Method::GetTransaction), 0);
            assert_eq!(restarted.cursors.advance_count(), writes);
        }
    }

    #[tokio::test]
    async fn local_frontiers_stop_at_page_boundaries_across_passes() {
        for count in [3, 4, 8, 11] {
            let harness = Harness::with(paged());
            harness.node.quiet().unwrap();
            harness.node.seed_signatures(OWNER, 2);
            harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
            harness.watch_owner().await.unwrap();
            harness.sync().await.unwrap();
            harness.node.forget_calls();
            harness.node.prepend_signatures(OWNER, count);

            let mut progress = harness.sync().await.unwrap();
            for _ in 0..3 {
                if progress.synced_height().is_some() {
                    break;
                }
                progress = harness.sync().await.unwrap();
            }
            assert_eq!(progress.synced_height(), Some(common::SLOT));
            let fetched: Vec<String> = harness
                .node
                .calls_to(Method::GetTransaction)
                .into_iter()
                .filter_map(|call| call.first())
                .collect();
            let expected: Vec<String> = (0..count).map(|index| format!("new{index:05}")).collect();
            assert_eq!(fetched, expected, "only new history is fetched");
            for call in harness.node.calls_to(Method::GetSignaturesForAddress) {
                assert!(!call.contains("until"), "{}", call.params);
            }
        }
    }

    #[tokio::test]
    async fn a_quiet_pass_writes_no_cursor() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 2);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();
        let after_first = harness.cursors.advance_count();
        assert!(after_first > 0, "the first pass established a cursor");

        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(harness.node.count(Method::GetTransaction), 0, "no history is fetched again");
        assert_eq!(
            harness.cursors.advance_count(),
            after_first,
            "nothing changed, nothing written"
        );
    }

    /// The instruction that creates an associated token account names its owner,
    /// so the owner's own walk finds it, and the account's history matters only
    /// once the owner holds it.
    #[tokio::test]
    async fn a_token_account_the_owner_does_not_hold_is_walked_until_read_and_once_held() {
        let account = token_account_address(OWNER, TokenId::UsdcSpl).unwrap();
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();
        let walked = || -> Vec<String> {
            harness
                .node
                .calls_to(Method::GetSignaturesForAddress)
                .into_iter()
                .filter_map(|call| call.first())
                .collect()
        };

        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(walked().len(), ACCOUNTS_PER_OWNER, "the first pass reads every account");

        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(walked(), vec![OWNER.to_owned()], "no token account is held");

        harness.node.at(
            Method::GetAccountInfo,
            &account,
            Answer::ok(&common::token_account(OWNER, common::USDC_MINT, 1).unwrap()),
        );
        harness.node.seed_signatures(&account, 1);
        harness.node.forget_calls();
        harness.sync().await.unwrap();
        assert_eq!(walked(), vec![OWNER.to_owned(), account]);
        assert_eq!(harness.node.count(Method::GetTransaction), 1, "the held account's transfer");
    }

    #[tokio::test]
    async fn a_signature_that_arrives_later_is_picked_up() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 2);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();
        harness.drain();
        harness.node.forget_calls();

        harness.node.prepend_signatures(OWNER, 1);
        harness.sync().await.unwrap();
        let fetched: Vec<String> = harness
            .node
            .calls_to(Method::GetTransaction)
            .into_iter()
            .filter_map(|call| call.first())
            .collect();
        assert_eq!(fetched, vec![String::from("new00000")], "only the new signature");
    }

    /// Public endpoints refuse a batch that carries more than one
    /// `getTransaction`, so every transaction is its own request.
    #[tokio::test]
    async fn every_transaction_is_fetched_in_a_request_of_its_own() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 3);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();
        harness.node.forget_calls();
        harness.sync().await.unwrap();

        assert_eq!(harness.node.count(Method::GetTransaction), 3);
        assert_eq!(
            harness.node.documents(),
            vec![2, ACCOUNTS_PER_OWNER, ACCOUNTS_PER_OWNER, 1, 1, 1],
            "the tip, the balances, one signature page for the owner and each of its token \
             accounts, then one request per transaction"
        );
    }

    #[tokio::test]
    async fn history_is_read_at_finalized_and_state_at_confirmed() {
        let harness = Harness::with(paged());
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 1);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch_owner().await.unwrap();
        harness.sync().await.unwrap();

        for call in harness.node.calls_to(Method::GetSignaturesForAddress) {
            assert!(call.contains("finalized"), "{}", call.params);
        }
        for call in harness.node.calls_to(Method::GetTransaction) {
            assert!(call.contains("finalized"), "{}", call.params);
            let params: serde_json::Value = serde_json::from_str(&call.params).unwrap();
            assert_eq!(
                params[1]["maxSupportedTransactionVersion"],
                json!(1),
                "a version 1 transaction is asked for and a newer one is not: {}",
                call.params
            );
        }
        for call in harness.node.calls_to(Method::GetBalance) {
            assert!(call.contains("confirmed"), "{}", call.params);
        }
    }

    #[tokio::test]
    async fn progress_reports_the_addresses_that_have_finished_their_history() {
        let config =
            SolanaConfig { signature_page_size: 2, page_budget: 1, ..SolanaConfig::default() };
        let harness = Harness::with(config);
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 10);
        harness.node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        harness.watch(&[watched(7, OWNER), watched(8, RECORDED)]).await.unwrap();

        let first = harness.sync().await.unwrap();
        assert_eq!(first.progress_permille(), 500, "the address with no history finished at once");
        assert_eq!(first.synced_height(), None, "the other has not, so this wallet is not synced");

        let mut last = first;
        for _ in 0..20u8 {
            last = harness.sync().await.unwrap();
            harness.drain();
            if last.progress_permille() == 1_000 {
                break;
            }
        }
        assert_eq!(last.progress_permille(), 1_000);
        assert_eq!(last.synced_height(), Some(common::SLOT), "now every history is covered");
    }
}

mod budget {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use jelly_core::{
        Clock as _,
        test_util::{FakeClock, FakeEventSink, MemoryCursorStore},
    };
    use jelly_solana::{Answered, BroadcastReply, RpcCall, SolanaAdapter, SolanaError, SolanaNode};
    use tokio::sync::Notify;
    use tokio::task::yield_now;
    use tokio::time::sleep;

    use super::*;
    use crate::common::{FakeNode, Fallible};

    /// Each `getTransaction` takes a second, on the runtime timer and on the
    /// adapter's clock alike, the way a rate-limited public endpoint answers.
    #[derive(Debug)]
    struct SlowTransactions {
        inner: Arc<FakeNode>,
        clock: Arc<FakeClock>,
    }

    impl SlowTransactions {
        async fn wait_if_fetching(&self, call: &RpcCall) {
            if call.method() == Method::GetTransaction {
                sleep(Duration::from_secs(1)).await;
                self.clock.advance(Duration::from_secs(1));
            }
        }
    }

    #[async_trait]
    impl SolanaNode for SlowTransactions {
        async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
            self.wait_if_fetching(call).await;
            self.inner.call(call).await
        }

        async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
            self.inner.batch(calls).await
        }

        async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
            self.inner.broadcast(call).await
        }
    }

    /// An adapter over a fixture wallet whose history of `signatures`
    /// transactions is served one second per fetch.
    fn slow_adapter(
        signatures: usize,
    ) -> Fallible<(SolanaAdapter, Arc<FakeClock>, Arc<MemoryCursorStore>)> {
        let node = FakeNode::new();
        node.quiet()?;
        node.seed_signatures(OWNER, signatures);
        node.always(Method::GetTransaction, Answer::raw(TX_NATIVE_TRANSFER));
        let clock = Arc::new(FakeClock::default());
        let cursors = Arc::new(MemoryCursorStore::new());
        let slow = Arc::new(SlowTransactions { inner: node, clock: Arc::clone(&clock) });
        let adapter = SolanaAdapter::builder(
            slow,
            Arc::<MemoryCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&clock),
        )
        .build();
        Ok((adapter, clock, cursors))
    }

    #[tokio::test(start_paused = true)]
    async fn a_pass_that_runs_out_of_time_still_advances_the_history_walk() {
        let (adapter, clock, cursors) = slow_adapter(400).unwrap();
        adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
        let events = FakeEventSink::new();

        let mut failures = Vec::new();
        for _ in 0..3u8 {
            let deadline = clock.deadline_in(Duration::from_secs(60));
            if let Err(error) = adapter.sync(deadline, &events).await {
                failures.push(error.to_string());
            }
        }
        assert!(cursors.advance_count() > 0, "three passes kept no progress at all: {failures:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_pass_whose_deadline_falls_during_a_fetch_returns_the_walk_it_made() {
        let (adapter, clock, cursors) = slow_adapter(400).unwrap();
        adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
        let events = FakeEventSink::new();

        // The sixty-first fetch is still in flight when the deadline passes.
        let deadline = clock.deadline_in(Duration::from_millis(60_500));
        let progress = adapter
            .sync(deadline, &events)
            .await
            .expect("a pass that ran out of time stops with what it has");
        assert_eq!(progress.synced_height(), None, "the history is not covered yet");
        assert!(cursors.advance_count() > 0, "the walk the pass made is kept for the next one");
    }

    #[tokio::test]
    async fn a_deadline_that_has_already_passed_stops_after_the_balances() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.seed_signatures(OWNER, 4);
        harness.watch_owner().await.unwrap();
        harness.node.forget_calls();
        harness.adapter.sync(Deadline::EXPIRED, &harness.events).await.unwrap();
        assert_eq!(harness.node.count(Method::GetSignaturesForAddress), 0);
        assert!(!harness.balances().is_empty(), "the balance sheet is still reported");
    }

    #[tokio::test]
    async fn a_read_no_endpoint_could_answer_fails_the_pass() {
        let harness = Harness::new();
        harness.node.quiet().unwrap();
        harness.node.always(Method::GetBalance, Answer::Unreachable);
        harness.watch_owner().await.unwrap();
        let error = harness.sync().await.unwrap_err();
        assert!(matches!(error, ChainError::EndpointsDown { .. }), "{error}");
    }

    /// Holds the first balance read it is asked for until released, so a test can
    /// look at the node while that pass is part way through.
    #[derive(Debug)]
    struct HeldBalances {
        inner: Arc<FakeNode>,
        is_armed: AtomicBool,
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl SolanaNode for HeldBalances {
        async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
            self.inner.call(call).await
        }

        async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
            let is_balance_read =
                calls.first().is_some_and(|call| call.method() == Method::GetBalance);
            if is_balance_read && self.is_armed.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.batch(calls).await
        }

        async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
            self.inner.broadcast(call).await
        }
    }

    #[tokio::test]
    async fn a_second_pass_waits_for_the_first_to_finish_before_it_starts() {
        let node = FakeNode::new();
        node.quiet().unwrap();
        let held = Arc::new(HeldBalances {
            inner: Arc::clone(&node),
            is_armed: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let clock = Arc::new(FakeClock::default());
        let adapter = SolanaAdapter::builder(
            Arc::<HeldBalances>::clone(&held),
            Arc::new(MemoryCursorStore::new()),
            Arc::<FakeClock>::clone(&clock),
        )
        .build();
        adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
        let events = FakeEventSink::new();
        let deadline = clock.deadline_in(Duration::from_secs(60));

        let observe = async {
            held.entered.notified().await;
            yield_now().await;
            let tips_while_held = node.count(Method::GetSlot);
            held.release.notify_one();
            tips_while_held
        };
        let (first, second, tips_while_held) =
            tokio::join!(adapter.sync(deadline, &events), adapter.sync(deadline, &events), observe);
        first.unwrap();
        second.unwrap();
        assert_eq!(tips_while_held, 1, "the second pass started while the first was running");
        assert_eq!(node.count(Method::GetSlot), 2, "each call still runs a pass of its own");
    }
}
