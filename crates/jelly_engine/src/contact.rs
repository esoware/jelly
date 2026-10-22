use jelly_core::{ChainId, CoreError};
use jelly_store::{ContactEdit, ContactRow, NewContact};
use serde::Serialize;

use crate::WalletEngine;

/// An address the user saved to send to, kept with the open wallet.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactRecord {
    pub id: i64,
    pub chain: ChainId,
    /// Always the canonical spelling the chain's codec produced.
    pub address: String,
    pub name: String,
    pub note: Option<String>,
}

impl From<ContactRow> for ContactRecord {
    fn from(row: ContactRow) -> Self {
        Self { id: row.id, chain: row.chain, address: row.address, name: row.name, note: row.note }
    }
}

impl WalletEngine {
    pub async fn list_contacts(&self) -> Result<Vec<ContactRecord>, CoreError> {
        let session = self.require_session().await?;
        let rows = self.inner.store.list_contacts(session.wallet_id()).await?;
        Ok(rows.into_iter().map(ContactRecord::from).collect())
    }

    /// Refuses an address the chain's codec does not accept, so a checksum typo never
    /// reaches the address book.
    pub async fn add_contact(
        &self,
        chain: ChainId,
        address: &str,
        name: &str,
        note: Option<&str>,
    ) -> Result<ContactRecord, CoreError> {
        let session = self.require_session().await?;
        let new = NewContact {
            wallet_id: session.wallet_id(),
            chain,
            address: self.inner.table.validate_address(chain, address)?.into_value(),
            name: contact_name(name)?,
            note: contact_note(note),
        };
        let row = self.inner.store.insert_contact(new).await?;
        Ok(ContactRecord::from(row))
    }

    /// The contact keeps its chain, and the new address is checked against it.
    pub async fn update_contact(
        &self,
        contact_id: i64,
        address: &str,
        name: &str,
        note: Option<&str>,
    ) -> Result<ContactRecord, CoreError> {
        let session = self.require_session().await?;
        let wallet_id = session.wallet_id();
        let current = self.inner.store.get_contact(wallet_id, contact_id).await?;
        let edit = ContactEdit {
            address: self.inner.table.validate_address(current.chain, address)?.into_value(),
            name: contact_name(name)?,
            note: contact_note(note),
        };
        let row = self.inner.store.update_contact(wallet_id, contact_id, edit).await?;
        Ok(ContactRecord::from(row))
    }

    pub async fn delete_contact(&self, contact_id: i64) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        Ok(self.inner.store.delete_contact(session.wallet_id(), contact_id).await?)
    }
}

fn contact_name(name: &str) -> Result<String, CoreError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CoreError::invalid_input("name", "Enter a name for this contact."));
    }
    Ok(trimmed.to_owned())
}

fn contact_note(note: Option<&str>) -> Option<String> {
    note.map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}
