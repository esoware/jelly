use crate::amount::Amount;
use crate::asset::AssetId;
use crate::tx::TxId;
use crate::wire_enum;

/// A height above the cached tip counts as one confirmation, because the node is ahead
/// of the wallet.
pub fn confirmations(height: Option<u64>, tip: u64) -> u32 {
    match height {
        None => 0,
        Some(mined) if mined > tip => 1,
        Some(mined) => {
            u32::try_from(tip.saturating_sub(mined).saturating_add(1)).unwrap_or(u32::MAX)
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Which way funds moved relative to the watched address.
    pub enum TxDirection {
        Incoming => "incoming",
        Outgoing => "outgoing",
        SelfTransfer => "self",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// What the network did with a transaction. A failed or replaced transaction never
    /// contributes to a total.
    pub enum TxOutcome {
        Success => "success",
        Failed => "failed",
        /// Another transaction took its place, so it never ran, moved nothing, and paid
        /// nothing.
        Replaced => "replaced",
        Unknown => "unknown",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How much of a transaction the sync pass saw.
    pub enum TxDetection {
        /// The transaction itself was decoded.
        Full => "full",
        /// Only a balance change was observed, so the details are unknown.
        BalanceDelta => "balance_delta",
    }
}

/// What an adapter reports one address's share of a transaction as. Use `builder` so
/// confirmation counts come from `confirmations`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxRecord {
    pub txid: TxId,
    pub asset: AssetId,
    pub address_id: i64,
    pub direction: TxDirection,
    pub outcome: TxOutcome,
    pub amount: Amount,
    pub fee: Option<Amount>,
    pub counterparty: Option<String>,
    /// The unconfirmed transaction this one spends from only to pay its fee, when it is
    /// a child-pays-for-parent speed-up that moves nothing else.
    pub pays_for: Option<TxId>,
    pub height: Option<u64>,
    /// Unix seconds of the block, or of the first sighting while unmined.
    pub timestamp: Option<i64>,
    /// Derived at read time from the current tip and never persisted.
    pub confirmations: u32,
    pub detection: TxDetection,
}

impl TxRecord {
    /// A pending, successful, fully decoded record that the caller refines.
    pub const fn builder(
        asset: AssetId,
        address_id: i64,
        txid: TxId,
        direction: TxDirection,
        amount: Amount,
    ) -> TxRecordBuilder {
        TxRecordBuilder {
            record: Self {
                txid,
                asset,
                address_id,
                direction,
                outcome: TxOutcome::Success,
                amount,
                fee: None,
                counterparty: None,
                pays_for: None,
                height: None,
                timestamp: None,
                confirmations: 0,
                detection: TxDetection::Full,
            },
        }
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct TxRecordBuilder {
    record: TxRecord,
}

impl TxRecordBuilder {
    pub const fn fee(mut self, fee: Amount) -> Self {
        self.record.fee = Some(fee);
        self
    }

    pub fn counterparty(mut self, counterparty: impl Into<String>) -> Self {
        self.record.counterparty = Some(counterparty.into());
        self
    }

    pub fn pays_for(mut self, txid: TxId) -> Self {
        self.record.pays_for = Some(txid);
        self
    }

    /// Also derives the confirmation count from `tip`.
    pub fn confirmed_at(mut self, height: u64, unix_secs: Option<i64>, tip: u64) -> Self {
        self.record.height = Some(height);
        self.record.timestamp = unix_secs;
        self.record.confirmations = confirmations(Some(height), tip);
        self
    }

    pub const fn seen_at(mut self, unix_secs: i64) -> Self {
        self.record.timestamp = Some(unix_secs);
        self
    }

    pub const fn outcome(mut self, outcome: TxOutcome) -> Self {
        self.record.outcome = outcome;
        self
    }

    pub const fn detection(mut self, detection: TxDetection) -> Self {
        self.record.detection = detection;
        self
    }

    pub fn build(self) -> TxRecord {
        self.record
    }
}
