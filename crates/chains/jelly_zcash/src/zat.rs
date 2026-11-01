use zcash_protocol::value::{MAX_MONEY, Zatoshis};

use crate::error::ZcashError;

/// No valid amount or total exceeds the money supply, so a sum past it is inconsistent
/// data.
pub(crate) fn add(left_zat: u64, right_zat: u64) -> Result<u64, ZcashError> {
    left_zat.checked_add(right_zat).filter(|total| *total <= MAX_MONEY).ok_or(ZcashError::Protocol)
}

/// An amount already checked against the money supply, as the consensus libraries take it.
pub(crate) fn zatoshis(value_zat: u64) -> Result<Zatoshis, ZcashError> {
    Zatoshis::from_u64(value_zat)
        .map_err(|_| ZcashError::Internal("a validated Zcash amount is out of range"))
}
