use std::fmt;

use zeroize::Zeroizing;

use crate::address::{Address, AddressCodec, AddressError};
use crate::chain::ScriptKind;
use crate::error::KeyError;
use crate::secret::{KeyMaterial, SecretText};
use crate::spec::ChainSpec;
use crate::uri::payment_uri;

/// A private key read from user input, with what its encoding implied.
#[derive(Debug)]
pub struct ImportedKey {
    pub material: KeyMaterial,
    /// The address type the encoding named, as Electrum's `p2wpkh:` prefix does. A key
    /// from any other source names none.
    pub named_script: Option<ScriptKind>,
}

/// The one thing a chain crate must implement: its spec, its address codec, and the key
/// operations the engine performs in that chain's shape. Pure, with no network access.
///
/// Adapter construction is not here, because it needs the network layer, which the
/// engine never sees. Each chain crate exports a `build` function for it, and the
/// registry dispatches to them.
pub trait ChainEntry: Send + Sync + fmt::Debug {
    fn spec(&self) -> &'static ChainSpec;

    fn codec(&self) -> &'static dyn AddressCodec;

    /// The chain's shipped `endpoints.json`, the curated defaults until the user adds
    /// endpoints.
    fn default_endpoints(&self) -> &'static str;

    /// Reads a bare address or a payment URI for this chain and returns the canonical
    /// spelling.
    ///
    /// The default strips the scheme and ignores every parameter. A chain whose payment
    /// requests can carry something the payment depends on, which Jelly cannot send,
    /// overrides this to refuse those.
    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        let target = payment_uri(self.spec().uri_scheme, value)
            .map_or_else(|| value.trim(), |uri| uri.target);
        self.codec().validate(target)
    }

    /// What a send to `value` carries as its destination: the canonical address, unless
    /// the chain's payment requests carry something the adapter itself must read, such as
    /// a comment the recipient credits the payment by, in which case the validated request
    /// as the user entered it.
    fn payment_target(&self, value: &str) -> Result<String, AddressError> {
        self.validate_address(value).map(Address::into_value)
    }

    /// The address `key` produces. `script` has already been resolved through
    /// [`ChainSpec::resolve_script`], so a UTXO chain receives `Some` and the rest
    /// `None`.
    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError>;

    /// The name of the key format `input` is written in, when it is one this chain
    /// accepts, found without decoding or decrypting a secret. The registry uses it to
    /// tell a user which chain a pasted key belongs to.
    ///
    /// A chain whose scanner watches only the wallet's own account, as
    /// [`ChainSpec::needs_account_key`] marks, holds no standalone keys and keeps this
    /// default.
    fn recognize_key(&self, _input: &str) -> Option<&'static str> {
        None
    }

    /// Imports a private key in any encoding this chain accepts. An encrypted format
    /// with no password yields [`KeyError::PasswordRequired`], and text in no accepted
    /// encoding yields [`KeyError::UnknownFormat`].
    ///
    /// A chain that holds no standalone keys keeps this default, which accepts nothing.
    fn import_key(
        &self,
        _input: &str,
        _password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        Err(KeyError::UnknownFormat)
    }

    /// `key` in the chain's usual export encoding, such as WIF or hex.
    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError>;

    /// The subaddress at `(major, minor)` of the account `key` holds, for a chain with
    /// subaddresses.
    fn subaddress(
        &self,
        _account: &KeyMaterial,
        _major: u32,
        _minor: u32,
    ) -> Result<String, KeyError> {
        Err(KeyError::NoSubaddresses { chain: self.spec().chain })
    }

    /// The signing key behind the subaddress at `(major, minor)`.
    fn subaddress_key(
        &self,
        _account: &KeyMaterial,
        _major: u32,
        _minor: u32,
    ) -> Result<KeyMaterial, KeyError> {
        Err(KeyError::NoSubaddresses { chain: self.spec().chain })
    }

    /// The chain's own seed phrase for the account `key` holds, for a chain whose
    /// ecosystem has one, such as Monero's 25 words.
    fn account_seed_words(&self, _account: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Err(KeyError::NoSeedWords { chain: self.spec().chain })
    }
}
