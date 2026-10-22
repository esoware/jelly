use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Address, ChainId, ChainSpec, ChainTable, CoreError, DerivationPath, ImportedKey, KeyError,
    KeyKind, KeyMaterial, KeySource, ScriptKind, SecretText,
};
use jelly_keypair::{
    Bip32Path, PathTemplate, account_prefix, child_index, generate_key, parse_path, purpose_of,
    stored_path,
};
use jelly_mnemonic::Mnemonic;
use jelly_store::{AddressRow, NewAddress};
use jelly_vault::{BlobContent, EntryId, SecretBlob};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use tokio::time::timeout;
use zeroize::Zeroizing;

use crate::error::blocking;
use crate::session::Session;
use crate::{WalletEngine, derive};

/// The typed confirmation an irreversible key deletion requires.
const DELETE_KEY_CONFIRMATION: &str = "DELETE";

/// How many times an index allocation re-reads and re-derives after losing a
/// race for the same address.
const ALLOCATION_ATTEMPTS: u32 = 8;

/// How long adding an address waits for the chain to say which other flavours of its
/// key are in use, before it returns without them.
const SCRIPT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// One address of the open wallet.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressRecord {
    pub id: i64,
    pub chain: ChainId,
    pub address: String,
    pub source: KeySource,
    pub script_kind: Option<ScriptKind>,
    pub label: Option<String>,
    pub derivation_index: Option<u32>,
}

impl From<AddressRow> for AddressRecord {
    fn from(row: AddressRow) -> Self {
        Self {
            id: row.id,
            chain: row.chain,
            address: row.address,
            source: row.source,
            script_kind: row.script_kind,
            label: row.label,
            derivation_index: row.derivation_index,
        }
    }
}

impl WalletEngine {
    /// Every address of the open wallet, each re-derived and compared before it is
    /// returned, because a row is a claim and its key is the proof.
    pub async fn list_addresses(
        &self,
        chain: Option<ChainId>,
    ) -> Result<Vec<AddressRecord>, CoreError> {
        let session = self.require_session().await?;
        let rows = self.inner.store.list_addresses(session.wallet_id(), chain).await?;
        let rows = derive::verify_rows(Arc::clone(&session), rows).await?;
        Ok(rows.into_iter().map(AddressRecord::from).collect())
    }

