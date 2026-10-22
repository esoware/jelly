use jelly_core::{
    ChainAdapter as _, ChainError, RawTx, RejectionKind, RetryPolicy, SignedTx, TxId,
};
use jelly_electrum::{Method, test_util::Answer};
use jelly_utxo::UtxoParams;

use crate::common::{
    BITCOIN, CHAINS, Fallible, Harness, LITECOIN, Setup, Wallet, kind_of, quoted, synced,
};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;

/// A built and signed transaction over a wallet with one confirmed coin.
async fn signed(params: &UtxoParams) -> Fallible<(Harness, Wallet, SignedTx)> {
    let (harness, wallet) = synced(Setup::on(params), kind_of(params), &[FUNDING_SAT]).await?;
    let unsigned = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
        .await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()])?;
    Ok((harness, wallet, signed))
}

fn refuse(harness: &Harness, message: &str) {
    harness
        .chain
        .server
        .always(Method::Broadcast, Answer::Error { code: 1, message: message.to_owned() });
}

#[tokio::test]
async fn a_transaction_the_server_accepts_comes_back_with_its_own_id() {
    for params in CHAINS {
        let (harness, _wallet, tx) = signed(&params).await.unwrap();
        harness.serve(Method::Broadcast, &quoted(tx.txid.value()));

        let txid = harness.adapter.broadcast(&tx).await.unwrap();
        assert_eq!(txid, tx.txid);
        assert_eq!(txid.chain(), params.chain);
        assert_eq!(harness.calls(Method::Broadcast), 1);
    }
}

#[tokio::test]
async fn a_transaction_the_network_already_has_is_a_send_that_landed() {
    for params in CHAINS {
        for message in ["txn-already-known", "Transaction already in block chain"] {
            let (harness, _wallet, tx) = signed(&params).await.unwrap();
            refuse(&harness, message);

            let txid = harness.adapter.broadcast(&tx).await.unwrap();
            assert_eq!(txid, tx.txid, "{}: {message}", params.chain);
            assert_eq!(harness.calls(Method::Broadcast), 1, "a broadcast never walks the rotation");
        }
    }
}

/// Litecoin Core emits Bitcoin Core's reject strings verbatim, so one table
/// classifies both.
#[tokio::test]
async fn cores_reject_strings_are_classified_with_what_may_be_done_next() {
    let cases = [
        ("min relay fee not met, 100 < 141", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
        (
            "insufficient fee, rejecting replacement, new feerate is too low",
            RejectionKind::ReplacementUnderpriced,
            RetryPolicy::DidNotLand,
        ),
        ("bad-txns-inputs-missingorspent", RejectionKind::MissingOrSpent, RetryPolicy::DidNotLand),
        ("dust", RejectionKind::Dust, RetryPolicy::FinalEverywhere),
    ];
    for params in CHAINS {
        for (message, kind, retry) in cases {
            let (harness, _wallet, tx) = signed(&params).await.unwrap();
            refuse(&harness, message);

            let error = harness.adapter.broadcast(&tx).await.expect_err("the send is refused");
            let ChainError::Rejected(rejection) = error else {
                panic!("{message}: {error} is not a classified refusal");
            };
            assert_eq!(rejection.kind, kind, "{}: {message}", params.chain);
            assert_eq!(rejection.retry, retry, "{}: {message}", params.chain);
            assert_eq!(harness.calls(Method::Broadcast), 1, "{message}");
        }
    }
}

#[tokio::test]
async fn an_unrecognised_refusal_is_not_guessed_at() {
    for params in CHAINS {
        let (harness, _wallet, tx) = signed(&params).await.unwrap();
        refuse(&harness, "the server is having a bad day");

        let error = harness.adapter.broadcast(&tx).await.expect_err("the transaction is refused");
        let ChainError::Rejected(rejection) = error else {
            panic!("{error} is not a classified refusal");
        };
        assert_eq!(rejection.kind, RejectionKind::Unclassified);
        assert_eq!(rejection.retry, RetryPolicy::TryAnotherEndpoint);
    }
}

#[tokio::test]
async fn a_server_that_answers_with_a_different_transaction_id_is_refused() {
    for params in CHAINS {
        let (harness, _wallet, tx) = signed(&params).await.unwrap();
        harness.serve(Method::Broadcast, &quoted(&"ab".repeat(32)));
        let error =
            harness.adapter.broadcast(&tx).await.expect_err("the answer names another transaction");
        assert!(matches!(error, ChainError::EndpointsDown { .. }), "{}: {error}", params.chain);
    }
}

#[tokio::test]
async fn a_signed_transaction_from_another_chain_never_reaches_the_network() {
    for (params, other) in [(BITCOIN, LITECOIN), (LITECOIN, BITCOIN)] {
        let (harness, _wallet, tx) = signed(&params).await.unwrap();
        let foreign = SignedTx { txid: TxId::new(other.chain, tx.txid.value()), raw: tx.raw };
        harness.adapter.broadcast(&foreign).await.unwrap_err();
        assert_eq!(harness.calls(Method::Broadcast), 0);
    }
}

#[tokio::test]
async fn bytes_that_are_not_a_transaction_are_refused_as_a_signing_failure() {
    for params in CHAINS {
        let (harness, _wallet, tx) = signed(&params).await.unwrap();
        let broken =
            SignedTx { txid: tx.txid, raw: RawTx::from_bytes(b"not a transaction".to_vec()) };
        let error = harness.adapter.broadcast(&broken).await.expect_err("undecodable bytes");
        assert!(matches!(error, ChainError::Signing(_)), "{error}");
        assert_eq!(harness.calls(Method::Broadcast), 0);
    }
}
