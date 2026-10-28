//! What the network fixes, as opposed to the tunables in [`crate::MoneroConfig`],
//! and where each persisted blob is stored.

use jelly_core::{Amount, AssetId, ChainId, ReservationSlot, ScanStream};

use crate::error::MoneroError;

pub(crate) const CHAIN: ChainId = ChainId::Monero;

pub(crate) const fn check_chain(chain: ChainId) -> Result<(), MoneroError> {
    if matches!(chain, CHAIN) { Ok(()) } else { Err(MoneroError::ForeignChain { chain }) }
}

/// The only asset on this chain.
pub(crate) const ASSET: AssetId = AssetId::Native(CHAIN);

/// Monero prices a fee per unit of weight, which for the two-output transactions this
/// adapter builds is the size in bytes.
pub(crate) const FEE_RATE_UNIT: &str = "piconero/B";

/// An amount this crate counts in piconero, the chain's base unit.
pub(crate) fn piconero(value: u64) -> Amount {
    Amount::from(value)
}

/// Monero's spendability lock in blocks. An output is unspendable until ten
/// blocks after the one that created it.
///
/// The assertion below checks at compile time that the library still agrees.
pub(crate) const LOCK_WINDOW: u64 = 10;
const _: () = assert!(
    LOCK_WINDOW == 10 && monero_wallet::DEFAULT_LOCK_WINDOW == 10,
    "The lock window and the monero-wallet default must both be ten blocks."
);

/// Ring size for CLSAG with Bulletproofs+, fixed by consensus since hardfork 15.
pub(crate) const RING_LEN: u8 = 16;

/// Highest hardfork version `monero-wallet` 0.2 can scan or build for.
///
/// It mirrors the library's own check, which is what refuses a newer block, and
/// only names the version in [`crate::MoneroError::ProtocolTooNew`].
pub(crate) const MAX_HARDFORK_VERSION: u8 = 16;

/// Recent block ids kept so a reorganization can be located exactly.
///
/// A deeper reorganization degrades to a rescan from the wallet birthday, which
/// costs time but never gives a wrong balance. Mainnet has never reorganized
/// anywhere near this deep.
pub(crate) const REORG_WINDOW: u64 = 128;

/// Where the scan blob lives. It's derived from the view key, so the cursor port
/// routes it to the vault instead of the wallet database.
pub(crate) const SCAN_STREAM: ScanStream = ScanStream::MoneroScan;

/// Where the sends in flight live: the key images a second send mustn't reuse and
/// the signed bytes a resend needs.
///
/// It's secret, so it goes to the vault too, and it's a reservation because it's
/// written before those bytes leave the process.
pub(crate) const SENDS_SLOT: ReservationSlot = ReservationSlot::MoneroSends;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::SPEC;

    #[test]
    fn what_the_interface_calls_settled_is_the_lock_window() {
        assert_eq!(u64::from(SPEC.settled_confirmations), LOCK_WINDOW);
    }

    #[test]
    fn the_chain_guard_admits_this_chain_and_nothing_else() {
        check_chain(CHAIN).unwrap();
        assert!(matches!(
            check_chain(ChainId::Bitcoin),
            Err(MoneroError::ForeignChain { chain: ChainId::Bitcoin })
        ));
    }
}
