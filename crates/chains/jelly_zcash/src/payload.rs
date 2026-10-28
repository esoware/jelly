use std::collections::BTreeSet;

use jelly_core::TxPayload;
use serde::{Deserialize, Serialize};
use zcash_primitives::transaction::{TxVersion, builder::DEFAULT_TX_EXPIRY_DELTA};
use zcash_protocol::consensus::{BranchId, MAIN_NETWORK};
use zcash_transparent::bundle::OutPoint;

use crate::address::{is_key_address, script};
use crate::config::{DUST_ZAT, MAX_INPUTS, branch_at};
use crate::entry::ENTRY;
use crate::error::ZcashError;
use crate::fee::{MAX_FEE_MULTIPLE, conventional_fee};
use crate::zat::add;

pub(crate) const INVALID_PAYMENT: ZcashError =
    ZcashError::Invalid("The Zcash payment is not valid. Build it again.");

/// One sending address of a payment. Serialized inside `Payment`, so its field names are
/// a stored format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub(crate) address_id: i64,
    pub(crate) address: String,
}

/// Serialized inside `Payment`, so its field names are a stored format. `address` is the
/// source that holds the coin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Coin {
    pub(crate) txid: String,
    pub(crate) index: u32,
    pub(crate) value_zat: u64,
    pub(crate) address: String,
}

impl Coin {
    pub(crate) fn outpoint(&self) -> Result<OutPoint, ZcashError> {
        Ok(OutPoint::new(hash_bytes(&self.txid)?, self.index))
    }
}

/// Serialized into the `UnsignedTx` payload and the `zcash.sends` reservation, so its field
/// names are a stored format.
///
/// `sources` are the request's addresses that the payment spends from or returns change
/// to, in the order the request named them. Change goes to the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payment {
    pub(crate) sources: Vec<Source>,
    pub(crate) to: String,
    pub(crate) target_height: u32,
    pub(crate) expiry_height: u32,
    pub(crate) inputs: Vec<Coin>,
    pub(crate) amount_zat: u64,
    pub(crate) change_zat: u64,
    pub(crate) fee_zat: u64,
    /// Change too small to keep, which `fee_zat` includes. Absent from a payment stored
    /// before this field existed.
    #[serde(default)]
    pub(crate) dropped_change_zat: u64,
}

impl Payment {
    pub(crate) fn branch(&self) -> BranchId {
        branch_at(self.target_height)
    }

    pub(crate) fn expiry_height(target_height: u32) -> Result<u32, ZcashError> {
        let end = branch_at(target_height).height_bounds(&MAIN_NETWORK).and_then(|(_, end)| end);
        let expiry =
            target_height.checked_add(DEFAULT_TX_EXPIRY_DELTA).ok_or(ZcashError::Protocol)?;
        // A transaction is only valid in the branch it was signed for, so expiry stops at the
        // last block before the next network upgrade, as zcashd does.
        Ok(end.map_or(expiry, |end| expiry.min(u32::from(end).saturating_sub(1))))
    }

    /// The address change returns to, the first source.
    pub(crate) fn change_address(&self) -> Result<&str, ZcashError> {
        self.sources.first().map(|source| source.address.as_str()).ok_or(INVALID_PAYMENT)
    }

    pub(crate) fn validate(&self) -> Result<(), ZcashError> {
        let is_canonical = |address: &str| {
            ENTRY.validate_address(address).is_ok_and(|checked| checked.value() == address)
        };
        let addresses: BTreeSet<&str> =
            self.sources.iter().map(|source| source.address.as_str()).collect();
        let address_ids: BTreeSet<i64> =
            self.sources.iter().map(|source| source.address_id).collect();
        if self.sources.is_empty()
            || addresses.len() != self.sources.len()
            || address_ids.len() != self.sources.len()
            || self.inputs.is_empty()
            || self.inputs.len() > MAX_INPUTS
            || !TxVersion::V5.valid_in_branch(self.branch())
            || Self::expiry_height(self.target_height).ok() != Some(self.expiry_height)
            || self.amount_zat < DUST_ZAT
            || (self.change_zat != 0 && self.change_zat < DUST_ZAT)
            || !self
                .sources
                .iter()
                .all(|source| is_key_address(&source.address) && is_canonical(&source.address))
            || !is_canonical(&self.to)
        {
            return Err(INVALID_PAYMENT);
        }
        let mut outpoints = BTreeSet::new();
        let mut spenders = BTreeSet::new();
        let mut total_zat = 0;
        for coin in &self.inputs {
            if hash_bytes(&coin.txid).is_err()
                || !outpoints.insert((&coin.txid, coin.index))
                || coin.value_zat == 0
                || !addresses.contains(coin.address.as_str())
            {
                return Err(INVALID_PAYMENT);
            }
            spenders.insert(coin.address.as_str());
            total_zat = add(total_zat, coin.value_zat).map_err(|_| INVALID_PAYMENT)?;
        }
        let change_address = self.change_address()?;
        let is_moved = |address: &str| {
            spenders.contains(address) || (self.change_zat > 0 && address == change_address)
        };
        if !addresses.iter().all(|address| is_moved(address)) {
            return Err(INVALID_PAYMENT);
        }
        let recipient = script(&self.to)?;
        let change = script(change_address)?;
        let plain_zat = conventional_fee(self.target_height, &self.inputs, &[&recipient])?;
        let with_change_zat =
            conventional_fee(self.target_height, &self.inputs, &[&recipient, &change])?;
        let spent_zat = add(self.amount_zat, self.change_zat)
            .and_then(|sum_zat| add(sum_zat, self.fee_zat))
            .map_err(|_| INVALID_PAYMENT)?;
        let paid_zat = self.fee_zat.checked_sub(self.dropped_change_zat).ok_or(INVALID_PAYMENT)?;
        let maximum_zat = with_change_zat.saturating_mul(MAX_FEE_MULTIPLE);
        let is_fee_valid = if self.change_zat > 0 {
            self.dropped_change_zat == 0 && paid_zat >= with_change_zat && paid_zat <= maximum_zat
        } else {
            self.dropped_change_zat < DUST_ZAT && paid_zat >= plain_zat && paid_zat <= maximum_zat
        };
        if spent_zat != total_zat || !is_fee_valid {
            return Err(INVALID_PAYMENT);
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<TxPayload, ZcashError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| ZcashError::Internal("a Zcash payment did not serialize"))?;
        Ok(TxPayload::from_bytes(bytes))
    }

    pub(crate) fn decode(payload: &TxPayload) -> Result<Self, ZcashError> {
        let payment: Self =
            serde_json::from_slice(payload.as_bytes()).map_err(|_| INVALID_PAYMENT)?;
        payment.validate()?;
        Ok(payment)
    }
}

/// Parses a hash written as 64 lowercase hex digits, as Blockbook and txids show it, into
/// internal byte order, which is reversed.
pub(crate) fn hash_bytes(hash: &str) -> Result<[u8; 32], ZcashError> {
    if hash.len() != 64 || !hash.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ZcashError::Protocol);
    }
    let mut bytes = [0; 32];
    hex::decode_to_slice(hash, &mut bytes).map_err(|_| ZcashError::Protocol)?;
    bytes.reverse();
    Ok(bytes)
}
