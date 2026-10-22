use jelly_core::{EndpointLabel, Rejection, RejectionKind, RetryPolicy, wire_enum};

use crate::client::SolanaClient;
use crate::error::{PREFLIGHT_FAILED, SIGNATURE_VERIFY_FAILED, SolanaError};
use crate::method::Method;
use crate::rpc::{Envelope, Preflight, TransactionFailure};

wire_enum! {
    /// Why the cluster refused a transaction, as a closed vocabulary.
    ///
    /// The runtime's `TransactionError` grows with every release, so its variant
    /// name is classified once here. An unknown name is
    /// [`BroadcastRefusal::Unclassified`], which is safe because the transaction
    /// was not forwarded.
    pub(crate) enum BroadcastRefusal {
        /// The network already has this transaction, which is a success.
        AlreadyProcessed => "already_processed",
        /// The blockhash is outside the node's window, because it expired or the
        /// node is behind. Either way this attempt did not land.
        BlockhashNotFound => "blockhash_not_found",
        /// An account the transaction names does not exist.
        AccountMissing => "account_missing",
        /// The fee payer cannot cover the fee.
        InsufficientFunds => "insufficient_funds",
        /// An account would be left below the rent-exempt minimum.
        BelowRentMinimum => "below_rent_minimum",
        /// An instruction failed during execution.
        ProgramFailed => "program_failed",
        /// Another transaction holds a lock this one needs, or the runtime
        /// refused this slot's load. The same bytes may land later.
        Contended => "contended",
        /// The transaction as written can never be accepted.
        Malformed => "malformed",
        /// A refusal this build has no name for.
        Unclassified => "unclassified",
    }
}

impl BroadcastRefusal {
    const fn kind(self) -> RejectionKind {
        match self {
            Self::AlreadyProcessed => RejectionKind::AlreadyKnown,
            Self::BlockhashNotFound => RejectionKind::StaleReference,
            Self::AccountMissing => RejectionKind::MissingOrSpent,
            Self::BelowRentMinimum => RejectionKind::Dust,
            Self::InsufficientFunds => RejectionKind::InsufficientFunds,
            Self::ProgramFailed => RejectionKind::Reverted,
            Self::Contended | Self::Malformed | Self::Unclassified => RejectionKind::Unclassified,
        }
    }

    const fn retry(self) -> RetryPolicy {
        match self {
            Self::AlreadyProcessed => RetryPolicy::AlreadyLanded,
            Self::BlockhashNotFound | Self::Contended | Self::Unclassified => {
                RetryPolicy::DidNotLand
            }
            Self::AccountMissing
            | Self::InsufficientFunds
            | Self::BelowRentMinimum
            | Self::ProgramFailed
            | Self::Malformed => RetryPolicy::FinalEverywhere,
        }
    }

    /// Classifies one `TransactionError` variant name. Names the runtime added
    /// since are `Unclassified`.
    fn classify(name: &str) -> Self {
        match name {
            "AlreadyProcessed" | "AlreadyProcessedInBlock" => Self::AlreadyProcessed,
            "BlockhashNotFound" => Self::BlockhashNotFound,
            "AccountNotFound" | "ProgramAccountNotFound" | "AddressLookupTableNotFound" => {
                Self::AccountMissing
            }
            "InsufficientFundsForFee" => Self::InsufficientFunds,
            "InsufficientFundsForRent" | "InvalidRentPayingAccount" => Self::BelowRentMinimum,
            "InstructionError" => Self::ProgramFailed,
            "AccountInUse"
            | "AccountBorrowOutstanding"
            | "ClusterMaintenance"
            | "TooManyAccountLocks"
            | "WouldExceedMaxBlockCostLimit"
            | "WouldExceedMaxAccountCostLimit"
            | "WouldExceedMaxVoteCostLimit"
            | "WouldExceedAccountDataBlockLimit"
            | "WouldExceedAccountDataTotalLimit"
            | "ProgramExecutionTemporarilyRestricted"
            | "ProgramCacheHitMaxLimit"
            | "CommitCancelled" => Self::Contended,
            "AccountLoadedTwice"
            | "CallChainTooDeep"
            | "DuplicateInstruction"
            | "InvalidAccountForFee"
            | "InvalidAccountIndex"
            | "InvalidAddressLookupTableData"
            | "InvalidAddressLookupTableIndex"
            | "InvalidAddressLookupTableOwner"
            | "InvalidLoadedAccountsDataSizeLimit"
            | "InvalidProgramForExecution"
            | "InvalidWritableAccount"
            | "MaxLoadedAccountsDataSizeExceeded"
            | "MissingSignatureForFee"
            | "ResanitizationNeeded"
            | "SanitizeFailure"
            | "SignatureFailure"
            | "UnbalancedTransaction"
            | "UnsupportedVersion" => Self::Malformed,
            _ => Self::Unclassified,
        }
    }

