use std::fmt;

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Deserializer};
#[cfg(feature = "ipc")]
use specta::datatype::DataType;
use zeroize::{Zeroize as _, Zeroizing};

use crate::wire_enum;

wire_enum! {
    /// The curve and role of a private key.
    pub enum KeyKind {
        /// secp256k1 scalar for the UTXO chains, the EVM networks, TRON, and XRP.
        Secp256k1 => "secp256k1",
        /// Solana's and TON's ed25519 seed as SLIP-10 produces it, not a clamped scalar.
        Ed25519Seed => "ed25519_seed",
        /// Monero spend scalar, already reduced mod l. The view key derives from it.
        MoneroSpend => "monero_spend",
    }
}

/// Private key bytes wiped on drop. The signer validates the length for `kind`.
pub struct KeyMaterial {
    kind: KeyKind,
    secret: Zeroizing<Vec<u8>>,
}

impl KeyMaterial {
    /// Takes ownership without copying secret bytes.
    pub const fn new(kind: KeyKind, bytes: Zeroizing<Vec<u8>>) -> Self {
        Self { kind, secret: bytes }
    }

    pub const fn kind(&self) -> KeyKind {
        self.kind
    }

    #[cfg(feature = "test-util")]
    pub fn len(&self) -> usize {
        self.secret.len()
    }

    #[cfg(feature = "test-util")]
    pub fn is_empty(&self) -> bool {
        self.secret.is_empty()
    }

    /// Borrows the key within the closure. Any retained copy must use zeroizing storage.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(&self.secret)
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyMaterial({:?}, <redacted {} bytes>)", self.kind, self.secret.len())
    }
}

/// A secret string that is never logged or displayed. It deserializes for IPC inputs
/// but deliberately never serializes.
#[derive(Clone)]
pub struct SecretText(SecretString);

impl SecretText {
    /// Copies `value` into an exactly sized secret and wipes the argument, spare capacity
    /// included.
    pub fn new(mut value: String) -> Self {
        let secret = SecretString::from(value.as_str());
        value.zeroize();
        Self(secret)
    }

    /// Borrows the text within the closure. Any retained copy must use zeroizing storage.
    pub fn with_str<R>(&self, read: impl FnOnce(&str) -> R) -> R {
        read(self.0.expose_secret())
    }
}

impl PartialEq for SecretText {
    fn eq(&self, other: &Self) -> bool {
        self.with_str(|text| other.with_str(|other| text == other))
    }
}

impl Eq for SecretText {}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretText(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(feature = "ipc")]
impl specta::Type for SecretText {
    fn definition(types: &mut specta::Types) -> DataType {
        <String as specta::Type>::definition(types)
    }
}
