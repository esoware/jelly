use monero_address::SubaddressIndex;
use zeroize::Zeroize;

/// One address of this wallet, by its account and its index in that account.
///
/// `SubaddressIndex` can't represent `(0, 0)`, the wallet's primary address.
/// This orders by account and then by index, so an account's lowest address
/// comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Zeroize)]
pub(crate) struct Subaddress {
    pub major: u32,
    pub minor: u32,
}

impl Subaddress {
    pub(crate) const PRIMARY: Self = Self::new(0, 0);

    pub(crate) const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// The primary address of this one's account, where a send's change goes.
    pub(crate) const fn account_primary(self) -> Self {
        Self::new(self.major, 0)
    }

    /// The library's index, which is `None` for the primary address.
    pub(crate) const fn index(self) -> Option<SubaddressIndex> {
        SubaddressIndex::new(self.major, self.minor)
    }
}