    /// Fallback text for a cluster that refused without a readable message.
    const fn explain(self) -> &'static str {
        match self {
            Self::AlreadyProcessed => "the network already has this transaction",
            Self::BlockhashNotFound => {
                "the blockhash is outside this node's window, so this attempt did not land"
            }
            Self::AccountMissing => "an account this transaction names does not exist",
            Self::InsufficientFunds => "the fee payer cannot cover the fee",
            Self::BelowRentMinimum => {
                "an account would be left below the rent-exempt minimum, which the runtime refuses"
            }
            Self::ProgramFailed => "an instruction failed while it ran",
            Self::Contended => {
                "the runtime could not take this slot's load; the same bytes may land later"
            }
            Self::Malformed => "the transaction as written can never be accepted",
            Self::Unclassified => "the cluster refused the transaction",
        }
    }

    /// The full rejection, with the node's own wording kept for diagnostics and
    /// [`Self::explain`] standing in when the node said nothing.
    fn into_rejection(self, detail: &str) -> Rejection {
        let trimmed = detail.trim();
        let detail =
            if trimmed.is_empty() { self.explain().to_owned() } else { trimmed.to_owned() };
        Rejection::new(self.kind(), self.retry(), detail)
    }
}

/// Turns a refused preflight into the classification the rest of the workspace
/// reads.
fn classify_preflight(message: &str, preflight: Option<&Preflight>) -> Rejection {
    let refusal = refusal_of(preflight.and_then(|data| data.err.as_ref()));
    let logs = preflight.map_or(&[][..], |data| data.logs.as_slice());
    refusal.into_rejection(&detail_line(message, logs))
}

/// The program log usually says more than the code does, so the last line that
/// mentions a failure is appended when there is one.
fn detail_line(message: &str, logs: &[String]) -> String {
    let tail = logs.iter().rev().find(|line| {
        line.contains("Error") || line.contains("failed") || line.contains("insufficient")
    });
    match tail {
        Some(line) => format!("{message}: {line}"),
        None => message.to_owned(),
    }
}

pub(crate) fn refusal_of(failure: Option<&TransactionFailure>) -> BroadcastRefusal {
    failure.map_or(BroadcastRefusal::Unclassified, |error| BroadcastRefusal::classify(error.name()))
}

/// A refusal travels as a value rather than an error, because the node simulated
/// the transaction and every node would say the same, so a rotation must not
/// shop it around.
#[derive(Debug, Clone)]
pub enum BroadcastReply {
    /// The node accepted the bytes and echoed this signature.
    Accepted { endpoint: EndpointLabel, signature: String },
    /// The cluster refused, already classified.
    Refused(Rejection),
}

