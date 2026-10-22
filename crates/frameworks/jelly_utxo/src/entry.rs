use std::fmt;

use jelly_core::{
    Address, AddressCodec, AddressError, ChainEntry, ChainSpec, ImportedKey, KeyError, KeyKind,
    KeyMaterial, ScriptKind, SecretText, payment_uri,
};
use jelly_keypair::Secp256k1Key;
use zeroize::Zeroizing;

use crate::legacy::is_legacy;
use crate::params::UtxoParams;
use crate::wif::WifFormat;

/// The one [`ChainEntry`] of every UTXO chain, which each chain crate fills with its
/// own spec, table, key format, and shipped endpoints.
pub struct UtxoEntry {
    pub spec: &'static ChainSpec,
    pub params: &'static UtxoParams,
    pub wif: WifFormat,
    /// Whether the payment request scheme is also the address's own prefix, as
    /// `CashAddr`'s is, so an address written after the scheme is read with it.
    pub is_scheme_prefix: bool,
    /// The chain's shipped `endpoints.json`.
    pub default_endpoints: &'static str,
}

// The shipped endpoints are left out, because their URLs never reach a log.
impl fmt::Debug for UtxoEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UtxoEntry").field("chain", &self.spec.chain).finish_non_exhaustive()
    }
}

impl ChainEntry for UtxoEntry {
    fn spec(&self) -> &'static ChainSpec {
        self.spec
    }

    fn codec(&self) -> &'static dyn AddressCodec {
        self.params.codec
    }

    fn default_endpoints(&self) -> &'static str {
        self.default_endpoints
    }

    /// On a chain whose scheme is the address prefix, an address after the scheme keeps
    /// it. A legacy address has no prefix of its own, so it is read bare.
    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        let trimmed = value.trim();
        let target = request_target(self.spec, trimmed)?;
        if self.is_scheme_prefix && !is_legacy(target) {
            return self.params.codec.validate(with_scheme(trimmed, target));
        }
        self.params.codec.validate(target)
    }

    /// `None` takes the chain's default flavour.
    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        let resolved = self
            .spec
            .resolve_script(None, script)?
            .ok_or(KeyError::internal("a utxo chain spec has no script kinds"))?;
        let pubkey = self.secp256k1_key(key)?.public_compressed();
        self.params.key_address(resolved, &pubkey)
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        self.wif.recognize(input)
    }

    fn import_key(
        &self,
        input: &str,
        _password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        self.wif.import(input)
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Ok(self.wif.export(&self.secp256k1_key(key)?))
    }
}

impl UtxoEntry {
    fn secp256k1_key(&self, key: &KeyMaterial) -> Result<Secp256k1Key, KeyError> {
        if key.kind() != KeyKind::Secp256k1 {
            return Err(KeyError::WrongKeyKind { chain: self.spec.chain });
        }
        Secp256k1Key::from_key_material(key)
    }
}

/// The address a BIP-21 payment request for the chain pays, or `value` itself when it is
/// no such request.
///
/// A parameter that only describes the payment, such as an amount or a label, is
/// dropped, because the send form asks for those itself. BIP-21 voids a request with a
/// `req-` parameter the client does not understand, so one refuses the whole request,
/// and dropping parameters never produces a payment its recipient cannot match.
fn request_target<'a>(spec: &ChainSpec, value: &'a str) -> Result<&'a str, AddressError> {
    let Some(uri) = payment_uri(spec.uri_scheme, value) else {
        return Ok(value);
    };
    let chain = spec.chain;
    if uri.params().any(|(name, _)| name.starts_with("req-")) {
        return Err(AddressError::UnsupportedRequest { chain });
    }
    // A request with only a lightning invoice has no address at all.
    if uri.target.is_empty() {
        return Err(AddressError::UnsupportedRequest { chain });
    }
    Ok(uri.target)
}

/// `target` with the scheme in front, when the user wrote the two together.
///
/// A position the codec reports then counts from where the user's text does. A `//`
/// between the two is not part of any address, so that form yields the bare payload.
fn with_scheme<'a>(trimmed: &'a str, target: &'a str) -> &'a str {
    let address = trimmed.split_once('?').map_or(trimmed, |(address, _)| address);
    match address.split_once(':') {
        Some((_, payload)) if payload == target => address,
        Some(_) | None => target,
    }
}
