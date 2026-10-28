use jelly_core::{ChainId, CoreError};
use jelly_mnemonic::Mnemonic;
use jelly_vault::{BlobKind, SecretBlob, VaultPassword};
use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::error::blocking;
use crate::{WalletEngine, derive, seed};

impl WalletEngine {
    /// The wallet's BIP-39 phrase. The vault checks the password itself, so a caller
    /// can't skip it.
    pub async fn export_seed_phrase(
        &self,
        password: SecretString,
    ) -> Result<Zeroizing<String>, CoreError> {
        let entropy = self.reauthed_export(password, BlobKind::SeedEntropy).await?;
        let phrase = blocking(move || {
            entropy.with_bytes(Mnemonic::from_entropy).map(|mnemonic| mnemonic.phrase())
        })
        .await?;
        tracing::warn!(target: "jelly::engine::export", "seed phrase exported");
        Ok(phrase)
    }

    pub async fn export_seed_passphrase(
        &self,
        password: SecretString,
    ) -> Result<Zeroizing<String>, CoreError> {
        let blob = self.reauthed_export(password, BlobKind::SeedPassphrase).await?;
        let passphrase = blob.with_bytes(seed::passphrase_text)?;
        tracing::warn!(target: "jelly::engine::export", "seed passphrase exported");
        Ok(passphrase)
    }

    /// The raw BIP-39 entropy behind the phrase.
    pub async fn export_seed_entropy_hex(
        &self,
        password: SecretString,
    ) -> Result<Zeroizing<String>, CoreError> {
        let entropy = self.reauthed_export(password, BlobKind::SeedEntropy).await?;
        tracing::warn!(target: "jelly::engine::export", "seed entropy exported");
        Ok(entropy.with_bytes(|bytes| Zeroizing::new(hex::encode(bytes))))
    }

    /// The account's own seed words on a chain that has them, such as Monero's 25 words.
    pub async fn export_account_seed(
        &self,
        chain: ChainId,
        password: SecretString,
    ) -> Result<Zeroizing<String>, CoreError> {
        let session = self.require_session().await?;
        session.verify_password(password).await?;
        let account = session.account_key(chain)?;
        let seed = session.entry(chain).account_seed_words(account)?;
        tracing::warn!(
            target: "jelly::engine::export",
            chain = chain.as_str(),
            "account seed exported"
        );
        Ok(seed)
    }

    /// One address's private key in the chain's usual export encoding.
    pub async fn export_private_key(
        &self,
        address_id: i64,
        password: SecretString,
    ) -> Result<Zeroizing<String>, CoreError> {
        let session = self.require_session().await?;
        let row = self.require_row(session.wallet_id(), address_id).await?;
        session.verify_password(password).await?;
        let material = derive::resolve_key(&session, &row)?;
        let key = session.entry(row.chain).export_key(&material)?;
        tracing::warn!(target: "jelly::engine::export", address_id, "private key exported");
        Ok(key)
    }

    /// The blob of `kind`, behind a password check the vault performs itself.
    async fn reauthed_export(
        &self,
        password: SecretString,
        kind: BlobKind,
    ) -> Result<SecretBlob, CoreError> {
        let session = self.require_session().await?;
        let password = VaultPassword::new(password);
        session.vault_reauth(move |vault| vault.export(&password, kind)).await
    }
}
