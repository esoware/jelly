use std::collections::BTreeMap;
use std::sync::Arc;

use hmac::{Hmac, KeyInit as _, Mac as _};
use jelly_core::{
    ChainId, ChainTable, CoreError, KeyMaterial, PassphrasePolicy, PhraseWords, SecretText,
};
use jelly_keypair::{HdKeychain, PathTemplate, Seed};
use jelly_mnemonic::Mnemonic;
use jelly_store::WalletSettingsRow;
use jelly_vault::{BlobContent, BlobKind, SecretBlob, UnlockedVault, Vault, VaultPassword};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::blocking;
use crate::wallet::PassphraseInput;

/// Domain string of the passphrase verifier.
///
/// Every existing wallet's verifier was computed under this exact string, so changing it
/// makes every wallet refuse its own passphrase. The tests below pin the tag it
/// produces.
const VERIFIER_LABEL: &[u8] = b"jelly-engine/v1/passphrase-verifier";

/// Where a new seed wallet's phrase comes from.
pub(crate) enum SeedSource {
    Generate(PhraseWords),
    Phrase(SecretText),
}

/// The keys a wallet's seed gives its session at one account index.
pub(crate) struct SeedKeys {
    pub(crate) keychain: HdKeychain,
    pub(crate) account: u32,
    /// The account key of every chain whose spec needs one to scan. Adapters share these
    /// until a lock retires them.
    pub(crate) account_keys: BTreeMap<ChainId, Arc<KeyMaterial>>,
}

impl SeedKeys {
    fn derive(keychain: HdKeychain, account: u32, table: &ChainTable) -> Result<Self, CoreError> {
        let mut account_keys = BTreeMap::new();
        for entry in table.entries() {
            let spec = entry.spec();
            if !spec.needs_account_key {
                continue;
            }
            let path = PathTemplate::parse(spec.default_path)?.path_at(account)?;
            let key = keychain.key_material_at(spec.key_kind, &path)?;
            account_keys.insert(spec.chain, Arc::new(key));
        }
        Ok(Self { keychain, account, account_keys })
    }
}

/// What a seed wallet's vault is created with.
pub(crate) struct SeedSecrets {
    entropy: Zeroizing<Vec<u8>>,
    /// Only a passphrase the wallet stores rather than asks for at every unlock.
    passphrase: Option<Zeroizing<String>>,
    verifier: Zeroizing<Vec<u8>>,
}

/// What a new seed wallet's vault is created with, and the keys its first session opens
/// with.
pub(crate) struct NewSeed {
    pub(crate) secrets: SeedSecrets,
    pub(crate) keys: SeedKeys,
}

/// Turns a phrase into the phrase to hand out and everything the new wallet needs, on
/// the blocking pool, because the PBKDF2 seed stretch and the account keys are real
/// work.
pub(crate) async fn derive_seed(
    source: SeedSource,
    passphrase: PassphraseInput,
    account: u32,
    table: &'static ChainTable,
) -> Result<(Zeroizing<String>, NewSeed), CoreError> {
    blocking(move || {
        let mnemonic = match source {
            SeedSource::Generate(words) => Mnemonic::generate(words)?,
            SeedSource::Phrase(phrase) => phrase.with_str(Mnemonic::parse)?,
        };
        let phrase = mnemonic.phrase();
        let entropy = mnemonic.with_entropy(|bytes| Zeroizing::new(bytes.to_vec()));
        let passphrase_text = Zeroizing::new(
            passphrase.value().map_or_else(String::new, |value| value.with_str(str::to_owned)),
        );
        let keychain = HdKeychain::from_seed(mnemonic.to_seed(&passphrase_text));
        drop(mnemonic);
        let verifier = verifier_tag(keychain.seed())?;
        let keys = SeedKeys::derive(keychain, account, table)?;
        let stored_passphrase = match passphrase.policy() {
            PassphrasePolicy::Stored => Some(passphrase_text),
            PassphrasePolicy::None | PassphrasePolicy::Prompt => None,
        };
        let secrets = SeedSecrets { entropy, passphrase: stored_passphrase, verifier };
        Ok::<_, CoreError>((phrase, NewSeed { secrets, keys }))
    })
    .await
}

