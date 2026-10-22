use jelly_core::ChainId;
use rusqlite::{Row, params};

use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};
use crate::wallet::WALLET;

const CONTACT: &str = "contact";
const UNIQUE_COLUMNS: &str = "wallet_id, chain, address";

/// Every column `map` reads.
macro_rules! columns {
    () => {
        "id, chain, address, name, note"
    };
}

// Every statement after the insert is scoped by `wallet_id`, so one wallet can never read
// or change another's contacts.
statements! {
    INSERT =
        concat!(
            "INSERT INTO contacts (wallet_id, chain, address, name, note, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             RETURNING ",
            columns!(),
        );

    GET = concat!("SELECT ", columns!(), " FROM contacts WHERE wallet_id = ?1 AND id = ?2");

    /// The unique index on `(wallet_id, chain, address)` already carries this order, so
    /// the query needs no sort.
    LIST =
        concat!(
            "SELECT ",
            columns!(),
            " FROM contacts WHERE wallet_id = ?1 ORDER BY chain ASC, address ASC",
        );

    UPDATE =
        concat!(
            "UPDATE contacts SET address = ?3, name = ?4, note = ?5 \
             WHERE wallet_id = ?1 AND id = ?2 \
             RETURNING ",
            columns!(),
        );

    DELETE = "DELETE FROM contacts WHERE wallet_id = ?1 AND id = ?2";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactRow {
    pub id: i64,
    pub chain: ChainId,
    pub address: String,
    pub name: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewContact {
    pub wallet_id: i64,
    pub chain: ChainId,
    pub address: String,
    pub name: String,
    pub note: Option<String>,
}

/// The chain is not editable, because the address was checked against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactEdit {
    pub address: String,
    pub name: String,
    pub note: Option<String>,
}

const CHAIN: Column = Column::new(CONTACT, "chain");

fn map(row: &Row<'_>) -> Result<ContactRow, StoreError> {
    Ok(ContactRow {
        id: row.get("id")?,
        chain: codec::wire(row, CHAIN, ChainId::parse)?,
        address: row.get("address")?,
        name: row.get("name")?,
        note: row.get("note")?,
    })
}

impl Reader<'_> {
    pub(crate) fn get_contact(
        &self,
        wallet_id: i64,
        contact_id: i64,
    ) -> Result<ContactRow, StoreError> {
        sql::optional(self.connection(), GET, params![wallet_id, contact_id], map)?
            .ok_or_else(|| StoreError::not_found(CONTACT, contact_id))
    }

    pub(crate) fn list_contacts(&self, wallet_id: i64) -> Result<Vec<ContactRow>, StoreError> {
        sql::all(self.connection(), LIST, params![wallet_id], map)
    }
}

impl Writer<'_> {
    pub(crate) fn insert_contact(&self, new: &NewContact) -> Result<ContactRow, StoreError> {
        let NewContact { wallet_id, chain, address, name, note } = new;
        sql::required(
            self.connection(),
            INSERT,
            params![wallet_id, chain.as_str(), address, name, note, self.now_unix_secs()],
            map,
            "an insert returned no contact",
        )
        .referring_to(WALLET, *wallet_id)
        .constrained(CONTACT, UNIQUE_COLUMNS)
    }

    pub(crate) fn update_contact(
        &self,
        wallet_id: i64,
        contact_id: i64,
        edit: &ContactEdit,
    ) -> Result<ContactRow, StoreError> {
        let ContactEdit { address, name, note } = edit;
        sql::optional(
            self.connection(),
            UPDATE,
            params![wallet_id, contact_id, address, name, note],
            map,
        )
        .constrained(CONTACT, UNIQUE_COLUMNS)?
        .ok_or_else(|| StoreError::not_found(CONTACT, contact_id))
    }

    pub(crate) fn delete_contact(&self, wallet_id: i64, contact_id: i64) -> Result<(), StoreError> {
        let removed = sql::run(self.connection(), DELETE, params![wallet_id, contact_id])?;
        if removed == 0 {
            return Err(StoreError::not_found(CONTACT, contact_id));
        }
        Ok(())
    }
}
