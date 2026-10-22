use std::fmt;

use jelly_core::{
    Address, AddressCodec, AddressError, ChainEntry, ChainSpec, ImportedKey, KeyError, KeyKind,
    KeyMaterial, PaymentUri, ScriptKind, SecretText, payment_uri,
};
use jelly_keypair::{
    Secp256k1Key, export_web3_key, import_web3_key, keccak_address_bytes, recognize_web3_key,
};
use zeroize::Zeroizing;

use crate::address::{EvmCodec, checksummed};
use crate::params::EvmParams;

/// The one [`ChainEntry`] of every EVM network, which each chain crate fills with its
/// own spec, network figures, codec, and shipped endpoints.
pub struct EvmEntry {
    pub spec: &'static ChainSpec,
    pub params: &'static EvmParams,
    pub codec: &'static EvmCodec,
    /// The chain's shipped `endpoints.json`.
    pub default_endpoints: &'static str,
}

// The shipped endpoints are left out, because their URLs never reach a log.
impl fmt::Debug for EvmEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmEntry").field("chain", &self.spec.chain).finish_non_exhaustive()
    }
}

impl ChainEntry for EvmEntry {
    fn spec(&self) -> &'static ChainSpec {
        self.spec
    }

    fn codec(&self) -> &'static dyn AddressCodec {
        self.codec
    }

    fn default_endpoints(&self) -> &'static str {
        self.default_endpoints
    }

    /// Reads a bare address or an EIP-681 payment request and returns the canonical
    /// spelling.
    ///
    /// A parameter that only describes the payment, such as a value, is dropped, because
    /// the send form asks for it itself.
    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        let Some(uri) = payment_uri(self.spec.uri_scheme, value) else {
            return self.codec.validate(value);
        };
        let target = request_target(self.params, &uri)?.trim();
        if target.is_empty() {
            return Err(AddressError::UnsupportedRequest { chain: self.params.chain });
        }
        self.codec.validate(target)
    }

    /// The checksummed address `key` holds, which is the same on every EVM network. An
    /// EVM address has no script type.
    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        let chain = self.params.chain;
        if script.is_some() {
            return Err(KeyError::ScriptNotApplicable { chain });
        }
        if key.kind() != KeyKind::Secp256k1 {
            return Err(KeyError::WrongKeyKind { chain });
        }
        let key = Secp256k1Key::from_key_material(key)?;
        Ok(checksummed(keccak_address_bytes(&key.public_uncompressed())?))
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        recognize_web3_key(input)
    }

    fn import_key(
        &self,
        input: &str,
        password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        import_web3_key(input, password)
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        export_web3_key(key, self.params.chain)
    }
}

/// EIP-681 puts an optional `pay-` prefix before the address, and an `@` chain id and a
/// `/` function after it.
///
/// An ERC-20 `transfer` request targets the token contract and names the payee in its
/// `address` parameter. Any other function call is refused, because its address
/// parameter is not a payee.
fn request_target<'u>(
    params: &EvmParams,
    uri: &'u PaymentUri<'_>,
) -> Result<&'u str, AddressError> {
    let chain = params.chain;
    let target = uri.target.strip_prefix("pay-").unwrap_or(uri.target);
    let (target, function) = match target.split_once('/') {
        Some((target, function)) => (target, Some(function)),
        None => (target, None),
    };
    let address = match target.split_once('@') {
        Some((address, id)) => {
            if chain_id_of(id) != Some(params.chain_id) {
                return Err(AddressError::WrongNetwork { chain });
            }
            address
        }
        None => target,
    };
    match function {
        None => Ok(address),
        Some("transfer") => uri.param("address").ok_or(AddressError::UnsupportedRequest { chain }),
        Some(_) => Err(AddressError::UnsupportedRequest { chain }),
    }
}

/// EIP-681 writes the chain id in decimal, and some wallets write it in hex.
fn chain_id_of(text: &str) -> Option<u64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eip_681_chain_ids_read_in_decimal_or_hex() {
        assert_eq!(chain_id_of("56"), Some(56));
        assert_eq!(chain_id_of("0x38"), Some(56));
        assert_eq!(chain_id_of("0X38"), Some(56));
        assert_eq!(chain_id_of("bsc"), None);
        assert_eq!(chain_id_of(""), None);
    }
}