    /// The lowest free index of the chain's default family, or of the requested script
    /// flavour's family, so a removed address is the next one handed out again.
    pub async fn add_address(
        &self,
        chain: ChainId,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let session = self.require_session().await?;
        let spec = self.inner.table.spec(chain);
        if spec.has_subaddresses {
            return self.add_subaddress_for(&session, chain, script, label).await;
        }
        let family = family_for(spec, script)?;
        // TON's wallet versions share one path family, so an index is taken only by an
        // address of the same flavour.
        let flavour = spec.resolve_script(None, script)?;
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            let rows = self.inner.store.list_addresses(session.wallet_id(), Some(chain)).await?;
            let used_paths =
                rows.iter().filter(|row| row.script_kind == flavour).filter_map(|row| {
                    match row.source {
                        KeySource::Derived { ref path } => Some(path),
                        KeySource::Imported { .. } | KeySource::Subaddress { .. } => None,
                    }
                });
            let path = family.path_at(family.next_index(used_paths))?;
            let outcome = self.insert_derived(&session, chain, &path, script, label.clone()).await;
            match outcome {
                Err(CoreError::AlreadyExists { .. }) if attempt < ALLOCATION_ATTEMPTS => {}
                Ok(added) => {
                    self.adopt_used_scripts(&session, &added).await;
                    return Ok(added);
                }
                other => return other,
            }
        }
    }

    /// An address at a path the caller names.
    pub async fn add_address_at(
        &self,
        chain: ChainId,
        path: &DerivationPath,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let session = self.require_session().await?;
        if self.inner.table.spec(chain).has_subaddresses {
            return Err(CoreError::Unsupported(
                "this chain's addresses are subaddresses of the wallet account, not derivations",
            ));
        }
        let path = parse_path(path.as_str())?;
        let added = self.insert_derived(&session, chain, &path, script, label).await?;
        self.adopt_used_scripts(&session, &added).await;
        Ok(added)
    }

    /// The lowest free subaddress under the wallet's account. `(0, 0)` is the account's
    /// primary address, so the first call hands that out.
    async fn add_subaddress_for(
        &self,
        session: &Session,
        chain: ChainId,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let entry = session.entry(chain);
        let account = session.account_key(chain)?;
        // A subaddress chain has no script flavours, so a requested one is refused as it
        // would be on a path.
        entry.spec().resolve_script(None, script)?;
        let major = session.account();
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            let rows = self.inner.store.list_addresses(session.wallet_id(), Some(chain)).await?;
            let minor = next_minor(&rows, major)?;
            let address = entry.subaddress(account, major, minor)?;
            let new = NewAddress {
                wallet_id: session.wallet_id(),
                chain,
                address,
                source: KeySource::Subaddress { major, minor },
                script_kind: None,
                derivation_index: Some(minor),
                label: label.clone(),
            };
            match self.insert(new).await {
                Err(CoreError::AlreadyExists { .. }) if attempt < ALLOCATION_ATTEMPTS => {}
                Ok(added) => {
                    self.register_subaddress(chain, major, minor).await;
                    return Ok(added);
                }
                other => return other,
            }
        }
    }

    /// A fresh standalone key for `chain`, kept in the vault.
    pub async fn create_imported_address(
        &self,
        chain: ChainId,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let session = self.require_session().await?;
        let spec = self.inner.table.spec(chain);
        require_standalone_keys(spec)?;
        let material = generate_key(spec.key_kind)?;
        self.store_imported(&session, chain, material, script, label).await
    }

    /// Imports a private key in any supported encoding. `key_password` is the
    /// keystore-v3 password and is ignored by every other format.
    ///
    /// With no `script`, the address type the key names, as Electrum's `p2wpkh:` prefix
    /// does, wins over the chain default, the way a path's purpose does.
    pub async fn import_private_key(
        &self,
        chain: ChainId,
        input: SecretText,
        key_password: Option<SecretString>,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let session = self.require_session().await?;
        require_standalone_keys(self.inner.table.spec(chain))?;
        let password =
            key_password.map(|password| SecretText::new(password.expose_secret().to_owned()));
        let table = self.inner.table;
        // A keystore file's KDF can run for a second, which does not belong on a runtime
        // worker.
        let imported = blocking(move || {
            input.with_str(|input| import_key(table, chain, input, password.as_ref()))
        })
        .await?;
        let ImportedKey { material, named_script } = imported;
        let added =
            self.store_imported(&session, chain, material, script.or(named_script), label).await?;
        self.adopt_used_scripts(&session, &added).await;
        Ok(added)
    }

    pub async fn set_address_label(
        &self,
        address_id: i64,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let session = self.require_session().await?;
        self.require_row(session.wallet_id(), address_id).await?;
        let row = self.inner.store.set_address_label(address_id, label).await?;
        Ok(AddressRecord::from(row))
    }

    /// Removes an address the seed derives, with its balances and history. The seed
    /// still holds its key, so adding the address again brings it back. So does a
    /// standalone key that another address still holds.
    pub async fn delete_address(&self, address_id: i64) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        let row = self.require_row(session.wallet_id(), address_id).await?;
        if matches!(row.source, KeySource::Imported { .. })
            && self.key_holders(session.wallet_id(), &row).await?.len() < 2
        {
            return Err(CoreError::Unsupported("a standalone key is deleted along with its key"));
        }
        Ok(self.inner.store.delete_address(address_id).await?)
    }

    /// Deletes a standalone key and every address it holds. Unrecoverable, so it takes
    /// the vault password and the typed confirmation.
    pub async fn delete_key(
        &self,
        address_id: i64,
        password: SecretString,
        confirmation: &str,
    ) -> Result<(), CoreError> {
        require_confirmation(confirmation)?;
        let session = self.require_session().await?;
        let row = self.require_row(session.wallet_id(), address_id).await?;
        let KeySource::Imported { ref vault_entry } = row.source else {
            return Err(CoreError::Unsupported("only a standalone key can be deleted"));
        };
        let entry = EntryId::parse(vault_entry)?;
        session.verify_password(password).await?;
        // The rows go first, so a failed vault write leaves an entry nothing reads rather
        // than a row whose key is gone.
        for holder in self.key_holders(session.wallet_id(), &row).await? {
            self.inner.store.delete_address(holder.id).await?;
        }
        session.vault_write(move |vault| vault.remove(entry)).await?;
        tracing::warn!(target: "jelly::engine::address", address_id, "standalone key deleted");
        Ok(())
    }

    pub async fn get_xpub(&self, path: &DerivationPath) -> Result<String, CoreError> {
        let session = self.require_session().await?;
        let path = parse_path(path.as_str())?;
        Ok(session.keychain().xpub_at(&path)?)
    }

    /// The account-level xpub behind a derived address, when it has one.
    pub async fn get_xpub_for_address(&self, address_id: i64) -> Result<Option<String>, CoreError> {
        let session = self.require_session().await?;
        let row = self.require_row(session.wallet_id(), address_id).await?;
        if self.inner.table.spec(row.chain).key_kind != KeyKind::Secp256k1 {
            return Ok(None);
        }
        let KeySource::Derived { ref path } = row.source else {
            return Ok(None);
        };
        let path = parse_path(path.as_str())?;
        let Some(account) = account_prefix(&path) else {
            return Ok(None);
        };
        Ok(Some(session.keychain().xpub_at(&account)?))
    }

    /// Every row of the wallet whose key is the standalone key of `row`, `row` among
    /// them.
    async fn key_holders(
        &self,
        wallet_id: i64,
        row: &AddressRow,
    ) -> Result<Vec<AddressRow>, CoreError> {
        let rows = self.inner.store.list_addresses(wallet_id, Some(row.chain)).await?;
        Ok(rows.into_iter().filter(|other| other.source == row.source).collect())
    }

    /// Adds every other script flavour of `added`'s key that the chain shows in use,
    /// so a key another wallet used under another flavour shows its funds. It asks once,
    /// when the key enters, and only a chain whose adapter probes for flavours is asked.
    /// A failure is logged and never fails the address that was added.
    async fn adopt_used_scripts(&self, session: &Session, added: &AddressRecord) {
        let adopting = timeout(SCRIPT_PROBE_TIMEOUT, self.try_adopt_used_scripts(session, added));
        match adopting.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                target: "jelly::engine::address",
                chain = added.chain.as_str(),
                %error,
                "could not check the other script flavours of a new key"
            ),
            Err(_) => tracing::warn!(
                target: "jelly::engine::address",
                chain = added.chain.as_str(),
                "the check of a new key's other script flavours ran out of time"
            ),
        }
    }

    async fn try_adopt_used_scripts(
        &self,
        session: &Session,
        added: &AddressRecord,
    ) -> Result<(), CoreError> {
        let Ok(adapter) = self.adapter(added.chain) else {
            return Ok(());
        };
        let Some(probe) = adapter.script_probe() else {
            return Ok(());
        };
        let entry = session.entry(added.chain);
        let row = self.require_row(session.wallet_id(), added.id).await?;
        let key = derive::resolve_key(session, &row)?;
        let rows = self.inner.store.list_addresses(session.wallet_id(), Some(added.chain)).await?;
        let mut candidates = Vec::new();
        for &script in entry.spec().script_kinds {
            if row.script_kind == Some(script) {
                continue;
            }
            let address = entry.address(&key, Some(script))?;
            if rows.iter().all(|known| known.address != address) {
                candidates.push((script, Address::assume_canonical(added.chain, address)));
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let addresses: Vec<Address> =
            candidates.iter().map(|(_, address)| address.clone()).collect();
        let used = probe.used(&addresses).await?;
        for (script, address) in candidates {
            if !used.contains(&address) {
                continue;
            }
            let new = NewAddress {
                wallet_id: session.wallet_id(),
                chain: added.chain,
                address: address.into_value(),
                source: row.source.clone(),
                script_kind: Some(script),
                derivation_index: row.derivation_index,
                label: None,
            };
            match self.insert(new).await {
                Ok(_) | Err(CoreError::AlreadyExists { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// An address row that must belong to the open wallet.
    pub(crate) async fn require_row(
        &self,
        wallet_id: i64,
        address_id: i64,
    ) -> Result<AddressRow, CoreError> {
        let row = self.inner.store.get_address(address_id).await?;
        if row.wallet_id == wallet_id {
            Ok(row)
        } else {
            Err(CoreError::not_found("address", address_id.to_string()))
        }
    }

    async fn insert_derived(
        &self,
        session: &Session,
        chain: ChainId,
        path: &Bip32Path,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let entry = session.entry(chain);
        let spec = entry.spec();
        let script_kind = spec.resolve_script(purpose_of(path), script)?;
        let key = session.keychain().key_material_at(spec.key_kind, path)?;
        let address = entry.address(&key, script_kind)?;
        // Solana's family varies a middle component, so the index is whichever one the
        // family varies. A path outside the family falls back to its last component.
        let derivation_index = family_for(spec, script_kind)?
            .index_of(path)
            .or_else(|| path.as_ref().last().copied().map(child_index));
        let new = NewAddress {
            wallet_id: session.wallet_id(),
            chain,
            address,
            source: KeySource::Derived { path: stored_path(path) },
            script_kind,
            derivation_index,
            label,
        };
        self.insert(new).await
    }

    async fn store_imported(
        &self,
        session: &Session,
        chain: ChainId,
        material: KeyMaterial,
        script: Option<ScriptKind>,
        label: Option<String>,
    ) -> Result<AddressRecord, CoreError> {
        let entry = session.entry(chain);
        let script_kind = entry.spec().resolve_script(None, script)?;
        let address = entry.address(&material, script_kind)?;
        // Checked before the vault write, which rewrites the whole file.
        let rows = self.inner.store.list_addresses(session.wallet_id(), Some(chain)).await?;
        if rows.iter().any(|row| row.address == address) {
            return Err(CoreError::AlreadyExists { entity: "address" });
        }
        let blob = material.with_bytes(|bytes| {
            SecretBlob::new(
                BlobContent::PrivateKey(material.kind()),
                Zeroizing::new(bytes.to_vec()),
            )
        });
        let vault_entry = session.vault_write(move |vault| vault.insert(blob)).await?;
        let new = NewAddress {
            wallet_id: session.wallet_id(),
            chain,
            address,
            source: KeySource::Imported { vault_entry: vault_entry.to_string() },
            script_kind,
            derivation_index: None,
            label,
        };
        match self.insert(new).await {
            Ok(added) => Ok(added),
            Err(error) => {
                session.vault_write(move |vault| vault.remove(vault_entry)).await?;
                Err(error)
            }
        }
    }

    async fn insert(&self, new: NewAddress) -> Result<AddressRecord, CoreError> {
        let row = self.inner.store.insert_address(new).await?;
        Ok(AddressRecord::from(row))
    }

    /// Widens the scanner's window so a new subaddress is recognised without a rescan.
    /// Best effort, because the chain may not be built yet.
    async fn register_subaddress(&self, chain: ChainId, major: u32, minor: u32) {
        let Ok(adapter) = self.adapter(chain) else {
            return;
        };
        let Some(subaddresses) = adapter.subaddresses() else {
            return;
        };
        if let Err(error) = subaddresses.register_window(major, minor).await {
            tracing::warn!(
                target: "jelly::engine::address",
                %error,
                "could not widen the subaddress window"
            );
        }
    }
}

/// Reads a key through the chain's entry, naming a pasted seed phrase for what it is.
///
/// The entries read no BIP-39, so the phrase check is here, after the entry, because
/// a TON seed is spelled in BIP-39's words and is also a valid phrase about once in
/// 256 tries.
fn import_key(
    table: &ChainTable,
    chain: ChainId,
    input: &str,
    password: Option<&SecretText>,
) -> Result<ImportedKey, KeyError> {
    let is_phrase = Mnemonic::parse(input).is_ok();
    match table.import_key(chain, input, password) {
        Err(_) if is_phrase => Err(KeyError::SeedPhrase),
        outcome => outcome,
    }
}

/// Chooses the derivation family from the requested script type.
///
/// Encoding one path in another flavour would create addresses other wallets never scan
/// during recovery. The shared script resolver rejects types the chain does not support.
fn family_for(spec: &ChainSpec, script: Option<ScriptKind>) -> Result<PathTemplate, CoreError> {
    let Some(script) = script else {
        return Ok(PathTemplate::parse(spec.default_path)?);
    };
    spec.resolve_script(None, Some(script))?;
    match script.purpose() {
        Some(purpose) => Ok(PathTemplate::bip44(purpose, spec.slip44)),
        None => Ok(PathTemplate::parse(spec.default_path)?),
    }
}

const fn require_standalone_keys(spec: &ChainSpec) -> Result<(), CoreError> {
    if spec.can_hold_standalone_keys() {
        Ok(())
    } else {
        Err(CoreError::Unsupported("this chain holds no standalone keys"))
    }
}

fn require_confirmation(confirmation: &str) -> Result<(), CoreError> {
    if confirmation == DELETE_KEY_CONFIRMATION {
        Ok(())
    } else {
        Err(CoreError::ConfirmationRequired { expected: String::from(DELETE_KEY_CONFIRMATION) })
    }
}

/// The lowest minor index under `major` with no row, so a removed subaddress is the next
/// one handed out again.
fn next_minor(rows: &[AddressRow], major: u32) -> Result<u32, CoreError> {
    let taken: BTreeSet<u32> = rows
        .iter()
        .filter_map(|row| match row.source {
            KeySource::Subaddress { major: found, minor } if found == major => Some(minor),
            KeySource::Subaddress { .. }
            | KeySource::Derived { .. }
            | KeySource::Imported { .. } => None,
        })
        .collect();
    (0..=u32::MAX)
        .find(|minor| !taken.contains(minor))
        .ok_or_else(|| CoreError::internal_without_source("the subaddress range is exhausted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subaddress(minor: u32) -> AddressRow {
        AddressRow {
            id: i64::from(minor),
            wallet_id: 1,
            chain: ChainId::Monero,
            address: format!("addr{minor}"),
            source: KeySource::Subaddress { major: 0, minor },
            script_kind: None,
            derivation_index: Some(minor),
            label: None,
            created_at_unix_secs: 0,
        }
    }

    #[test]
    fn the_first_subaddress_is_the_primary_one() {
        assert_eq!(next_minor(&[], 0).ok(), Some(0));
    }

    #[test]
    fn the_lowest_gap_is_filled_first() {
        let rows = [subaddress(0), subaddress(1), subaddress(3)];
        assert_eq!(next_minor(&rows, 0).ok(), Some(2));
    }

    #[test]
    fn a_subaddress_of_another_account_takes_no_index() {
        let mut row = subaddress(7);
        row.source = KeySource::Subaddress { major: 1, minor: 7 };
        assert_eq!(next_minor(&[row], 0).ok(), Some(0));
    }

    #[test]
    fn the_key_deletion_confirmation_is_exact() {
        require_confirmation(DELETE_KEY_CONFIRMATION).unwrap();
        for wrong in ["delete", "DELETE ", "", "please"] {
            assert!(matches!(
                require_confirmation(wrong),
                Err(CoreError::ConfirmationRequired { .. })
            ));
        }
    }
}
