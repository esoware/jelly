use jelly_core::WatchedAddress;

use crate::address;
use crate::config::CHAIN;
use crate::error::TronError;

#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    pub(crate) address: String,
    pub(crate) body: [u8; address::BODY_BYTES],
}

/// All or nothing. A foreign or invalid address fails the whole set, because
/// silently dropping it would report a confident zero balance.
pub(crate) fn resolve(addresses: &[WatchedAddress]) -> Result<Vec<Watched>, TronError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let chain = entry.chain();
        if chain != CHAIN {
            return Err(TronError::ForeignChain { chain });
        }
        let body = address::body("watched address", entry.address.value())?;
        watched.push(Watched { address_id: entry.id, address: address::display(&body), body });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, test_util::WatchedAddressBuilder};

    use super::*;
    use crate::test_util::OWNER;

    #[test]
    fn a_watched_address_resolves_to_its_canonical_spelling_and_body() {
        let watched =
            vec![WatchedAddressBuilder::new(7, CHAIN, format!(" {OWNER} ").as_str()).build()];
        let resolved = resolve(&watched).expect("it resolves");
        let entry = resolved.first().expect("one entry");
        assert_eq!(entry.address_id, 7);
        assert_eq!(entry.address, OWNER);
        assert_eq!(address::display(&entry.body), OWNER);
    }

    #[test]
    fn one_unreadable_or_foreign_row_refuses_the_whole_watch_set() {
        let mixed = vec![
            WatchedAddressBuilder::new(7, CHAIN, OWNER).build(),
            WatchedAddressBuilder::new(8, CHAIN, "not an address").build(),
        ];
        assert!(matches!(
            resolve(&mixed),
            Err(TronError::InvalidAddress { field: "watched address", .. })
        ));

        let foreign = vec![WatchedAddressBuilder::new(7, ChainId::Bitcoin, OWNER).build()];
        assert!(matches!(resolve(&foreign), Err(TronError::ForeignChain { .. })));
    }
}
