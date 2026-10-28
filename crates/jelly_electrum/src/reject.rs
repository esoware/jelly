use jelly_core::{Rejection, RejectionKind, RetryPolicy};

/// Bitcoin Core's `sendrawtransaction` refusal strings, which Core-derived nodes emit
/// verbatim and Electrum servers relay mostly unchanged.
///
/// Dogecoin Core is Bitcoin Core 0.14 underneath and still refuses by priority and
/// by a full mempool, so those older strings are here too.
///
/// Since Bitcoin Core 0.12, which every chain here builds on, `insufficient fee` only
/// refuses a replacement, because the relay floors have their own reasons. Dogecoin
/// sends that reason bare.
///
/// Bitcoin Core 31's cluster mempool refuses a replacement that doesn't improve the
/// feerate diagram as `replacement-failed`, and a send past its cluster limits as
/// `too-large-cluster` where Core 30 said `too-long-mempool-chain`.
///
/// Matched in order, so a specific reason comes before a general one its message also
/// contains.
const CORE_REFUSALS: &[(&str, RejectionKind, RetryPolicy)] = &[
    ("txn-already-in-mempool", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
    ("txn-already-known", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
    ("transaction already in block chain", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
    (
        "transaction outputs already in utxo set",
        RejectionKind::AlreadyKnown,
        RetryPolicy::AlreadyLanded,
    ),
    ("already have transaction", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
    (
        "not enough additional fees",
        RejectionKind::BelowIncrementalRelayFee,
        RetryPolicy::DidNotLand,
    ),
    ("replacement-failed", RejectionKind::ReplacementUnderpriced, RetryPolicy::DidNotLand),
    ("insufficient fee", RejectionKind::BelowIncrementalRelayFee, RetryPolicy::DidNotLand),
    ("min relay fee not met", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("mempool min fee not met", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("min_relay_fee", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("insufficient priority", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("rate limited free transaction", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("mempool full", RejectionKind::BelowRelayFee, RetryPolicy::DidNotLand),
    ("bad-txns-inputs-missingorspent", RejectionKind::MissingOrSpent, RetryPolicy::DidNotLand),
    ("missing-inputs", RejectionKind::MissingOrSpent, RetryPolicy::DidNotLand),
    ("missing inputs", RejectionKind::MissingOrSpent, RetryPolicy::DidNotLand),
    ("bad-txns-inputs-spent", RejectionKind::MissingOrSpent, RetryPolicy::DidNotLand),
    ("txn-mempool-conflict", RejectionKind::DoubleSpend, RetryPolicy::TryAnotherEndpoint),
    ("replacement-adds-unconfirmed", RejectionKind::DoubleSpend, RetryPolicy::DidNotLand),
    ("too-long-mempool-chain", RejectionKind::TooLongMempoolChain, RetryPolicy::DidNotLand),
    ("too-large-cluster", RejectionKind::TooLongMempoolChain, RetryPolicy::DidNotLand),
    ("dust", RejectionKind::Dust, RetryPolicy::FinalEverywhere),
    ("non-bip68-final", RejectionKind::StaleReference, RetryPolicy::DidNotLand),
    ("bad-txns-nonfinal", RejectionKind::StaleReference, RetryPolicy::DidNotLand),
    ("non-final", RejectionKind::StaleReference, RetryPolicy::DidNotLand),
    (
        "mandatory-script-verify-flag-failed",
        RejectionKind::Unclassified,
        RetryPolicy::FinalEverywhere,
    ),
    ("non-mandatory-script-verify-flag", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    (
        "mempool-script-verify-flag-failed",
        RejectionKind::Unclassified,
        RetryPolicy::FinalEverywhere,
    ),
    ("block-script-verify-flag-failed", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("scriptsig-not-pushonly", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("bad-txns-oversize", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("tx-size", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("bad-txns-in-belowout", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    (
        "bad-txns-premature-spend-of-coinbase",
        RejectionKind::Unclassified,
        RetryPolicy::FinalEverywhere,
    ),
    ("bad-txns-fee-outofrange", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("bad-txns-vout-negative", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("bad-txns-vout-toolarge", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
    ("absurdly-high-fee", RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
    ("max-fee-exceeded", RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
    ("fee exceeds maximum", RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
];

/// The detail for a refusal with no readable reason, since a [`Rejection`]'s detail is
/// never empty.
const UNEXPLAINED: &str = "the node refused the transaction and gave no reason";

/// Unknown text becomes `Unclassified` with `TryAnotherEndpoint`. The caller must
/// check the txid before retrying an ambiguous send.
pub(crate) fn classify_rejection(message: &str) -> Rejection {
    let detail = condense(message);
    if detail.is_empty() {
        return Rejection::new(
            RejectionKind::Unclassified,
            RetryPolicy::TryAnotherEndpoint,
            UNEXPLAINED,
        );
    }
    let lower = detail.to_ascii_lowercase();

    // A relayed replacement refusal names the rule it broke, and only the feerate
    // comparison means the rate itself was too low.
    if lower.contains("rejecting replacement") {
        let is_underpriced = lower.contains("new feerate") || lower.contains("old feerate");
        let kind = if is_underpriced {
            RejectionKind::ReplacementUnderpriced
        } else {
            RejectionKind::BelowIncrementalRelayFee
        };
        return Rejection::new(kind, RetryPolicy::DidNotLand, detail);
    }

    for (needle, kind, retry) in CORE_REFUSALS {
        if lower.contains(needle) {
            return Rejection::new(*kind, *retry, detail);
        }
    }
    Rejection::new(RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint, detail)
}

/// One line, without the rejected transaction hex `ElectrumX` echoes back.
/// Idempotent.
fn condense(message: &str) -> String {
    let trimmed = match message.rsplit_once("\n[") {
        Some((head, tail)) if tail.ends_with(']') => head,
        _ => message,
    };
    trimmed.split_whitespace().collect::<Vec<&str>>().join(" ")
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    const REJECT_MESSAGES: &str = include_str!("../tests/vectors/reject_messages.json");

    #[derive(Deserialize)]
    struct Vector {
        cases: Vec<Case>,
        echoed: Echoed,
    }

    #[derive(Deserialize)]
    struct Case {
        message: String,
        kind: RejectionKind,
        retry: RetryPolicy,
    }

    #[derive(Deserialize)]
    struct Echoed {
        message: String,
        detail: String,
    }

    #[test]
    fn the_echoed_transaction_is_dropped_and_dropping_it_twice_changes_nothing() {
        let raw = "rejected\n\nbad-txns-inputs-missingorspent\n[0200000001aabb]";
        let once = condense(raw);
        assert_eq!(once, "rejected bad-txns-inputs-missingorspent");
        assert_eq!(condense(&once), once);
    }

    #[test]
    fn a_replacement_that_does_not_raise_the_feerate_is_underpriced() {
        let rejection = classify_rejection(
            "insufficient fee, rejecting replacement 00ff; new feerate 1.00 BTC/kvB <= old \
             feerate 1.00 BTC/kvB",
        );
        assert_eq!(rejection.kind, RejectionKind::ReplacementUnderpriced);
        assert_eq!(rejection.retry, RetryPolicy::DidNotLand);
    }

    #[test]
    fn a_replacement_that_does_not_pay_the_increment_is_below_the_incremental_relay_fee() {
        let rejection = classify_rejection(
            "insufficient fee, rejecting replacement 00ff, not enough additional fees to relay; \
             0.00 < 0.00001",
        );
        assert_eq!(rejection.kind, RejectionKind::BelowIncrementalRelayFee);
    }

    #[test]
    fn an_unrecognized_refusal_is_not_guessed_at() {
        let rejection = classify_rejection("the server is having a bad day");
        assert_eq!(rejection.kind, RejectionKind::Unclassified);
        assert_eq!(rejection.retry, RetryPolicy::TryAnotherEndpoint);
        assert_eq!(rejection.detail, "the server is having a bad day");
    }

    #[test]
    fn a_refusal_with_no_words_still_produces_a_sentence() {
        for message in ["", "   ", "\n[0200000001aabb]"] {
            let rejection = classify_rejection(message);
            assert_eq!(rejection.detail, UNEXPLAINED, "{message:?}");
            assert_eq!(rejection.kind, RejectionKind::Unclassified);
            assert_eq!(rejection.retry, RetryPolicy::TryAnotherEndpoint);
        }
    }

    #[test]
    fn every_recorded_refusal_classifies_as_recorded() {
        let vector: Vector = serde_json::from_str(REJECT_MESSAGES).expect("the vector parses");
        for (needle, _, _) in CORE_REFUSALS {
            assert!(
                vector.cases.iter().any(|case| case.message.to_ascii_lowercase().contains(needle)),
                "no recorded refusal carries {needle}"
            );
        }

        for case in &vector.cases {
            let rejection = classify_rejection(&case.message);
            assert_eq!(rejection.kind, case.kind, "{}", case.message);
            assert_eq!(rejection.retry, case.retry, "{}", case.message);
        }
    }

    #[test]
    fn the_echoed_transaction_never_reaches_the_detail() {
        let vector: Vector = serde_json::from_str(REJECT_MESSAGES).expect("the vector parses");
        let rejection = classify_rejection(&vector.echoed.message);
        assert_eq!(rejection.detail, vector.echoed.detail);
        assert_eq!(rejection.kind, RejectionKind::MissingOrSpent);
        assert!(!rejection.detail.contains("0200000001"), "{}", rejection.detail);
    }

    /// Bitcoin Core 28 renamed `ALREADY_IN_CHAIN` to `ALREADY_IN_UTXO_SET`, and
    /// `sendrawtransaction` reports a confirmed transaction with this text.
    #[test]
    fn the_core_28_already_confirmed_text_is_already_landed() {
        for message in [
            "Transaction outputs already in utxo set",
            "the transaction was rejected by network rules.\n\nTransaction outputs already in \
             utxo set\n[0200000001aabb]",
        ] {
            let rejection = classify_rejection(message);
            assert_eq!(rejection.kind, RejectionKind::AlreadyKnown, "{message:?}");
            assert_eq!(rejection.retry, RetryPolicy::AlreadyLanded, "{message:?}");
        }
    }

    /// Bitcoin Core 30 renamed the two script verification reject reasons.
    #[test]
    fn the_core_30_script_verify_failures_are_final_everywhere() {
        for message in [
            "mempool-script-verify-flag-failed (Signature must be zero for failed CHECK(MULTI)SIG \
             operation)",
            "the transaction was rejected by network rules.\n\nblock-script-verify-flag-failed \
             (Script evaluated without error but finished with a false/empty top stack element)",
        ] {
            let rejection = classify_rejection(message);
            assert_eq!(rejection.retry, RetryPolicy::FinalEverywhere, "{message:?}");
        }
    }

    /// Bitcoin Core 31's cluster mempool dropped the old feerate comparison.
    ///
    /// A replacement whose feerate doesn't beat what it would evict now fails the
    /// feerate diagram check under the `replacement-failed` reason.
    #[test]
    fn the_core_31_replacement_failed_reason_is_an_underpriced_replacement() {
        let message = "the transaction was rejected by network rules.\n\nreplacement-failed, \
                       insufficient feerate: does not improve feerate diagram\n[0200000001aabb]";
        let rejection = classify_rejection(message);
        assert_eq!(
            (rejection.kind, rejection.retry),
            (RejectionKind::ReplacementUnderpriced, RetryPolicy::DidNotLand),
            "expected Core 31's refused replacement to be an underpriced replacement that did not \
             land, but it was classified as {} with retry {}",
            rejection.kind.as_str(),
            rejection.retry.as_str()
        );
    }

    /// Dogecoin Core's `sendrawtransaction` reports only the reject code and reason.
    ///
    /// The `rejecting replacement` detail never arrives, and its validation uses the
    /// `insufficient fee` reason for nothing but a refused replacement.
    #[test]
    fn the_dogecoin_insufficient_fee_reason_is_a_refused_replacement() {
        let message = "the transaction was rejected by network rules.\n\n66: insufficient \
                       fee\n[0100000001aabb]";
        let rejection = classify_rejection(message);
        let is_replacement = matches!(
            rejection.kind,
            RejectionKind::ReplacementUnderpriced | RejectionKind::BelowIncrementalRelayFee
        );
        assert!(
            is_replacement && rejection.retry == RetryPolicy::DidNotLand,
            "expected Dogecoin's refused replacement to be a replacement fee refusal that did not \
             land, but it was classified as {} with retry {}",
            rejection.kind.as_str(),
            rejection.retry.as_str()
        );
    }

    /// Bitcoin Core 31 replaced the ancestor and descendant limits with cluster
    /// limits, so a send past them is refused as `too-large-cluster` where Core 30
    /// said `too-long-mempool-chain`.
    #[test]
    fn the_core_31_too_large_cluster_reason_is_a_too_long_mempool_chain() {
        let message =
            "the transaction was rejected by network rules.\n\ntoo-large-cluster\n[0200000001aabb]";
        let rejection = classify_rejection(message);
        assert_eq!(
            (rejection.kind, rejection.retry),
            (RejectionKind::TooLongMempoolChain, RetryPolicy::DidNotLand),
            "expected Core 31's cluster limit refusal to read like Core 30's chain limit refusal, \
             but it was classified as {} with retry {}",
            rejection.kind.as_str(),
            rejection.retry.as_str()
        );
    }
}
