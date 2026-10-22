use std::collections::{BTreeMap, HashMap};

use jelly_core::{KeySource, WatchedAddress};

use crate::subaddress::Subaddress;

/// The watch set indexed both ways, because a pass walks it by subaddress and a
/// send looks it up by address id.
#[derive(Debug, Default)]
pub(crate) struct Watched {
    by_subaddress: BTreeMap<Subaddress, i64>,
    by_id: HashMap<i64, Subaddress>,
}

impl From<&[WatchedAddress]> for Watched {
    fn from(addresses: &[WatchedAddress]) -> Self {
        let mut watched = Self::default();
        for address in addresses {
            if let KeySource::Subaddress { major, minor } = address.source {
                let subaddress = Subaddress::new(major, minor);
                watched.by_subaddress.insert(subaddress, address.id);
                watched.by_id.insert(address.id, subaddress);
            }
        }
        watched
    }
}

impl Watched {
    pub(crate) fn len(&self) -> usize {
        self.by_subaddress.len()
    }

    /// Every watched address with its subaddress, lowest subaddress first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Subaddress, i64)> {
        self.by_subaddress.iter().map(|(&subaddress, &address_id)| (subaddress, address_id))
    }

    pub(crate) fn subaddresses(&self) -> impl Iterator<Item = Subaddress> {
        self.by_subaddress.keys().copied()
    }

    pub(crate) fn subaddress_of(&self, address_id: i64) -> Option<Subaddress> {
        self.by_id.get(&address_id).copied()
    }

    pub(crate) fn address_id_of(&self, subaddress: Subaddress) -> Option<i64> {
        self.by_subaddress.get(&subaddress).copied()
    }

    /// Which watched address an outgoing transaction is filed under.
    ///
    /// Monero spends within an account and returns change to it, so the account is
    /// the unit. The subaddress the send was made from wins when watched, then the
    /// account's primary address, then the lowest watched address in that account.
    pub(crate) fn attribute(&self, subaddress: Subaddress) -> Option<i64> {
        self.address_id_of(subaddress)
            .or_else(|| self.address_id_of(subaddress.account_primary()))
            .or_else(|| {
                self.iter()
                    .find(|(watched, _)| watched.major == subaddress.major)
                    .map(|(_, address_id)| address_id)
            })
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;
    use jelly_core::test_util::WatchedAddressBuilder;

    use super::*;

    fn watched(rows: &[(i64, u32, u32)]) -> Watched {
        let addresses: Vec<WatchedAddress> = rows
            .iter()
            .map(|&(id, major, minor)| {
                WatchedAddressBuilder::new(id, ChainId::Monero, "4AAA")
                    .subaddress(major, minor)
                    .build()
            })
            .collect();
        Watched::from(addresses.as_slice())
    }

    #[test]
    fn an_outgoing_transaction_is_filed_under_the_account_it_spent_from() {
        let watched = watched(&[(1, 0, 0), (2, 1, 0), (3, 1, 5), (4, 2, 9)]);
        assert_eq!(watched.attribute(Subaddress::new(1, 5)), Some(3), "the exact subaddress");
        assert_eq!(
            watched.attribute(Subaddress::new(1, 7)),
            Some(2),
            "the account's primary address"
        );
        assert_eq!(
            watched.attribute(Subaddress::new(2, 1)),
            Some(4),
            "the lowest watched in that account"
        );
        assert_eq!(
            watched.attribute(Subaddress::new(3, 0)),
            None,
            "nothing watched in that account"
        );
    }
}
