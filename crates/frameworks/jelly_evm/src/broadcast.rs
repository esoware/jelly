use jelly_core::{Rejection, RejectionKind, RetryPolicy, wire_enum};

wire_enum! {
    /// Why a node refused `eth_sendRawTransaction`.
    ///
    /// No client gives these a stable JSON-RPC error code, so `classify` matches
    /// substrings of each client's wording.
    pub(crate) enum BroadcastRefusal {
        /// The network already has these exact bytes, so the send is done.
        AlreadyKnown => "already_known",
        NonceTooLow => "nonce_too_low",
        /// A nonce above the next one the node expects. A node that queues such a
        /// transaction answers with its hash, so a refusal means it holds nothing.
        NonceGap => "nonce_gap",
        ReplacementUnderpriced => "replacement_underpriced",
        InsufficientFunds => "insufficient_funds",
        IntrinsicGasTooLow => "intrinsic_gas_too_low",
        FeeTooLow => "fee_too_low",
        Oversized => "oversized",
        Reverted => "reverted",
        /// Wording no row matches. It's kept verbatim and tried on another endpoint.
        Unclassified => "unclassified",
    }
}

impl BroadcastRefusal {
    /// `Oversized` maps to `Unclassified` because [`RejectionKind`] has no size
    /// refusal. Its [`RetryPolicy`] still says no endpoint will take it.
    pub(crate) const fn kind(self) -> RejectionKind {
        match self {
            Self::AlreadyKnown => RejectionKind::AlreadyKnown,
            Self::NonceTooLow => RejectionKind::NonceTooLow,
            Self::NonceGap => RejectionKind::NonceTooHigh,
            Self::ReplacementUnderpriced => RejectionKind::ReplacementUnderpriced,
            Self::InsufficientFunds => RejectionKind::InsufficientFunds,
            Self::IntrinsicGasTooLow => RejectionKind::GasTooLow,
            Self::FeeTooLow => RejectionKind::BelowRelayFee,
            Self::Reverted => RejectionKind::Reverted,
            Self::Oversized | Self::Unclassified => RejectionKind::Unclassified,
        }
    }

    pub(crate) const fn retry(self) -> RetryPolicy {
        match self {
            Self::AlreadyKnown => RetryPolicy::AlreadyLanded,
            Self::NonceTooLow
            | Self::ReplacementUnderpriced
            | Self::InsufficientFunds
            | Self::IntrinsicGasTooLow
            | Self::FeeTooLow
            | Self::Oversized
            | Self::Reverted => RetryPolicy::FinalEverywhere,
            Self::NonceGap => RetryPolicy::DidNotLand,
            Self::Unclassified => RetryPolicy::TryAnotherEndpoint,
        }
    }

