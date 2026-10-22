use jelly_core::WatchedAddress;

use crate::address::is_key_address;
use crate::config::CHAIN;
use crate::entry::ENTRY;
use crate::error::ZcashError;

#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    /// The canonical spelling of a transparent mainnet key-hash address.
    pub(crate) address: String,
}

/// All or nothing: one unreadable, foreign, or script-hash address, or one that repeats
/// another's id or address, fails the whole set.
pub(crate) fn resolve(addresses: &[WatchedAddress]) -> Result<Vec<Watched>, ZcashError> {
    let mut watched: Vec<Watched> = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let address = ENTRY
            .validate_address(entry.address.value())
            .map_err(|error| ZcashError::Key(error.into()))?;
        if entry.chain() != CHAIN
            || !is_key_address(address.value())
            || watched
                .iter()
                .any(|known| known.address_id == entry.id || known.address == address.value())
        {
            return Err(ZcashError::Invalid(
                "A Zcash source must be a unique transparent mainnet key address.",
            ));
        }
        watched.push(Watched { address_id: entry.id, address: address.into_value() });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;
    use jelly_core::test_util::WatchedAddressBuilder;

    use super::*;
    use crate::fixtures::{OWNER, RECIPIENT, SCRIPT_HASH};

    const UNIQUE_KEY_ADDRESS: &str =
        "A Zcash source must be a unique transparent mainnet key address.";

    #[test]
    fn a_watched_address_resolves_to_its_canonical_spelling() {
        let spelling = format!(" {OWNER} ");
        let resolved = resolve(&[WatchedAddressBuilder::new(7, CHAIN, &spelling).build()]).unwrap();
        let [entry] = resolved.as_slice() else { panic!("expected one entry: {resolved:?}") };
        assert_eq!((entry.address_id, entry.address.as_str()), (7, OWNER));
    }

    #[test]
    fn one_unreadable_foreign_script_hash_or_repeated_row_refuses_the_whole_watch_set() {
        let owner = WatchedAddressBuilder::new(7, CHAIN, OWNER).build();
        let unreadable = WatchedAddressBuilder::new(8, CHAIN, "not an address").build();
        let refused = resolve(&[owner.clone(), unreadable]);
        assert!(matches!(refused, Err(ZcashError::Key(_))), "{refused:?}");
        for other in [
            WatchedAddressBuilder::new(8, ChainId::Bitcoin, RECIPIENT).build(),
            WatchedAddressBuilder::new(8, CHAIN, SCRIPT_HASH).build(),
            WatchedAddressBuilder::new(7, CHAIN, RECIPIENT).build(),
            WatchedAddressBuilder::new(8, CHAIN, OWNER).build(),
        ] {
            let refused = resolve(&[owner.clone(), other]);
            assert!(
                matches!(refused, Err(ZcashError::Invalid(text)) if text == UNIQUE_KEY_ADDRESS),
                "{refused:?}"
            );
        }
    }
}
