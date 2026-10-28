use std::fmt;

use jelly_core::{KeyKind, KeyMaterial, random_bytes, wire_enum};
use zeroize::Zeroizing;

use crate::error::VaultError;

/// 16 OS-random bytes with nothing secret in them, so it's safe in `SQLite`, in a log, and
/// over IPC.
///
/// [`Display`](fmt::Display) and [`EntryId::parse`] round-trip through 32 lowercase hex
/// characters, the form the address table stores.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntryId([u8; Self::LEN]);

impl EntryId {
    pub(crate) const LEN: usize = 16;

    pub(crate) fn generate() -> Result<Self, VaultError> {
        Ok(Self(*random_bytes::<{ Self::LEN }>()?))
    }

    pub(crate) const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    pub fn parse(text: &str) -> Result<Self, VaultError> {
        let mut bytes = [0u8; Self::LEN];
        hex::decode_to_slice(text, &mut bytes).map_err(|_| VaultError::InvalidEntryId)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntryId({self})")
    }
}

wire_enum! {
    /// The kind a vault entry is listed and searched under.
    pub enum BlobKind {
        /// BIP-39 entropy of the wallet seed.
        SeedEntropy => "seed_entropy",
        /// BIP-39 passphrase, present only under `PassphrasePolicy::Stored`.
        SeedPassphrase => "seed_passphrase",
        /// Proof that an unlock supplied the same passphrase as creation.
        PassphraseVerifier => "passphrase_verifier",
        PrivateKey => "private_key",
        MoneroScanState => "monero_scan_state",
    }
}

/// What a blob holds, with the key kind for a private key.
///
/// One enum instead of a [`BlobKind`] and an optional [`KeyKind`], so a private key
/// without a key kind, or a seed with one, can't be built. Each variant holds what the
/// [`BlobKind`] variant of the same name documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobContent {
    SeedEntropy,
    SeedPassphrase,
    PassphraseVerifier,
    PrivateKey(KeyKind),
    MoneroScanState,
}

impl BlobContent {
    const fn kind(self) -> BlobKind {
        match self {
            Self::SeedEntropy => BlobKind::SeedEntropy,
            Self::SeedPassphrase => BlobKind::SeedPassphrase,
            Self::PassphraseVerifier => BlobKind::PassphraseVerifier,
            Self::PrivateKey(_) => BlobKind::PrivateKey,
            Self::MoneroScanState => BlobKind::MoneroScanState,
        }
    }

    const fn key_kind(self) -> Option<KeyKind> {
        match self {
            Self::PrivateKey(kind) => Some(kind),
            Self::SeedEntropy
            | Self::SeedPassphrase
            | Self::PassphraseVerifier
            | Self::MoneroScanState => None,
        }
    }

    /// Rebuilds the content from its on-disk pair, or `None` for a pair this type can't
    /// represent.
    pub(crate) const fn from_parts(kind: BlobKind, key_kind: Option<KeyKind>) -> Option<Self> {
        match (kind, key_kind) {
            (BlobKind::SeedEntropy, None) => Some(Self::SeedEntropy),
            (BlobKind::SeedPassphrase, None) => Some(Self::SeedPassphrase),
            (BlobKind::PassphraseVerifier, None) => Some(Self::PassphraseVerifier),
            (BlobKind::MoneroScanState, None) => Some(Self::MoneroScanState),
            (BlobKind::PrivateKey, Some(key_kind)) => Some(Self::PrivateKey(key_kind)),
            (
                BlobKind::SeedEntropy
                | BlobKind::SeedPassphrase
                | BlobKind::PassphraseVerifier
                | BlobKind::MoneroScanState,
                Some(_),
            )
            | (BlobKind::PrivateKey, None) => None,
        }
    }
}

/// One secret, wiped on drop and never serialized or printed.
///
/// Deliberately not `Clone`. Only the [`crate::UnlockedVault`] methods that return an
/// owned blob copy one.
pub struct SecretBlob {
    content: BlobContent,
    bytes: Zeroizing<Vec<u8>>,
}

