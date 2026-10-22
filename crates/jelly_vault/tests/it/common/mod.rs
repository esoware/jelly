use std::error::Error;

use jelly_core::KeyKind;
use jelly_vault::{
    BlobContent, BlobKind, EntryId, SecretBlob, UnlockedVault, test_util::TempVault,
};
use zeroize::Zeroizing;

pub(crate) const WALLET_ID: &str = "jelly-test-wallet";

pub(crate) fn blob(content: BlobContent, byte: u8, length: usize) -> SecretBlob {
    SecretBlob::new(content, Zeroizing::new(vec![byte; length]))
}

pub(crate) fn seed() -> SecretBlob {
    blob(BlobContent::SeedEntropy, 0x11, 32)
}

pub(crate) fn key() -> SecretBlob {
    blob(BlobContent::PrivateKey(KeyKind::Secp256k1), 0x22, 32)
}

pub(crate) fn filled() -> Result<TempVault, Box<dyn Error>> {
    let fixture = TempVault::new(WALLET_ID)?;
    let mut vault = fixture.create()?;
    vault.insert(seed())?;
    vault.insert(key())?;
    vault.lock();
    Ok(fixture)
}

/// Every entry of `kind`, in id order.
pub(crate) fn ids_of_kind(vault: &UnlockedVault, kind: BlobKind) -> Vec<EntryId> {
    vault.entries_of_kind(kind).map(|(id, _)| id).collect()
}
