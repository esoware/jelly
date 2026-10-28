use alloy::primitives::Address as EvmAddress;
use jelly_core::{Address, AddressCodec, AddressError, ChainId};
use jelly_keypair::keccak_address_bytes;

use crate::error::EvmError;

/// A key has the same address on every EVM network, so the chain only tags errors
/// and the [`Address`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvmCodec {
    chain: ChainId,
}

impl EvmCodec {
    pub const fn new(chain: ChainId) -> Self {
        Self { chain }
    }
}

impl AddressCodec for EvmCodec {
    fn chain(&self) -> ChainId {
        self.chain
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        let bytes = evm_address_bytes(self.chain, value)?;
        Ok(Address::assume_canonical(self.chain, checksummed(bytes)))
    }
}

/// The EIP-55 spelling of an address.
pub(crate) fn checksummed(bytes: [u8; 20]) -> String {
    EvmAddress::from(bytes).to_checksum(None)
}

/// Mixed-case input must carry a valid EIP-55 checksum. Single-case input is
/// accepted, as other wallets accept it, though alloy's `parse_checksummed` would
/// refuse it. `chain` only tags the error.
pub(crate) fn evm_address_bytes(chain: ChainId, value: &str) -> Result<[u8; 20], AddressError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain });
    }
    let body = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")).unwrap_or(trimmed);
    if body.len() != 40 {
        return Err(AddressError::WrongLength { chain });
    }
    if let Some(offset) = body.bytes().position(|byte| !byte.is_ascii_hexdigit()) {
        let position = trimmed.len().saturating_sub(body.len()).saturating_add(offset);
        return Err(AddressError::Malformed { chain, position });
    }
    let address: EvmAddress =
        body.parse().map_err(|_| AddressError::Malformed { chain, position: 0 })?;
    let is_mixed_case = body.bytes().any(|byte| byte.is_ascii_uppercase())
        && body.bytes().any(|byte| byte.is_ascii_lowercase());
    if is_mixed_case && address.to_checksum_buffer(None).as_str().get(2..) != Some(body) {
        return Err(AddressError::Checksum { chain });
    }
    Ok(address.into_array())
}

pub(crate) fn parse(chain: ChainId, text: &str) -> Result<EvmAddress, EvmError> {
    let bytes = evm_address_bytes(chain, text)
        .map_err(|error| EvmError::InvalidAddress { detail: error.to_string() })?;
    Ok(EvmAddress::from(bytes))
}

/// Derived by the same code as the address the wallet shows, so signing checks the
/// key against the address the user was given.
pub(crate) fn of_public_key(public_uncompressed: &[u8]) -> Result<EvmAddress, EvmError> {
    Ok(EvmAddress::from(keccak_address_bytes(public_uncompressed).map_err(EvmError::Signing)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

    #[test]
    fn a_checksummed_address_parses_on_both_networks_to_the_same_bytes() {
        let ethereum = parse(ChainId::Ethereum, OWNER).expect("valid");
        let bsc = parse(ChainId::Bsc, OWNER).expect("valid");
        assert_eq!(ethereum, bsc, "one address format serves both networks");
        assert_eq!(ethereum.to_string(), OWNER, "the canonical spelling is checksummed");
    }

    #[test]
    fn a_lowercase_address_is_accepted_and_re_checksummed() {
        let parsed = parse(ChainId::Ethereum, &OWNER.to_ascii_lowercase()).expect("valid");
        assert_eq!(parsed.to_string(), OWNER);
    }

    #[test]
    fn a_broken_checksum_and_a_short_address_are_both_refused() {
        let mangled = OWNER.replacen("d8dA", "D8dA", 1);
        assert!(parse(ChainId::Ethereum, &mangled).is_err(), "a mixed-case checksum is checked");
        parse(ChainId::Bsc, "0x1234").unwrap_err();
        parse(ChainId::Ethereum, "").unwrap_err();
    }

    #[test]
    fn a_codec_tags_its_answers_with_its_own_chain() {
        let codec = EvmCodec::new(ChainId::Bsc);
        assert_eq!(codec.chain(), ChainId::Bsc);
        let address = codec.validate(&OWNER.to_ascii_lowercase()).expect("valid");
        assert_eq!(address.chain(), ChainId::Bsc);
        assert_eq!(address.value(), OWNER);
        assert_eq!(
            codec.validate("0x1234"),
            Err(AddressError::WrongLength { chain: ChainId::Bsc })
        );
    }
}
