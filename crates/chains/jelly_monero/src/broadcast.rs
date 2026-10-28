use jelly_core::{Rejection, RejectionKind, RetryPolicy, wire_enum};
use serde::Deserialize;

wire_enum! {
    /// Why `monerod` refused a transaction.
    ///
    /// `send_raw_transaction` answers with a free-form `reason` plus one boolean
    /// per refusal class. The booleans are read first, and the text is only a
    /// fallback for a node that set none of them. The declaration order is the
    /// order the booleans are tested in, most specific first.
    pub(crate) enum BroadcastRefusal {
        /// A key image this transaction spends is already spent on chain or in
        /// the pool. Only one of the two can ever land.
        DoubleSpend => "double_spend",
        /// One of the outputs the ring names is gone or already consumed. A
        /// rebuild picks a fresh ring.
        InvalidInput => "invalid_input",
        /// The inputs don't cover the outputs plus the fee.
        Overspend => "overspend",
        /// Below this network's relay floor at the moment it was offered.
        FeeTooLow => "fee_too_low",
        /// A ring of the wrong length, which is consensus-invalid.
        LowMixin => "low_mixin",
        /// An output the network won't accept.
        InvalidOutput => "invalid_output",
        /// Fewer outputs than Monero's two-output minimum.
        TooFewOutputs => "too_few_outputs",
        /// Larger than the network relays.
        TooBig => "too_big",
        /// Not relayed under this node's own policy, which another node may not
        /// share.
        NotRelayed => "not_relayed",
        /// This node refused and said nothing this build understands. The
        /// node's own words travel as the rejection's detail.
        Unclassified => "unclassified",
    }
}

impl BroadcastRefusal {
    const fn kind(self) -> RejectionKind {
        match self {
            Self::DoubleSpend => RejectionKind::DoubleSpend,
            Self::InvalidInput => RejectionKind::MissingOrSpent,
            Self::FeeTooLow => RejectionKind::BelowRelayFee,
            Self::Overspend => RejectionKind::InsufficientFunds,
            Self::LowMixin
            | Self::InvalidOutput
            | Self::TooFewOutputs
            | Self::TooBig
            | Self::NotRelayed
            | Self::Unclassified => RejectionKind::Unclassified,
        }
    }

    const fn retry(self) -> RetryPolicy {
        match self {
            // The network will refuse this transaction as it stands, wherever it
            // is offered.
            Self::DoubleSpend
            | Self::Overspend
            | Self::LowMixin
            | Self::InvalidOutput
            | Self::TooFewOutputs
            | Self::TooBig => RetryPolicy::FinalEverywhere,
            // The transaction is in no pool, so a rebuild is safe and necessary.
            Self::InvalidInput | Self::FeeTooLow | Self::Unclassified => RetryPolicy::DidNotLand,
            // This node's policy, not the network's.
            Self::NotRelayed => RetryPolicy::TryAnotherEndpoint,
        }
    }

