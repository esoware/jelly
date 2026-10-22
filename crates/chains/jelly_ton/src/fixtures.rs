//! Fixtures the unit tests share.

use tonlib_core::types::TonAddress;

use crate::address::ton_destination;
use crate::record::AccountTx;

/// The account `text` names, in any spelling a recipient may write.
pub(crate) fn address(text: &str) -> TonAddress {
    ton_destination(text).unwrap().address
}

/// The rows of a history fixture. Every one is real mainnet history, newest first, as
/// toncenter's `getTransactions` returned it.
pub(crate) fn rows(text: &str) -> Vec<AccountTx> {
    serde_json::from_str(text).unwrap()
}
