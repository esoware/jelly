//! Pending Solana sends, persisted before broadcast and followed by signature.
//!
//! A transport failure leaves the result unknown, so the same bytes are resent
//! while the blockhash is valid. Rebuilding with a fresh blockhash could pay
//! twice, so a rebuild is safe only once the block height passes the last valid
//! height with no status.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{
    Amount, AssetId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord, TxRecordBuilder,
};
use serde::{Deserialize, Serialize};

use crate::config::NATIVE;
use crate::rpc::SignatureStatus;
use crate::sign;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Tracked {
    /// The base58 signature, which is the transaction id.
    pub(crate) signature: String,
    /// The row of the watched address that paid for it.
    pub(crate) address_id: i64,
    /// The fee payer, so a send to itself is reported as one.
    pub(crate) payer: String,
    pub(crate) asset: AssetId,
    /// Base units of `asset`.
    pub(crate) amount: u64,
    pub(crate) recipient: String,
    /// The last block height at which this transaction can land.
    pub(crate) expiry_height: u64,
    /// The signed bytes, base64, for a resend. A signed transaction is public.
    pub(crate) raw: String,
    /// False while the bytes have been signed but not yet handed to an endpoint.
    #[serde(rename = "sent")]
    pub(crate) is_sent: bool,
}

impl Tracked {
    fn amount(&self) -> Amount {
        Amount::from(self.amount)
    }

    fn direction(&self) -> TxDirection {
        if self.recipient == self.payer { TxDirection::SelfTransfer } else { TxDirection::Outgoing }
    }

    /// True when `raw` is `message` signed. A signed single-signer transaction is
    /// one shortvec length byte, a 64-byte signature, then exactly the message, so
    /// the comparison is exact.
    pub(crate) fn is_signed_message(&self, message: &[u8]) -> bool {
        BASE64
            .decode(&self.raw)
            .is_ok_and(|bytes| bytes.get(sign::SIGNATURE_OVERHEAD_BYTES..) == Some(message))
    }

    /// Carries no fee, so the store keeps the one it already holds until the
    /// signature walk replaces it with the charged figure.
    ///
    /// A failed transaction moved nothing and still cost its fee payer the fee, so
    /// it is recorded the way the walk reads it: native, outgoing and zero, whatever
    /// the send meant to move.
    pub(crate) fn landed_record(&self, slot: u64, tip_slot: u64, is_failed: bool) -> TxRecord {
        let (asset, direction, moved, outcome) = if is_failed {
            (NATIVE, TxDirection::Outgoing, Amount::ZERO, TxOutcome::Failed)
        } else {
            (self.asset, self.direction(), self.amount(), TxOutcome::Success)
        };
        self.record(asset, direction, moved, outcome).confirmed_at(slot, None, tip_slot).build()
    }

    /// Settles the row the submission wrote under a token send's own asset once the
    /// send has landed and failed, because the failure itself is recorded as native.
    /// Its fee is zero, because the fee was paid in SOL and the native record
    /// carries it.
    pub(crate) fn failed_token_record(&self, slot: u64, tip_slot: u64) -> Option<TxRecord> {
        let AssetId::Token(_) = self.asset else { return None };
        let record = self
            .record(self.asset, self.direction(), Amount::ZERO, TxOutcome::Failed)
            .fee(Amount::ZERO)
            .confirmed_at(slot, None, tip_slot)
            .build();
        Some(record)
    }

    /// Zero fee, because the transaction never landed. Without a `seen_at_secs`
    /// reading the store supplies a timestamp, and the Unix epoch is never
    /// substituted.
    pub(crate) fn expired_record(&self, seen_at_secs: Option<i64>) -> TxRecord {
        let mut builder = self
            .record(self.asset, self.direction(), self.amount(), TxOutcome::Failed)
            .fee(Amount::ZERO);
        if let Some(seen_at_secs) = seen_at_secs {
            builder = builder.seen_at(seen_at_secs);
        }
        builder.build()
    }

