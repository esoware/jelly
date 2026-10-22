use jelly_core::TxPayload;
use serde::{Deserialize, Serialize};

use crate::address::{xrp_address, xrp_destination};
use crate::config::{MAX_DELETION_FEE_DROPS, MAX_DROPS, MAX_FEE_DROPS, VALIDITY_LEDGERS};
use crate::error::XrpError;

const INVALID_PAYMENT: &str = "The XRP payment is not valid. Build it again.";

jelly_core::wire_enum! {
    /// The transaction type a send is signed as.
    pub(crate) enum SendKind {
        Payment => "payment",
        /// An `AccountDelete`, which removes the sender's account and pays the
        /// recipient everything it holds less the fee.
        AccountDelete => "account_delete",
    }
}

/// Serialized into the `UnsignedTx` payload and into the `xrp.sends` reservation, so its
/// field names are a stored format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payment {
    pub(crate) kind: SendKind,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) destination_tag: Option<u32>,
    /// What the recipient receives. A deletion pays whatever the account holds when it is
    /// applied, so this is what it held when the send was built, less the fee.
    pub(crate) amount_drops: u64,
    pub(crate) fee_drops: u64,
    pub(crate) sequence: u32,
    pub(crate) first_ledger: u32,
    pub(crate) last_ledger: u32,
    /// The base and owner reserve the sender keeps, which a deletion recovers instead.
    pub(crate) reserve_drops: u64,
    /// The ledger's base reserve, the least a payment must bring to create an account.
    pub(crate) base_reserve_drops: u64,
    pub(crate) is_activation: bool,
}

impl Payment {
    pub(crate) fn validate(&self) -> Result<(), XrpError> {
        let from = xrp_destination(&self.from).map_err(|_| XrpError::Invalid(INVALID_PAYMENT))?;
        let to = xrp_destination(&self.to).map_err(|_| XrpError::Invalid(INVALID_PAYMENT))?;
        let is_kind_consistent = match self.kind {
            SendKind::Payment => self.fee_drops <= MAX_FEE_DROPS,
            SendKind::AccountDelete => {
                self.fee_drops <= MAX_DELETION_FEE_DROPS
                    && self.reserve_drops == 0
                    && !self.is_activation
            }
        };
        if xrp_address(&from.account) != self.from
            || xrp_address(&to.account) != self.to
            || self.from == self.to
            || !is_kind_consistent
            || self.amount_drops == 0
            || self.amount_drops > MAX_DROPS
            || self.fee_drops == 0
            || self.base_reserve_drops == 0
            || self.base_reserve_drops > MAX_DROPS
            || self.sequence == 0
            || self.first_ledger > self.last_ledger
            || self.last_ledger.saturating_sub(self.first_ledger) >= VALIDITY_LEDGERS
        {
            return Err(XrpError::Invalid(INVALID_PAYMENT));
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<TxPayload, XrpError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| XrpError::Internal("an XRP payment did not serialize"))?;
        Ok(TxPayload::from_bytes(bytes))
    }

    pub(crate) fn decode(payload: &TxPayload) -> Result<Self, XrpError> {
        let payment: Self = serde_json::from_slice(payload.as_bytes())
            .map_err(|_| XrpError::Invalid(INVALID_PAYMENT))?;
        payment.validate()?;
        Ok(payment)
    }
}
