//! Pending sends, identified by `sha256(raw_data)`. After an ambiguous failure
//! the identical bytes are retried, because a fresh timestamp would make a second
//! payment.
//!
//! `verdict` allows a rebuild only once head-block time has reached the
//! expiration and no node holds the transaction.

use jelly_core::{
    Amount, AssetId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord, TxRecordBuilder,
    confirmations,
};
use serde::{Deserialize, Serialize};

use crate::entry::SPEC;
use crate::shape::{BlockRef, ContractResult, TransactionInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Tracked {
    /// The sixty-four character hex transaction id, which is `sha256(raw_data)`.
    pub(crate) txid: String,
    /// The row of the watched address that paid for it.
    pub(crate) address_id: i64,
    /// The spending address, so a send to itself is reported as one.
    pub(crate) owner: String,
    pub(crate) recipient: String,
    pub(crate) asset: AssetId,
    pub(crate) amount: Amount,
    /// The network refuses this transaction once a head block's timestamp reaches
    /// this millisecond.
    #[serde(rename = "expirationMs")]
    pub(crate) expiration_millis: i64,
    /// The signed bytes, hex, for a resend. A signed transaction is public.
    pub(crate) raw: String,
    /// False while the bytes have been signed but not yet handed to an endpoint.
    #[serde(rename = "sent")]
    pub(crate) is_sent: bool,
}

impl Tracked {
    /// The start of every record a follow-up writes for this send, decoded in full
    /// and naming its recipient.
    fn record(&self, moved: Amount, outcome: TxOutcome, fee: Amount) -> TxRecordBuilder {
        let direction = if self.recipient == self.owner {
            TxDirection::SelfTransfer
        } else {
            TxDirection::Outgoing
        };
        TxRecord::builder(
            self.asset,
            self.address_id,
            TxId::new(self.asset.chain(), &self.txid),
            direction,
            moved,
        )
        .outcome(outcome)
        .detection(TxDetection::Full)
        .counterparty(self.recipient.clone())
        .fee(fee)
    }

    /// Records the fee the receipt says was charged, not the preview's estimate.
    /// A failed transaction still pays it.
    pub(crate) fn landed_record(&self, landed: &Landed, tip_height: u64) -> TxRecord {
        let outcome =
            if landed.result.is_success() { TxOutcome::Success } else { TxOutcome::Failed };
        // A failed transaction moved nothing, and a record that says otherwise
        // would contribute to a total that never happened.
        let moved = if landed.result.is_success() { self.amount } else { Amount::ZERO };
        self.record(moved, outcome, Amount::from(landed.fee_sun))
            .confirmed_at(landed.height, landed.timestamp_millis.map(millis_to_secs), tip_height)
            .build()
    }

    /// Zero fee, because the network never accepted it. Stamped with head-block
    /// time, which decided the expiry, rather than the local clock.
    pub(crate) fn expired_record(&self, head_unix_secs: i64) -> TxRecord {
        self.record(Amount::ZERO, TxOutcome::Failed, Amount::ZERO).seen_at(head_unix_secs).build()
    }
}

/// TRON timestamps are milliseconds, and a record's timestamp is seconds.
pub(crate) const fn millis_to_secs(millis: i64) -> i64 {
    millis.saturating_div(1_000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Landed {
    pub(crate) height: u64,
    pub(crate) timestamp_millis: Option<i64>,
    pub(crate) fee_sun: u64,
    pub(crate) result: ContractResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The transaction is in a block. `is_settled` is true once it is deep enough
    /// that TRON considers it final, which is when the wallet stops following it.
    Landed { landed: Landed, is_settled: bool },
    /// No receipt, and either a node still holds it or head-block time has not
    /// reached its expiration. The same bytes are worth sending again.
    InFlight,
    /// No receipt, no node holds it, and head-block time has reached the
    /// expiration. The transaction is dead and a rebuild is safe.
    Expired,
}

/// Judges where a send stands from its receipt and the tip.
///
/// `is_held` is whether any node answered `gettransactionbyid`, which covers the
/// pool, so a transaction in a mempool is in flight however long it has waited.
/// Expiry is judged by the tip's timestamp, the clock the network uses.
pub(crate) fn verdict(
    info: &TransactionInfo,
    is_held: bool,
    tip: &BlockRef,
    expiration_millis: i64,
) -> Verdict {
    if let Some(height) = info.height() {
        let landed = Landed {
            height,
            timestamp_millis: info.timestamp_millis(),
            fee_sun: info.fee_sun(),
            result: info.result(),
        };
        let is_settled = confirmations(Some(height), tip.number) >= SPEC.settled_confirmations;
        return Verdict::Landed { landed, is_settled };
    }
    if is_held || !has_expired(tip, expiration_millis) {
        return Verdict::InFlight;
    }
    Verdict::Expired
}

/// java-tron refuses a transaction whose expiration is not after the head block's
/// timestamp, so from that block on none can include it.
pub(crate) const fn has_expired(tip: &BlockRef, expiration_millis: i64) -> bool {
    tip.timestamp_millis >= expiration_millis
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;
    use crate::fixtures::tracked;

    fn info(body: &str) -> TransactionInfo {
        serde_json::from_str(body).expect("a receipt decodes")
    }

    fn tip(number: u64, timestamp_millis: i64) -> BlockRef {
        BlockRef { number, block_id: [0u8; 32], timestamp_millis }
    }

    #[test]
    fn a_receipt_means_it_landed_and_it_settles_once_it_is_deep_enough() {
        let receipt = info(r#"{"id":"aa","blockNumber":100,"blockTimeStamp":1000,"fee":265000}"#);
        let shallow = verdict(&receipt, false, &tip(100, 0), 1_000_000);
        let Verdict::Landed { landed, is_settled } = shallow else {
            panic!("a receipt is a landing");
        };
        assert_eq!(landed.height, 100);
        assert_eq!(landed.fee_sun, 265_000);
        assert_eq!(landed.result, ContractResult::Success);
        assert!(!is_settled, "one block deep is not solidified");

        let deep = verdict(&receipt, false, &tip(118, 0), 1_000_000);
        assert!(matches!(deep, Verdict::Landed { is_settled: true, .. }));
    }

    #[test]
    fn no_receipt_is_in_flight_while_a_node_holds_it_or_the_window_is_open() {
        let missing = info("{}");
        assert_eq!(verdict(&missing, true, &tip(0, 9_000_000), 1_000_000), Verdict::InFlight);
        assert_eq!(verdict(&missing, false, &tip(0, 999_999), 1_000_000), Verdict::InFlight);
        assert_eq!(
            verdict(&missing, false, &tip(0, 1_000_000), 1_000_000),
            Verdict::Expired,
            "java-tron refuses a transaction whose expiration is not after the head block"
        );
        assert_eq!(verdict(&missing, false, &tip(0, 1_000_001), 1_000_000), Verdict::Expired);
    }

    #[test]
    fn a_landed_failure_moves_nothing_and_still_records_the_fee_it_burned() {
        let receipt = info(
            r#"{"id":"aa","blockNumber":9,"blockTimeStamp":9000,"fee":345000,"receipt":{"result":"OUT_OF_ENERGY"}}"#,
        );
        let Verdict::Landed { landed, .. } = verdict(&receipt, false, &tip(9, 0), 1) else {
            panic!("a receipt is a landing");
        };
        let record = tracked("aa", 7).landed_record(&landed, 9);
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.amount, Amount::ZERO, "a failure never contributes to a total");
        assert_eq!(record.fee, Some(Amount::from_base_units(345_000)));
        assert_eq!(record.height, Some(9));
        assert_eq!(record.timestamp, Some(9), "milliseconds become seconds");
        assert_eq!(record.confirmations, 1);
        assert_eq!(record.direction, TxDirection::Outgoing);
    }

    #[test]
    fn a_send_that_expired_records_a_zero_fee_because_nothing_was_charged() {
        // The head block that decided the expiry is what the record is stamped
        // with, so it lands in history where it happened whatever the local
        // clock says.
        let record = tracked("aa", 7).expired_record(millis_to_secs(1_786_599_999_000));
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.fee, Some(Amount::ZERO));
        assert_eq!(record.height, None);
        assert_eq!(record.timestamp, Some(1_786_599_999));
    }

    #[test]
    fn a_send_to_the_spending_address_itself_is_reported_as_one() {
        let entry = Tracked { recipient: String::from("TOwner"), ..tracked("aa", 7) };
        let landed = Landed {
            height: 1,
            timestamp_millis: None,
            fee_sun: 0,
            result: ContractResult::Success,
        };
        assert_eq!(entry.landed_record(&landed, 1).direction, TxDirection::SelfTransfer);
        assert_eq!(entry.expired_record(9).direction, TxDirection::SelfTransfer);
    }

    #[test]
    fn a_tracked_send_round_trips_through_its_stored_form() {
        let token = Tracked { asset: AssetId::Token(TokenId::UsdtTrc20), ..tracked("aa", 7) };
        let json = serde_json::to_string(&token).expect("it encodes");
        let back: Tracked = serde_json::from_str(&json).expect("it decodes");
        assert_eq!(back, token);
        assert!(json.contains("\"1000\""), "money crosses as a decimal string");
        assert!(json.contains("\"expirationMs\":"), "{json}");
        assert!(json.contains("\"sent\":"), "{json}");
    }
}