    /// The detail for a refusal that came with no reason text, because a rejection's
    /// detail is never empty. It only reaches the log. The interface words a refusal
    /// from its [`RejectionKind`].
    const fn explain(self) -> &'static str {
        match self {
            Self::DoubleSpend => "one of the coins this transaction spends is already spent",
            Self::InvalidInput => "one of the ring members is gone; rebuild the transaction",
            Self::Overspend => "the inputs do not cover the payment and the fee",
            Self::FeeTooLow => "the fee is below the network's relay floor; raise the priority",
            Self::LowMixin => "the ring is not the size consensus requires",
            Self::InvalidOutput => "the network refused one of the outputs",
            Self::TooFewOutputs => "monero requires at least two outputs",
            Self::TooBig => "the transaction is too large to relay; split the send",
            Self::NotRelayed => "this node declined to relay it; another node may accept it",
            Self::Unclassified => "the node refused the transaction",
        }
    }

    /// The fallback for a refusal that set no boolean, and the one place Monero's
    /// refusal text is read. Everything downstream branches on [`RejectionKind`].
    pub(crate) fn classify(reason: &str) -> Self {
        let lower = reason.to_ascii_lowercase();
        let has = |needle: &str| lower.contains(needle);
        if has("double spend") || has("double_spend") {
            Self::DoubleSpend
        } else if has("invalid input") || has("invalid_input") {
            Self::InvalidInput
        } else if has("overspend") {
            Self::Overspend
        } else if has("fee") && (has("low") || has("small")) {
            Self::FeeTooLow
        } else if has("mixin") || has("ring size") {
            Self::LowMixin
        } else if has("invalid output") || has("invalid_output") {
            Self::InvalidOutput
        } else if has("too big") || has("too_big") {
            Self::TooBig
        } else if has("not relayed") || has("not_relayed") {
            Self::NotRelayed
        } else {
            Self::Unclassified
        }
    }

    /// The typed rejection, keeping the node's own words as the detail.
    pub(crate) fn into_rejection(self, reason: &str) -> Rejection {
        let trimmed = reason.trim();
        let detail =
            if trimmed.is_empty() { self.explain().to_owned() } else { trimmed.to_owned() };
        Rejection::new(self.kind(), self.retry(), detail)
    }
}

/// What one broadcast attempt answered.
///
/// A refusal is an answer, not a transport failure, so it's typed, stops the
/// failover, and carries the node's own words.
#[derive(Debug, PartialEq, Eq)]
pub enum BroadcastReply {
    Accepted,
    /// The network refused, already classified.
    Refused(Rejection),
}

/// monerod's `send_raw_transaction`.
///
/// `reason` is free-form text kept only as the rejection's detail, and nothing
/// branches on it unless every boolean is unset.
#[expect(
    clippy::struct_excessive_bools,
    reason = "monerod's wire format, one boolean per refusal class"
)]
#[derive(Debug, Deserialize)]
pub(crate) struct SendRawAnswer {
    status: String,
    #[serde(default)]
    double_spend: bool,
    #[serde(default)]
    invalid_input: bool,
    #[serde(default)]
    overspend: bool,
    #[serde(default)]
    fee_too_low: bool,
    #[serde(default)]
    low_mixin: bool,
    #[serde(default)]
    invalid_output: bool,
    #[serde(default)]
    too_few_outputs: bool,
    #[serde(default)]
    too_big: bool,
    #[serde(default)]
    not_relayed: bool,
    #[serde(default)]
    reason: String,
}

impl SendRawAnswer {
    const fn is_set(&self, refusal: BroadcastRefusal) -> bool {
        match refusal {
            BroadcastRefusal::DoubleSpend => self.double_spend,
            BroadcastRefusal::InvalidInput => self.invalid_input,
            BroadcastRefusal::Overspend => self.overspend,
            BroadcastRefusal::FeeTooLow => self.fee_too_low,
            BroadcastRefusal::LowMixin => self.low_mixin,
            BroadcastRefusal::InvalidOutput => self.invalid_output,
            BroadcastRefusal::TooFewOutputs => self.too_few_outputs,
            BroadcastRefusal::TooBig => self.too_big,
            BroadcastRefusal::NotRelayed => self.not_relayed,
            BroadcastRefusal::Unclassified => false,
        }
    }

    /// The first boolean set, most specific first.
    fn refusal(&self) -> Option<BroadcastRefusal> {
        BroadcastRefusal::ALL.into_iter().find(|refusal| self.is_set(*refusal))
    }

