use bs58::decode::Error as Base58Error;
use jelly_core::{Address, AddressCodec, AddressError, ChainId};
use solana_pubkey::Pubkey;

use crate::config::CHAIN;
use crate::error::SolanaError;

pub(crate) const CODEC: &dyn AddressCodec = &SolanaCodec;

#[derive(Debug)]
struct SolanaCodec;

impl AddressCodec for SolanaCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        let trimmed = value.trim();
        address_bytes(trimmed)?;
        Ok(Address::assume_canonical(CHAIN, trimmed))
    }
}

/// Base58 of the 32-byte ed25519 public key.
///
/// Program-derived addresses are deliberately off the curve, so no curve check is
/// applied here or in [`address_bytes`].
pub(crate) fn encode(pubkey: &[u8; 32]) -> String {
    bs58::encode(pubkey).into_string()
}

/// The workspace's only Solana address decoder. It trims surrounding whitespace and
/// refuses anything else malformed.
fn address_bytes(value: &str) -> Result<[u8; 32], AddressError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    let raw = bs58::decode(trimmed).into_vec().map_err(|error| {
        // The decoder stops at the first invalid character, so its index is the position.
        let position = if let Base58Error::InvalidCharacter { index, .. }
        | Base58Error::NonAsciiCharacter { index } = error
        {
            index
        } else {
            0
        };
        AddressError::Malformed { chain: CHAIN, position }
    })?;
    raw.as_slice().try_into().map_err(|_| AddressError::WrongLength { chain: CHAIN })
}

pub(crate) fn parse(field: &'static str, value: &str) -> Result<Pubkey, SolanaError> {
    address_bytes(value)
        .map(Pubkey::from)
        .map_err(|reason| SolanaError::InvalidAddress { field, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::OWNER;

    #[test]
    fn a_pasted_address_is_trimmed_and_read_back_canonically() {
        let key = parse("recipient", &format!("  {OWNER}\n")).expect("it parses");
        assert_eq!(key, OWNER);
    }

    #[test]
    fn anything_that_is_not_thirty_two_bytes_of_base58_is_refused() {
        for bad in ["", "not an address", "0xdAC17F958D2ee523a2206206994597C13D831ec7", "111"] {
            let error = parse("recipient", bad).expect_err(bad);
            assert!(matches!(error, SolanaError::InvalidAddress { .. }), "{error:?}");
            assert!(!error.to_string().contains(bad) || bad.is_empty(), "{error}");
        }
    }

    #[test]
    fn an_encoded_key_decodes_to_the_same_bytes() {
        let pubkey = OWNER.to_bytes();
        let address = encode(&pubkey);
        assert_eq!(address, OWNER.to_string());
        assert_eq!(address_bytes(&address).expect("bytes"), pubkey);
    }

    #[test]
    fn an_invalid_character_is_reported_at_its_own_position() {
        for (value, position) in
            [("HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpq0", 43), ("ab\u{e9}c", 2)]
        {
            assert_eq!(
                address_bytes(value),
                Err(AddressError::Malformed { chain: CHAIN, position }),
                "{value}"
            );
        }
    }
}
