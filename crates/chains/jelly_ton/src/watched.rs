use jelly_core::WatchedAddress;
use tonlib_core::types::TonAddress;

use crate::address::{TonDestination, ton_destination};
use crate::config::CHAIN;
use crate::error::TonError;
use crate::wallet::{Version, version_at};

#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    pub(crate) address: TonAddress,
    pub(crate) pubkey: [u8; 32],
    /// The wallet version whose wallet for `pubkey` sits at `address`.
    pub(crate) version: Version,
}

/// All or nothing: one foreign, keyless, or unreadable address, or one that isn't a wallet
/// of its key, fails the whole set.
pub(crate) fn resolve(addresses: &[WatchedAddress]) -> Result<Vec<Watched>, TonError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let mismatch = TonError::Invalid("The TON source address does not match its wallet key.");
        if entry.chain() != CHAIN {
            return Err(mismatch);
        }
        let pubkey =
            entry.pubkey.ok_or(TonError::Invalid("The TON wallet public key is missing."))?;
        let TonDestination { address, comment, .. } = ton_destination(entry.address.value())
            .map_err(|_| TonError::Invalid("The TON source address is not valid."))?;
        let (None, Some(version)) = (comment, version_at(&pubkey, &address)?) else {
            return Err(mismatch);
        };
        watched.push(Watched { address_id: entry.id, address, pubkey, version });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;
    use jelly_core::test_util::WatchedAddressBuilder;

    use super::*;
    use crate::wallet::wallet_address;

    const PUBKEY: [u8; 32] = [7; 32];

    fn keyed(address_id: i64, chain: ChainId, address: &str) -> WatchedAddress {
        let mut entry = WatchedAddressBuilder::new(address_id, chain, address).build();
        entry.pubkey = Some(PUBKEY);
        entry
    }

    #[track_caller]
    fn assert_refused(addresses: &[WatchedAddress], message: &str) {
        assert!(
            matches!(resolve(addresses), Err(TonError::Invalid(text)) if text == message),
            "expected {message:?}"
        );
    }

    #[test]
    fn each_wallet_version_of_a_key_resolves_to_its_own_version() {
        for version in Version::ALL {
            let address = wallet_address(&PUBKEY, version).unwrap();
            let resolved = resolve(&[keyed(7, CHAIN, &format!(" {address} "))]).unwrap();
            let [entry] = resolved.as_slice() else { panic!("expected one entry: {resolved:?}") };
            assert_eq!((entry.address_id, entry.version), (7, version));
        }
    }

    #[test]
    fn one_foreign_keyless_unreadable_or_requesting_row_refuses_the_whole_watch_set() {
        let address = wallet_address(&PUBKEY, Version::V5r1).unwrap();
        let owner = keyed(7, CHAIN, &address);
        let mismatch = "The TON source address does not match its wallet key.";
        assert_refused(&[owner.clone(), keyed(8, ChainId::Tron, &address)], mismatch);
        let keyless = WatchedAddressBuilder::new(8, CHAIN, &address).build();
        assert_refused(&[owner.clone(), keyless], "The TON wallet public key is missing.");
        assert_refused(
            &[owner.clone(), keyed(8, CHAIN, "not an address")],
            "The TON source address is not valid.",
        );
        let request = format!("ton://transfer/{address}?text=order");
        assert_refused(&[owner, keyed(8, CHAIN, &request)], mismatch);
    }
}