    /// monerod answers `OK` for a transaction it already holds, which makes
    /// resending the identical bytes safe.
    pub(crate) fn read(&self) -> BroadcastReply {
        if self.status == "OK" {
            return BroadcastReply::Accepted;
        }
        let refusal = self.refusal().unwrap_or_else(|| BroadcastRefusal::classify(&self.reason));
        BroadcastReply::Refused(refusal.into_rejection(&self.reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_class_has_a_distinct_spelling_and_a_sentence() {
        for refusal in BroadcastRefusal::ALL {
            assert_eq!(BroadcastRefusal::parse(refusal.as_str()), Some(refusal));
            assert!(!refusal.explain().is_empty());
        }
        assert_eq!(BroadcastRefusal::COUNT, 10);
    }

    #[test]
    fn a_refusal_that_can_never_land_is_final_everywhere_and_no_other_is() {
        let never_lands = [
            BroadcastRefusal::DoubleSpend,
            BroadcastRefusal::Overspend,
            BroadcastRefusal::LowMixin,
            BroadcastRefusal::InvalidOutput,
            BroadcastRefusal::TooFewOutputs,
            BroadcastRefusal::TooBig,
        ];
        for refusal in BroadcastRefusal::ALL {
            assert_eq!(
                refusal.retry() == RetryPolicy::FinalEverywhere,
                never_lands.contains(&refusal),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_refusal_a_rebuild_or_another_node_might_clear_is_retryable() {
        for refusal in [
            BroadcastRefusal::InvalidInput,
            BroadcastRefusal::FeeTooLow,
            BroadcastRefusal::Unclassified,
        ] {
            assert_eq!(refusal.retry(), RetryPolicy::DidNotLand, "{refusal}");
        }
        assert_eq!(BroadcastRefusal::NotRelayed.retry(), RetryPolicy::TryAnotherEndpoint);
    }

    #[test]
    fn the_text_fallback_recognizes_the_wordings_monerod_uses() {
        let cases = [
            ("Failed to check tx inputs: double spend", BroadcastRefusal::DoubleSpend),
            ("tx uses invalid input", BroadcastRefusal::InvalidInput),
            ("transaction fee is too low", BroadcastRefusal::FeeTooLow),
            ("ring size is wrong", BroadcastRefusal::LowMixin),
            ("transaction is too big", BroadcastRefusal::TooBig),
            ("tx was not relayed", BroadcastRefusal::NotRelayed),
            ("something else entirely", BroadcastRefusal::Unclassified),
        ];
        for (reason, expected) in cases {
            assert_eq!(BroadcastRefusal::classify(reason), expected, "{reason}");
        }
    }

    #[test]
    fn an_empty_reason_still_produces_a_sentence_and_a_reason_is_never_dropped() {
        let empty = BroadcastRefusal::DoubleSpend.into_rejection("   ");
        assert_eq!(empty.detail, BroadcastRefusal::DoubleSpend.explain());
        let given = BroadcastRefusal::FeeTooLow.into_rejection("  fee too low  ");
        assert_eq!(given.detail, "fee too low");
    }

    #[test]
    fn a_broadcast_answer_is_read_from_its_booleans_before_its_words() {
        let ok: SendRawAnswer = serde_json::from_str(r#"{"status":"OK"}"#).expect("parses");
        assert_eq!(ok.read(), BroadcastReply::Accepted);

        let double: SendRawAnswer =
            serde_json::from_str(r#"{"status":"Failed","double_spend":true,"reason":"unhelpful"}"#)
                .expect("parses");
        let refused = double.read();
        assert!(
            matches!(&refused, BroadcastReply::Refused(rejection)
                if rejection.kind == RejectionKind::DoubleSpend
                    && rejection.detail == "unhelpful"),
            "{refused:?}"
        );

        let textual: SendRawAnswer =
            serde_json::from_str(r#"{"status":"Failed","reason":"transaction fee is too low"}"#)
                .expect("parses");
        let refused = textual.read();
        assert!(
            matches!(&refused, BroadcastReply::Refused(rejection)
                if rejection.kind == RejectionKind::BelowRelayFee),
            "{refused:?}"
        );
    }

    #[test]
    fn the_most_specific_boolean_set_classifies_the_refusal() {
        let several: SendRawAnswer = serde_json::from_str(
            r#"{"status":"Failed","fee_too_low":true,"not_relayed":true,"too_big":true}"#,
        )
        .expect("parses");
        assert_eq!(several.refusal(), Some(BroadcastRefusal::FeeTooLow));
        let none: SendRawAnswer = serde_json::from_str(r#"{"status":"Failed"}"#).expect("parses");
        assert_eq!(none.refusal(), None);
    }
}
