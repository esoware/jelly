use alloy::primitives::Address;
use jelly_core::{ChainId, WatchedAddress};

use crate::address;
use crate::error::EvmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    pub(crate) owner: Address,
}

/// All or nothing. A foreign or invalid address fails the whole set, because
/// silently dropping it would report a confident zero balance.
pub(crate) fn resolve(
    chain: ChainId,
    addresses: &[WatchedAddress],
) -> Result<Vec<Watched>, EvmError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let found = entry.chain();
        if found != chain {
            return Err(EvmError::ForeignChain { chain: found });
        }
        watched.push(Watched {
            address_id: entry.id,
            owner: address::parse(chain, entry.address.value())?,
        });
    }
    Ok(watched)
}

pub(crate) fn address_of(watched: &[Watched], owner: Address) -> Option<i64> {
    watched.iter().find(|entry| entry.owner == owner).map(|entry| entry.address_id)
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use jelly_core::test_util::WatchedAddressBuilder;

    use super::*;

    #[test]
    fn a_watch_set_from_another_chain_fails_the_whole_set() {
        let good = WatchedAddressBuilder::new(
            1,
            ChainId::Ethereum,
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        )
        .build();
        let foreign = WatchedAddressBuilder::new(
            2,
            ChainId::Bsc,
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        )
        .build();
        assert_eq!(resolve(ChainId::Ethereum, from_ref(&good)).expect("resolved").len(), 1);
        assert!(matches!(
            resolve(ChainId::Ethereum, &[good, foreign]),
            Err(EvmError::ForeignChain { chain: ChainId::Bsc })
        ));
    }
}