/// Creates a wallet's vault. A seed wallet's is filled in the same blocking pass, so no
/// caller ever sees it half-populated.
pub(crate) async fn provision(
    handle: Vault,
    password: VaultPassword,
    secrets: SeedSecrets,
) -> Result<UnlockedVault, CoreError> {
    blocking(move || {
        let mut vault = handle.create(&password)?;
        let SeedSecrets { entropy, passphrase, verifier } = secrets;
        vault.insert(SecretBlob::new(BlobContent::SeedEntropy, entropy))?;
        if let Some(passphrase) = passphrase {
            let bytes = Zeroizing::new(passphrase.as_bytes().to_vec());
            vault.insert(SecretBlob::new(BlobContent::SeedPassphrase, bytes))?;
        }
        vault.insert(SecretBlob::new(BlobContent::PassphraseVerifier, verifier))?;
        Ok::<_, CoreError>(vault)
    })
    .await
}

/// Opens the vault and derives the session's keys in one blocking pass, because the KDF,
/// the whole-file decryption, and the seed stretch all block.
pub(crate) async fn unlock_vault_and_keys(
    handle: Vault,
    password: VaultPassword,
    settings: WalletSettingsRow,
    supplied: Option<SecretText>,
    table: &'static ChainTable,
) -> Result<(UnlockedVault, SeedKeys), CoreError> {
    blocking(move || {
        let vault = handle.unlock(&password)?;
        let keychain = open_keychain(&vault, settings.passphrase_policy, supplied)?;
        let keys = SeedKeys::derive(keychain, settings.account_index, table)?;
        Ok::<_, CoreError>((vault, keys))
    })
    .await
}

fn open_keychain(
    vault: &UnlockedVault,
    policy: PassphrasePolicy,
    supplied: Option<SecretText>,
) -> Result<HdKeychain, CoreError> {
    let entropy = vault.read_first(BlobKind::SeedEntropy).ok_or_else(|| {
        CoreError::internal_without_source("this wallet has no seed in its vault")
    })?;
    let passphrase_text = match policy {
        PassphrasePolicy::None => Zeroizing::new(String::new()),
        PassphrasePolicy::Stored => vault
            .read_first(BlobKind::SeedPassphrase)
            .ok_or(CoreError::PassphraseRequired)?
            .with_bytes(passphrase_text)?,
        PassphrasePolicy::Prompt => {
            let value = supplied.ok_or(CoreError::PassphraseRequired)?;
            Zeroizing::new(value.with_str(str::to_owned))
        }
    };
    let mnemonic = entropy.with_bytes(Mnemonic::from_entropy)?;
    Ok(HdKeychain::from_seed(mnemonic.to_seed(&passphrase_text)))
}

/// A stored passphrase blob as the text it was stored from.
pub(crate) fn passphrase_text(bytes: &[u8]) -> Result<Zeroizing<String>, CoreError> {
    str::from_utf8(bytes)
        .map(|text| Zeroizing::new(text.to_owned()))
        .map_err(|_| CoreError::internal_without_source("the stored passphrase is not text"))
}

/// HMAC-SHA256 of a fixed label under the expanded seed.
///
/// The seed already carries the passphrase, so a tag that verifies proves the
/// same passphrase was supplied. This exists because BIP-39 has no wrong-passphrase
/// detection of its own, since every passphrase yields a valid seed and a disjoint
/// wallet.
fn verifier_tag(seed: &Seed) -> Result<Zeroizing<Vec<u8>>, CoreError> {
    let mut mac = seed.with_bytes(|bytes| Hmac::<Sha256>::new_from_slice(bytes)).map_err(|_| {
        CoreError::internal_without_source("the passphrase verifier key is the wrong length")
    })?;
    mac.update(VERIFIER_LABEL);
    Ok(Zeroizing::new(mac.finalize().into_bytes().to_vec()))
}

/// Constant-time verification, through the MAC's own comparison.
pub(crate) fn is_verifier_match(seed: &Seed, tag: &[u8]) -> bool {
    seed.with_bytes(|bytes| {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(bytes) else {
            return false;
        };
        mac.update(VERIFIER_LABEL);
        mac.verify_slice(tag).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag is an on-disk value, so a change to the label, the MAC, or the seed
    /// accessor has to fail here first.
    #[test]
    fn the_verifier_tag_is_frozen() {
        let seed = Seed::from_boxed(Box::new([7u8; Seed::LEN]));
        let tag = verifier_tag(&seed).expect("the mac accepts a 64 byte key");
        assert_eq!(
            hex::encode(tag.as_slice()),
            "c9863bcb4ed9698c1086ca8f6db4cd8f7417acdba78f87299de61c9982b204d1"
        );
        assert!(is_verifier_match(&seed, &tag));

        let other = Seed::from_boxed(Box::new([8u8; Seed::LEN]));
        assert!(!is_verifier_match(&other, &tag));
    }
}
