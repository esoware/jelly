use std::fmt;

use crate::address::{Address, AddressError};
use crate::chain::ChainId;
use crate::entry::{ChainEntry, ImportedKey};
use crate::error::KeyError;
use crate::secret::SecretText;
use crate::spec::ChainSpec;

/// Every chain's entry, one per [`ChainId`] variant.
///
/// Constructed once by the registry crate, which is the only crate that names every
/// chain crate. Everything else reaches a chain through [`ChainTable::entry`].
pub struct ChainTable {
    entries: [&'static dyn ChainEntry; ChainId::COUNT],
}

impl fmt::Debug for ChainTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChainTable").field("chains", &ChainId::ALL).finish()
    }
}

impl ChainTable {
    /// `entries` must sit in [`ChainId::ALL`] order. A table whose entries declare
    /// other chains is a build defect, and the assertion names the first wrong slot.
    pub fn new(entries: &[&'static dyn ChainEntry; ChainId::COUNT]) -> Self {
        for (chain, entry) in ChainId::ALL.into_iter().zip(entries) {
            assert_eq!(
                entry.spec().chain,
                chain,
                "the chain table entry for {chain} declares {}",
                entry.spec().chain
            );
        }
        Self { entries: *entries }
    }

    pub fn entry(&self, chain: ChainId) -> &'static dyn ChainEntry {
        // The constructor checked that every slot holds its own chain, and `ALL` is in
        // declaration order, so the position lookup cannot miss.
        let position =
            ChainId::ALL.iter().position(|candidate| *candidate == chain).unwrap_or_default();
        #[expect(
            clippy::indexing_slicing,
            reason = "a position in ALL is within the array, whose length is ALL's"
        )]
        let entry = self.entries[position];
        entry
    }

    pub fn spec(&self, chain: ChainId) -> &'static ChainSpec {
        self.entry(chain).spec()
    }

    /// Every entry, in [`ChainId::ALL`] order.
    pub fn entries(&self) -> &[&'static dyn ChainEntry] {
        &self.entries
    }

    /// Reads a bare address or a payment URI for `chain`. A payment URI of another
    /// chain is refused as the wrong network rather than read as a malformed address.
    pub fn validate_address(&self, chain: ChainId, value: &str) -> Result<Address, KeyError> {
        let trimmed = self.refuse_foreign_scheme(chain, value)?;
        Ok(self.entry(chain).validate_address(trimmed)?)
    }

    /// What a send on `chain` to `value` carries as its destination, with the same
    /// refusal of another chain's payment URI as [`ChainTable::validate_address`].
    pub fn payment_target(&self, chain: ChainId, value: &str) -> Result<String, KeyError> {
        let trimmed = self.refuse_foreign_scheme(chain, value)?;
        Ok(self.entry(chain).payment_target(trimmed)?)
    }

    fn refuse_foreign_scheme<'a>(
        &self,
        chain: ChainId,
        value: &'a str,
    ) -> Result<&'a str, KeyError> {
        let trimmed = value.trim();
        let own_scheme = self.spec(chain).uri_scheme;
        if let Some((scheme, _)) = trimmed.split_once(':')
            && !scheme.eq_ignore_ascii_case(own_scheme)
            && self.entries.iter().any(|entry| scheme.eq_ignore_ascii_case(entry.spec().uri_scheme))
        {
            return Err(KeyError::Address(AddressError::WrongNetwork { chain }));
        }
        Ok(trimmed)
    }

    /// Imports a key for `chain`, and when the text is in no encoding that chain
    /// accepts, says which chain it does belong to.
    pub fn import_key(
        &self,
        chain: ChainId,
        input: &str,
        password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        match self.entry(chain).import_key(input, password) {
            Err(KeyError::UnknownFormat) => {}
            outcome => return outcome,
        }
        let claimed = self
            .entries
            .iter()
            .filter(|entry| entry.spec().chain != chain)
            .find_map(|entry| entry.recognize_key(input));
        match claimed {
            Some(format) => Err(KeyError::WrongChain { format, chain }),
            None => Err(KeyError::UnknownFormat),
        }
    }
}