    /// A record of this send, which this wallet built, so it is decoded in full and
    /// names the recipient.
    fn record(
        &self,
        asset: AssetId,
        direction: TxDirection,
        amount: Amount,
        outcome: TxOutcome,
    ) -> TxRecordBuilder {
        TxRecord::builder(
            asset,
            self.address_id,
            TxId::new(asset.chain(), &self.signature),
            direction,
            amount,
        )
        .outcome(outcome)
        .detection(TxDetection::Full)
        .counterparty(self.recipient.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The transaction is in a block.
    Landed {
        slot: u64,
        /// True when the transaction ran and failed. It still paid its fee.
        is_failed: bool,
        /// True once a supermajority has voted for the block, which is when the
        /// wallet stops following it.
        is_settled: bool,
    },
    /// No status yet and the blockhash is still inside its window, so the same
    /// bytes are worth sending again.
    InFlight,
    /// No status and the block height has passed the blockhash's last valid
    /// height. The transaction is dead and a rebuild is safe.
    Expired,
}

/// `block_height` is a block height, not a slot, because a blockhash's window is
/// counted in blocks and skipped slots do not advance it.
///
/// It is the height of the endpoint that answered `status`, because one behind
/// the tip has no status for a transaction in a block it has not reached.
pub(crate) fn verdict(
    status: Option<&SignatureStatus>,
    block_height: u64,
    expiry_height: u64,
) -> Verdict {
    match status {
        Some(status) => Verdict::Landed {
            slot: status.slot,
            is_failed: status.err.is_some(),
            is_settled: status.is_settled(),
        },
        None if block_height <= expiry_height => Verdict::InFlight,
        None => Verdict::Expired,
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;
    use crate::fixtures;
    use crate::rpc::{Commitment, TransactionFailure};

    fn tracked() -> Tracked {
        fixtures::tracked("sig", 7)
    }

    fn status(slot: u64, is_failed: bool, level: Option<Commitment>) -> SignatureStatus {
        SignatureStatus {
            slot,
            err: is_failed.then(|| TransactionFailure::new("InstructionError")),
            confirmation_status: level,
        }
    }

    #[test]
    fn a_status_means_it_landed_and_settles_only_on_a_vote() {
        let seen = status(100, false, Some(Commitment::Processed));
        assert_eq!(
            verdict(Some(&seen), 1, 330),
            Verdict::Landed { slot: 100, is_failed: false, is_settled: false }
        );
        let voted = status(100, false, Some(Commitment::Confirmed));
        assert_eq!(
            verdict(Some(&voted), 1, 330),
            Verdict::Landed { slot: 100, is_failed: false, is_settled: true }
        );
        let reverted = status(100, true, Some(Commitment::Finalized));
        assert_eq!(
            verdict(Some(&reverted), 1, 330),
            Verdict::Landed { slot: 100, is_failed: true, is_settled: true }
        );
    }

    #[test]
    fn no_status_is_in_flight_until_the_block_height_passes_the_window() {
        assert_eq!(verdict(None, 1, 330), Verdict::InFlight);
        assert_eq!(verdict(None, 330, 330), Verdict::InFlight, "the last valid height still is");
        assert_eq!(verdict(None, 331, 330), Verdict::Expired);
    }

    #[test]
    fn a_landed_record_keeps_the_fee_the_submission_wrote_and_a_dead_one_zeroes_it() {
        let entry = tracked();
        let landed = entry.landed_record(100, 105, false);
        assert_eq!(landed.outcome, TxOutcome::Success);
        assert_eq!(landed.direction, TxDirection::Outgoing);
        assert_eq!(landed.detection, TxDetection::Full);
        assert_eq!(landed.amount, Amount::from_base_units(1_000));
        assert_eq!(landed.height, Some(100));
        assert_eq!(landed.confirmations, 6);
        assert_eq!(landed.fee, None, "a fee that was charged is not overwritten with an estimate");

        let failed = entry.landed_record(100, 100, true);
        assert_eq!(failed.outcome, TxOutcome::Failed);
        assert_eq!(failed.fee, None);

        let dead = entry.expired_record(Some(1_700_000_000));
        assert_eq!(dead.outcome, TxOutcome::Failed);
        assert_eq!(dead.fee, Some(Amount::ZERO), "nothing was charged");
        assert_eq!(dead.height, None);
        assert_eq!(dead.timestamp, Some(1_700_000_000));
    }

    /// The store timestamps a record that carries none when it writes the row.
    /// Stamping the epoch instead would sort this transaction to the bottom of the
    /// history for good.
    #[test]
    fn a_dead_record_with_no_clock_reading_carries_no_timestamp() {
        let dead = tracked().expired_record(None);
        assert_eq!(dead.timestamp, None);
        assert_eq!(dead.fee, Some(Amount::ZERO));
        assert_eq!(dead.outcome, TxOutcome::Failed);
    }

    #[test]
    fn a_send_to_the_payer_itself_is_reported_as_a_self_transfer() {
        let entry = Tracked { recipient: String::from("payer"), ..tracked() };
        let record = entry.landed_record(1, 1, false);
        assert_eq!(record.direction, TxDirection::SelfTransfer);
    }

    #[test]
    fn a_tracked_send_round_trips_with_its_asset() {
        let token = Tracked { asset: AssetId::Token(TokenId::UsdcSpl), is_sent: true, ..tracked() };
        let json = serde_json::to_string(&token).expect("it encodes");
        assert!(json.contains(r#""sent":true"#), "{json}");
        let back: Tracked = serde_json::from_str(&json).expect("it decodes");
        assert_eq!(back, token);
        assert_eq!(back.landed_record(1, 1, false).asset, AssetId::Token(TokenId::UsdcSpl));
    }
}