impl BroadcastReply {
    /// Public so a scripted [`crate::SolanaNode`] classifies a refusal exactly as
    /// the shipped one does.
    ///
    /// A refusal is recognised by a structured `err` in the error `data` or by
    /// Solana's preflight codes, because some gateways strip `data` and leave
    /// only the code and wording.
    pub fn read(endpoint: &EndpointLabel, body: &str) -> Result<Self, SolanaError> {
        const METHOD: Method = Method::SendTransaction;
        let envelope: Envelope<'_> = serde_json::from_str(body)
            .map_err(|source| SolanaError::unreadable(endpoint, METHOD, &source))?;
        if let Some(failure) = envelope.error {
            let is_refused = failure.data.as_ref().is_some_and(|data| data.err.is_some())
                || matches!(failure.code, PREFLIGHT_FAILED | SIGNATURE_VERIFY_FAILED);
            if is_refused {
                return Ok(Self::Refused(classify_preflight(
                    &failure.message,
                    failure.data.as_ref(),
                )));
            }
            return Err(SolanaError::Rpc {
                endpoint: endpoint.clone(),
                method: METHOD,
                code: failure.code,
                message: failure.message,
            });
        }
        let raw = envelope.result.ok_or_else(|| SolanaError::EmptyAnswer {
            endpoint: endpoint.clone(),
            method: METHOD,
        })?;
        let signature = serde_json::from_str(raw.get())
            .map_err(|source| SolanaError::unreadable(endpoint, METHOD, &source))?;
        Ok(Self::Accepted { endpoint: endpoint.clone(), signature })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attempt {
    First,
    Rebroadcast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The endpoint took the bytes.
    Accepted,
    /// The network already had this transaction, which is a success.
    AlreadyLanded,
}

/// One attempt against one endpoint.
pub(crate) async fn deliver(
    client: &SolanaClient,
    raw: &[u8],
    signature: &str,
    attempt: Attempt,
) -> Result<Delivery, SolanaError> {
    match client.send_transaction(raw, attempt).await? {
        BroadcastReply::Accepted { endpoint, signature: echoed } => {
            // The signature is derived from the bytes, so a node echoing another
            // one is not answering about this transaction.
            if echoed == signature {
                Ok(Delivery::Accepted)
            } else {
                Err(SolanaError::SignatureEcho { endpoint })
            }
        }
        BroadcastReply::Refused(rejection) => {
            if rejection.retry == RetryPolicy::AlreadyLanded {
                return Ok(Delivery::AlreadyLanded);
            }
            Err(SolanaError::Rejected(rejection))
        }
    }
}

#[cfg(test)]
mod tests {
    use jelly_net::Outcome;

    use super::*;
    use crate::test_util::endpoint;

    fn preflight(name: &str, logs: &[&str]) -> Preflight {
        Preflight {
            err: Some(TransactionFailure::new(name)),
            logs: logs.iter().map(|line| (*line).to_owned()).collect(),
        }
    }

    #[test]
    fn every_refusal_round_trips_its_name_and_only_a_duplicate_counts_as_landed() {
        assert_eq!(BroadcastRefusal::COUNT, 9);
        for refusal in BroadcastRefusal::ALL {
            assert_eq!(BroadcastRefusal::parse(refusal.as_str()), Some(refusal));
            assert_eq!(
                refusal.retry() == RetryPolicy::AlreadyLanded,
                refusal == BroadcastRefusal::AlreadyProcessed,
                "{refusal}"
            );
        }
        assert_eq!(BroadcastRefusal::AlreadyProcessed.retry(), RetryPolicy::AlreadyLanded);
        assert_eq!(BroadcastRefusal::BlockhashNotFound.retry(), RetryPolicy::DidNotLand);
        assert_eq!(BroadcastRefusal::ProgramFailed.kind(), RejectionKind::Reverted);
        assert_eq!(BroadcastRefusal::BelowRentMinimum.kind(), RejectionKind::Dust);
    }

    #[test]
    fn an_empty_wording_falls_back_to_the_explanation_and_a_real_one_is_kept() {
        for refusal in BroadcastRefusal::ALL {
            assert!(!refusal.explain().is_empty(), "{refusal}");
            assert_eq!(refusal.into_rejection("   ").detail, refusal.explain(), "{refusal}");
        }
        let given = BroadcastRefusal::BlockhashNotFound.into_rejection("  BlockhashNotFound  ");
        assert_eq!(given.detail, "BlockhashNotFound");
    }

    #[test]
    fn the_runtime_error_names_this_build_knows_classify_and_the_rest_do_not_land() {
        let table = [
            ("AlreadyProcessed", BroadcastRefusal::AlreadyProcessed),
            ("BlockhashNotFound", BroadcastRefusal::BlockhashNotFound),
            ("AccountNotFound", BroadcastRefusal::AccountMissing),
            ("InsufficientFundsForFee", BroadcastRefusal::InsufficientFunds),
            ("InsufficientFundsForRent", BroadcastRefusal::BelowRentMinimum),
            ("InstructionError", BroadcastRefusal::ProgramFailed),
            ("AccountInUse", BroadcastRefusal::Contended),
            ("SanitizeFailure", BroadcastRefusal::Malformed),
            ("SomethingTheRuntimeAddedLater", BroadcastRefusal::Unclassified),
        ];
        for (name, expected) in table {
            assert_eq!(BroadcastRefusal::classify(name), expected, "{name}");
        }
        assert_eq!(BroadcastRefusal::classify("").retry(), RetryPolicy::DidNotLand);
    }

    #[test]
    fn a_preflight_refusal_keeps_the_log_line_that_says_why() {
        let rejection = classify_preflight(
            "Transaction simulation failed",
            Some(&preflight("InstructionError", &["Program log: Error: insufficient funds"])),
        );
        assert_eq!(rejection.kind, RejectionKind::Reverted);
        assert_eq!(rejection.retry, RetryPolicy::FinalEverywhere);
        assert!(rejection.detail.contains("insufficient funds"), "{}", rejection.detail);
    }

    #[test]
    fn a_refusal_with_no_structured_error_did_not_land_and_keeps_the_message() {
        let rejection = classify_preflight("preflight failed", None);
        assert_eq!(rejection.kind, RejectionKind::Unclassified);
        assert_eq!(rejection.retry, RetryPolicy::DidNotLand);
        assert_eq!(rejection.detail, "preflight failed");
    }

    #[test]
    fn an_accepted_send_carries_the_signature_the_node_echoed() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":"5zq5"}"#;
        let reply = BroadcastReply::read(&endpoint(), body).expect("an answer");
        assert!(
            matches!(reply, BroadcastReply::Accepted { ref signature, .. } if signature == "5zq5")
        );
    }

    #[test]
    fn a_preflight_refusal_is_an_answer_and_a_plain_rpc_error_is_a_failure() {
        let refused = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32002,
            "message":"Transaction simulation failed","data":{"err":"AlreadyProcessed"}}}"#;
        let reply = BroadcastReply::read(&endpoint(), refused).expect("an answer");
        let BroadcastReply::Refused(rejection) = reply else {
            panic!("a refusal is an answer");
        };
        assert_eq!(rejection.retry, RetryPolicy::AlreadyLanded);

        // A gateway that strips `data` still leaves the preflight code.
        let stripped = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32002,"message":"failed"}}"#;
        assert!(matches!(
            BroadcastReply::read(&endpoint(), stripped).expect("an answer"),
            BroadcastReply::Refused(_)
        ));

        let behind =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32005,"message":"Node is behind"}}"#;
        let error = BroadcastReply::read(&endpoint(), behind).expect_err("a host failure");
        assert!(matches!(error, SolanaError::Rpc { code: -32005, .. }), "{error:?}");
    }

    #[test]
    fn an_unreadable_body_is_reported_by_position_and_an_empty_one_as_empty() {
        let error = BroadcastReply::read(&endpoint(), "not json").expect_err("not a response");
        assert!(matches!(error, SolanaError::Answer { line: 1, .. }), "{error:?}");
        let empty = BroadcastReply::read(&endpoint(), "{}").expect_err("neither result nor error");
        assert!(
            matches!(empty, SolanaError::EmptyAnswer { method: Method::SendTransaction, .. }),
            "{empty:?}"
        );
        assert_eq!(empty.outcome(), Outcome::Rotate);
    }
}