impl SecretBlob {
    /// Takes ownership of `bytes` without copying.
    pub const fn new(content: BlobContent, bytes: Zeroizing<Vec<u8>>) -> Self {
        Self { content, bytes }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub const fn content(&self) -> BlobContent {
        self.content
    }

    pub(crate) const fn kind(&self) -> BlobKind {
        self.content.kind()
    }

    pub(crate) const fn key_kind(&self) -> Option<KeyKind> {
        self.content.key_kind()
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The bytes must not leave the closure in anything that doesn't zeroize.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(&self.bytes)
    }

    /// Only a private key qualifies, because a seed is derived from, not signed with.
    pub(crate) fn into_key_material(self) -> Option<KeyMaterial> {
        self.content.key_kind().map(|kind| KeyMaterial::new(kind, self.bytes))
    }

    pub(crate) fn duplicate(&self) -> Self {
        Self { content: self.content, bytes: Zeroizing::new(self.bytes.to_vec()) }
    }
}

impl fmt::Debug for SecretBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBlob({:?}, <redacted {} bytes>)", self.content, self.bytes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_differ_and_round_trip_through_their_hex_form() {
        let id = EntryId::generate().unwrap();
        assert_ne!(id, EntryId::generate().unwrap());
        assert_eq!(id.to_string().len(), EntryId::LEN.saturating_mul(2));
        assert_eq!(EntryId::parse(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn malformed_ids_are_refused() {
        let short = "aa".repeat(EntryId::LEN.saturating_sub(1));
        let long = "aa".repeat(EntryId::LEN.saturating_add(1));
        for text in ["", "zz", &short, &long] {
            assert!(matches!(EntryId::parse(text), Err(VaultError::InvalidEntryId)));
        }
        assert_eq!(VaultError::InvalidEntryId.to_string(), "a vault entry id is 32 hex characters");
    }

    #[test]
    fn content_and_parts_are_inverses_and_mismatched_parts_are_refused() {
        let all = [
            BlobContent::SeedEntropy,
            BlobContent::SeedPassphrase,
            BlobContent::PassphraseVerifier,
            BlobContent::MoneroScanState,
        ]
        .into_iter()
        .chain(KeyKind::ALL.map(BlobContent::PrivateKey));
        for content in all {
            assert_eq!(BlobContent::from_parts(content.kind(), content.key_kind()), Some(content));
        }
        assert_eq!(BlobContent::from_parts(BlobKind::PrivateKey, None), None);
        assert_eq!(BlobContent::from_parts(BlobKind::SeedEntropy, Some(KeyKind::Secp256k1)), None);
    }

    #[test]
    fn only_a_private_key_is_key_material() {
        let key = SecretBlob::new(
            BlobContent::PrivateKey(KeyKind::Secp256k1),
            Zeroizing::new(vec![9u8; 32]),
        );
        assert_eq!(key.kind(), BlobKind::PrivateKey);
        let material = key.into_key_material().unwrap();
        assert_eq!(material.kind(), KeyKind::Secp256k1);

        let seed = SecretBlob::new(BlobContent::SeedEntropy, Zeroizing::new(vec![1u8; 32]));
        assert!(seed.into_key_material().is_none());
    }

    #[test]
    fn a_duplicate_holds_the_same_secret_and_debug_redacts_it() {
        let blob = SecretBlob::new(BlobContent::MoneroScanState, Zeroizing::new(vec![0xab; 40]));
        let copy = blob.duplicate();
        assert_eq!(copy.content(), blob.content());
        copy.with_bytes(|bytes| assert_eq!(bytes, &[0xab; 40]));
        let rendered = format!("{blob:?}");
        assert!(!rendered.contains("abab"), "{rendered}");
        assert!(rendered.contains("<redacted 40 bytes>"), "{rendered}");
    }
}
