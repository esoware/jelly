use std::iter;

use jelly_core::WatchedAddress;
use solana_pubkey::Pubkey;

use crate::address;
use crate::config::CHAIN;
use crate::error::SolanaError;
use crate::token::SolanaToken;

#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    pub(crate) owner: Pubkey,
    pub(crate) address: String,
}

impl Watched {
    /// The owner, then each carried token's associated account with the token it
    /// holds. A token balance moves only in its own account, and a transfer into
    /// an existing one names that account and never its owner.
    pub(crate) fn accounts(&self) -> impl Iterator<Item = (Option<SolanaToken>, Pubkey)> {
        let owner = self.owner;
        let tokens = SolanaToken::all().map(move |token| (Some(token), token.account_of(&owner)));
        iter::once((None, owner)).chain(tokens)
    }
}

/// All or nothing. A foreign or invalid address fails the whole set, because
/// silently dropping it would report a confident zero balance.
pub(crate) fn resolve(addresses: &[WatchedAddress]) -> Result<Vec<Watched>, SolanaError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let chain = entry.chain();
        if chain != CHAIN {
            return Err(SolanaError::ForeignChain { chain });
        }
        let owner = address::parse("watched address", entry.address.value())?;
        watched.push(Watched { address_id: entry.id, owner, address: owner.to_string() });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, test_util::WatchedAddressBuilder};

    use super::*;
    use crate::fixtures::OWNER;

    #[test]
    fn a_watched_address_resolves_to_its_canonical_spelling() {
        let watched = vec![
            WatchedAddressBuilder::new(7, ChainId::Solana, format!(" {OWNER} ").as_str()).build(),
        ];
        let resolved = resolve(&watched).expect("it resolves");
        let entry = resolved.first().expect("one entry");
        assert_eq!(entry.address_id, 7);
        assert_eq!(entry.address, OWNER.to_string());
        assert_eq!(entry.owner, OWNER);
    }

    #[test]
    fn one_unreadable_row_refuses_the_whole_watch_set() {
        let owner = OWNER.to_string();
        let mixed = vec![
            WatchedAddressBuilder::new(7, ChainId::Solana, &owner).build(),
            WatchedAddressBuilder::new(8, ChainId::Solana, "not an address").build(),
        ];
        assert!(matches!(
            resolve(&mixed),
            Err(SolanaError::InvalidAddress { field: "watched address", .. })
        ));

        let foreign = vec![WatchedAddressBuilder::new(7, ChainId::Bitcoin, &owner).build()];
        assert!(matches!(resolve(&foreign), Err(SolanaError::ForeignChain { .. })));
    }
}
