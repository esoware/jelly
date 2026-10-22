use jelly_core::WatchedAddress;

use crate::address::{XrpDestination, xrp_address, xrp_destination};
use crate::config::CHAIN;
use crate::error::XrpError;

#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    /// The classic address, whatever spelling the store holds.
    pub(crate) address: String,
}

/// All or nothing: one foreign, unreadable, or tagged address fails the whole set.
pub(crate) fn resolve(addresses: &[WatchedAddress]) -> Result<Vec<Watched>, XrpError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let XrpDestination { account, tag } = xrp_destination(entry.address.value())
            .map_err(|_| XrpError::Invalid("The XRP source address is not valid."))?;
        if entry.chain() != CHAIN || tag.is_some() {
            return Err(XrpError::Invalid("An XRP source must be an untagged mainnet account."));
        }
        watched.push(Watched { address_id: entry.id, address: xrp_address(&account) });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;
    use jelly_core::test_util::WatchedAddressBuilder;

    use super::*;
    use crate::address::CODEC;

    const OWNER: &str = "rfxdLwsZnoespnTDDb1Xhvbc8EFNdztaoq";

    #[track_caller]
    fn assert_refused(addresses: &[WatchedAddress], message: &str) {
        assert!(
            matches!(resolve(addresses), Err(XrpError::Invalid(text)) if text == message),
            "expected {message:?}"
        );
    }

    #[test]
    fn a_watched_address_resolves_to_its_classic_spelling() {
        let spelling = format!(" {OWNER} ");
        let resolved = resolve(&[WatchedAddressBuilder::new(7, CHAIN, &spelling).build()]).unwrap();
        let [entry] = resolved.as_slice() else { panic!("expected one entry: {resolved:?}") };
        assert_eq!((entry.address_id, entry.address.as_str()), (7, OWNER));
    }

    #[test]
    fn one_unreadable_tagged_or_foreign_row_refuses_the_whole_watch_set() {
        let owner = WatchedAddressBuilder::new(7, CHAIN, OWNER).build();
        let unreadable = WatchedAddressBuilder::new(8, CHAIN, "not an address").build();
        assert_refused(&[owner.clone(), unreadable], "The XRP source address is not valid.");
        let untagged_only = "An XRP source must be an untagged mainnet account.";
        let tagged = CODEC.validate(&format!("xrpl:{OWNER}?dt=0")).unwrap().into_value();
        let tagged = WatchedAddressBuilder::new(8, CHAIN, &tagged).build();
        assert_refused(&[owner.clone(), tagged], untagged_only);
        let foreign = WatchedAddressBuilder::new(8, ChainId::Bitcoin, OWNER).build();
        assert_refused(&[owner, foreign], untagged_only);
    }
}
