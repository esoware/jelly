use jelly_core::{Amount, AssetId, Clock, TokenId, TxPayload};
use serde::{Deserialize, Serialize};
use tonlib_core::types::TonAddress;

use crate::address::ton_destination;
use crate::config::{CHAIN, MAX_COMMENT_BYTES, MAX_FEE_NANOGRAM};
use crate::error::TonError;
use crate::jetton::jetton;
use crate::wallet::{Version, ton_wallet};

pub(crate) const INVALID_PAYMENT: TonError =
    TonError::Invalid("The TON payment is not valid. Build it again.");

/// Serialized into the `UnsignedTx` payload and into the `ton.sends` reservation, so its
/// field names are a stored format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payment {
    pub(crate) from: String,
    /// The Gram recipient with the bounce flag the payment carries, or the owner a
    /// jetton transfer pays, as the user wrote it.
    pub(crate) to: String,
    pub(crate) pubkey: [u8; 32],
    pub(crate) version: Version,
    pub(crate) fee_nanogram: u64,
    pub(crate) seqno: u32,
    /// The sender's last transaction lt when the payment was built. While the send is
    /// pending, sync walks the history back to it so the settlement is seen.
    pub(crate) before_lt: u64,
    pub(crate) valid_until: u32,
    pub(crate) is_deploy: bool,
    pub(crate) is_max: bool,
    pub(crate) transfer: Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Transfer {
    Native {
        amount_nanogram: u64,
        is_bounceable: bool,
        comment: Option<String>,
    },
    /// A TEP-74 `transfer` to the sender's own jetton wallet, which moves `amount` to
    /// the owner `to` names and returns what `attached_nanogram` doesn't spend. The
    /// comment rides in the forward payload the owner is notified with.
    Jetton {
        token: TokenId,
        wallet: String,
        amount: Amount,
        query_id: u64,
        attached_nanogram: u64,
        comment: Option<String>,
    },
}

impl Payment {
    pub(crate) const fn asset(&self) -> AssetId {
        match self.transfer {
            Transfer::Native { .. } => AssetId::Native(CHAIN),
            Transfer::Jetton { token, .. } => AssetId::Token(token),
        }
    }

    pub(crate) fn amount(&self) -> Amount {
        match self.transfer {
            Transfer::Native { amount_nanogram, .. } => Amount::from(amount_nanogram),
            Transfer::Jetton { amount, .. } => amount,
        }
    }

    /// The Gram the payment can take from the sender beyond the amount it pays: the
    /// network fee, plus the gas a jetton transfer attaches.
    pub(crate) fn cost_nanogram(&self) -> Result<u64, TonError> {
        match self.transfer {
            Transfer::Native { .. } => Ok(self.fee_nanogram),
            Transfer::Jetton { attached_nanogram, .. } => self
                .fee_nanogram
                .checked_add(attached_nanogram)
                .ok_or(TonError::Internal("a jetton transfer's cost overflowed")),
        }
    }

    pub(crate) fn comment(&self) -> Option<&str> {
        match self.transfer {
            Transfer::Native { ref comment, .. } | Transfer::Jetton { ref comment, .. } => {
                comment.as_deref()
            }
        }
    }

    /// Refuses a payment once its `valid_until` is reached, since the wallet would no
    /// longer accept it.
    pub(crate) fn ensure_unexpired(&self, clock: &dyn Clock) -> Result<(), TonError> {
        if clock.now_secs()? >= i64::from(self.valid_until) {
            return Err(TonError::Invalid("This TON payment has expired. Build it again."));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), TonError> {
        let wallet = ton_wallet(&self.pubkey, self.version)?;
        let from = ton_destination(&self.from).map_err(|_| INVALID_PAYMENT)?;
        let to = ton_destination(&self.to).map_err(|_| INVALID_PAYMENT)?;
        if wallet.address != from.address
            || from.address == to.address
            || to.comment.is_some()
            || self.fee_nanogram == 0
            || self.fee_nanogram > MAX_FEE_NANOGRAM
            || self.valid_until == 0
            || (self.is_deploy && self.seqno != 0)
            || self.comment().is_some_and(|text| text.len() > MAX_COMMENT_BYTES)
        {
            return Err(INVALID_PAYMENT);
        }
        match self.transfer {
            Transfer::Native { amount_nanogram, is_bounceable, .. } => {
                if to.is_bounceable != Some(is_bounceable) || amount_nanogram == 0 {
                    return Err(INVALID_PAYMENT);
                }
            }
            Transfer::Jetton { token, ref wallet, amount, attached_nanogram, .. } => {
                let held = jetton(token).ok_or(INVALID_PAYMENT)?;
                if held.attached_nanogram != attached_nanogram
                    || amount.is_zero()
                    || TonAddress::from_hex_str(wallet).is_err()
                {
                    return Err(INVALID_PAYMENT);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<TxPayload, TonError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| TonError::Internal("a TON payment did not serialize"))?;
        Ok(TxPayload::from_bytes(bytes))
    }

    pub(crate) fn decode(payload: &TxPayload) -> Result<Self, TonError> {
        let payment: Self =
            serde_json::from_slice(payload.as_bytes()).map_err(|_| INVALID_PAYMENT)?;
        payment.validate()?;
        Ok(payment)
    }
}