    /// Fallback detail for a refusal with no readable message. Only the log shows
    /// it.
    pub(crate) const fn explain(self) -> &'static str {
        match self {
            Self::AlreadyKnown => "the network already has this transaction",
            Self::NonceTooLow => "this nonce is already used; the transaction can't confirm",
            Self::NonceGap => {
                "an earlier nonce is still missing, so the node did not take this one"
            }
            Self::ReplacementUnderpriced => {
                "a replacement has to outbid what it replaces by at least a tenth"
            }
            Self::InsufficientFunds => "the balance doesn't cover the value and the gas",
            Self::IntrinsicGasTooLow => "the gas limit is below what this transaction costs to run",
            Self::FeeTooLow => "the fee is below what this node relays at; raise the price",
            Self::Oversized => "the transaction is too large for the network to relay",
            Self::Reverted => "the transaction reverts when it is run",
            Self::Unclassified => "the node refused the transaction",
        }
    }

    /// Keeps the node's own wording, or [`Self::explain`] when it gave none.
    pub(crate) fn into_rejection(self, message: &str) -> Rejection {
        let trimmed = message.trim();
        let detail =
            if trimmed.is_empty() { self.explain().to_owned() } else { trimmed.to_owned() };
        Rejection::new(self.kind(), self.retry(), detail)
    }

    /// Specific rows come first, because a replacement refusal also contains
    /// `underpriced`, which the plain fee row matches.
    pub(crate) fn classify(message: &str) -> Self {
        let text = message.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));

        if has(&[
            "already known",
            "alreadyknown",
            "already in the tx pool",
            "txpool: already exists",
            "known transaction",
            "duplicate transaction",
        ]) {
            Self::AlreadyKnown
        } else if has(&[
            "replacement transaction underpriced",
            "replacement fee too low",
            "gas price too low to replace",
        ]) {
            Self::ReplacementUnderpriced
        } else if has(&["nonce too low", "nonce is too low", "old nonce", "oldnonce"]) {
            Self::NonceTooLow
        } else if has(&["nonce too high", "nonce gap", "noncegap", "nonce is too high"]) {
            Self::NonceGap
        } else if has(&[
            "insufficient funds",
            "insufficient balance",
            "insufficientfunds",
            "insufficientbalance",
        ]) {
            Self::InsufficientFunds
        } else if has(&["intrinsic gas too low", "gas limit too low", "intrinsicgastoolow"]) {
            Self::IntrinsicGasTooLow
        } else if has(&[
            "fee cap less than block base fee",
            "max fee per gas less than block base fee",
            "gas price below minimum",
            "gas tip cap",
            "gasprice is less than gas price minimum",
            "feetoolow",
            "underpriced",
        ]) {
            Self::FeeTooLow
        } else if has(&[
            "oversized data",
            "exceeds block gas limit",
            "tx too large",
            "transaction too large",
        ]) {
            Self::Oversized
        } else if has(&["execution reverted", "always failing transaction"]) {
            Self::Reverted
        } else {
            Self::Unclassified
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error wordings recorded from each client.
    const RECORDED: [(&str, &str, BroadcastRefusal, RejectionKind, RetryPolicy); 15] = [
        (
            "geth",
            "nonce too low: address 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, tx: 3 state: 5",
            BroadcastRefusal::NonceTooLow,
            RejectionKind::NonceTooLow,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "geth",
            "insufficient funds for gas * price + value: address 0xd8dA have 1000 want 2500",
            BroadcastRefusal::InsufficientFunds,
            RejectionKind::InsufficientFunds,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "geth",
            "already known",
            BroadcastRefusal::AlreadyKnown,
            RejectionKind::AlreadyKnown,
            RetryPolicy::AlreadyLanded,
        ),
        (
            "geth",
            "replacement transaction underpriced",
            BroadcastRefusal::ReplacementUnderpriced,
            RejectionKind::ReplacementUnderpriced,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "geth",
            "oversized data",
            BroadcastRefusal::Oversized,
            RejectionKind::Unclassified,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "geth",
            "nonce too high",
            BroadcastRefusal::NonceGap,
            RejectionKind::NonceTooHigh,
            RetryPolicy::DidNotLand,
        ),
        (
            "geth",
            "max fee per gas less than block base fee",
            BroadcastRefusal::FeeTooLow,
            RejectionKind::BelowRelayFee,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "geth",
            "execution reverted",
            BroadcastRefusal::Reverted,
            RejectionKind::Reverted,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "erigon",
            "txpool: already exists",
            BroadcastRefusal::AlreadyKnown,
            RejectionKind::AlreadyKnown,
            RetryPolicy::AlreadyLanded,
        ),
        (
            "reth",
            "transaction underpriced",
            BroadcastRefusal::FeeTooLow,
            RejectionKind::BelowRelayFee,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "reth",
            "intrinsic gas too low",
            BroadcastRefusal::IntrinsicGasTooLow,
            RejectionKind::GasTooLow,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "bsc-geth",
            "gasprice is less than gas price minimum",
            BroadcastRefusal::FeeTooLow,
            RejectionKind::BelowRelayFee,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "nethermind",
            "AlreadyKnown",
            BroadcastRefusal::AlreadyKnown,
            RejectionKind::AlreadyKnown,
            RetryPolicy::AlreadyLanded,
        ),
        (
            "nethermind",
            "OldNonce, Current nonce: 5, nonce of rejected tx: 3",
            BroadcastRefusal::NonceTooLow,
            RejectionKind::NonceTooLow,
            RetryPolicy::FinalEverywhere,
        ),
        (
            "nethermind",
            "InsufficientFunds, Account balance: 0",
            BroadcastRefusal::InsufficientFunds,
            RejectionKind::InsufficientFunds,
            RetryPolicy::FinalEverywhere,
        ),
    ];

    #[test]
    fn every_recorded_wording_resolves_to_one_refusal_one_kind_and_one_policy() {
        for (client, message, expected, kind, retry) in RECORDED {
            assert_eq!(BroadcastRefusal::classify(message), expected, "{client}: {message}");
            let shared = expected.into_rejection(message);
            assert_eq!(shared.kind, kind, "{client}: {message}");
            assert_eq!(shared.retry, retry, "{client}: {message}");
            assert_eq!(shared.detail, message, "the node's own wording is kept whole");
        }
        assert_eq!(
            BroadcastRefusal::classify("some brand new client wording"),
            BroadcastRefusal::Unclassified,
            "wording no row matches is kept verbatim and tried elsewhere"
        );
    }

    #[test]
    fn an_empty_wording_still_produces_a_sentence_and_a_real_one_is_never_dropped() {
        for refusal in BroadcastRefusal::ALL {
            assert!(!refusal.explain().is_empty(), "{refusal}");
            assert_eq!(refusal.into_rejection("   ").detail, refusal.explain(), "{refusal}");
        }
        let given = BroadcastRefusal::NonceTooLow.into_rejection("  nonce too low  ");
        assert_eq!(given.detail, "nonce too low");
    }

    #[test]
    fn a_replacement_refusal_is_not_read_as_a_plain_underpriced_one() {
        assert_eq!(
            BroadcastRefusal::classify("replacement transaction underpriced"),
            BroadcastRefusal::ReplacementUnderpriced
        );
        assert_eq!(
            BroadcastRefusal::classify("transaction underpriced"),
            BroadcastRefusal::FeeTooLow
        );
    }

    #[test]
    fn every_refusal_but_a_known_a_gap_and_an_unclassified_one_is_final_everywhere() {
        assert_eq!(BroadcastRefusal::AlreadyKnown.retry(), RetryPolicy::AlreadyLanded);
        assert_eq!(BroadcastRefusal::NonceGap.retry(), RetryPolicy::DidNotLand);
        assert_eq!(BroadcastRefusal::Unclassified.retry(), RetryPolicy::TryAnotherEndpoint);
        for refusal in BroadcastRefusal::ALL {
            assert_eq!(BroadcastRefusal::parse(refusal.as_str()), Some(refusal), "{refusal}");
            if matches!(
                refusal,
                BroadcastRefusal::AlreadyKnown
                    | BroadcastRefusal::NonceGap
                    | BroadcastRefusal::Unclassified
            ) {
                continue;
            }
            assert_eq!(refusal.retry(), RetryPolicy::FinalEverywhere, "{refusal}");
        }
        assert_eq!(BroadcastRefusal::COUNT, 10, "a new refusal needs a policy of its own above");
    }
}
