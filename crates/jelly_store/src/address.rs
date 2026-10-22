use jelly_core::{ChainId, DerivationPath, KeySource, ScriptKind};
use rusqlite::{Row, params};

use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};
use crate::wallet::WALLET;

pub(crate) const ADDRESS: &str = "address";
const UNIQUE_COLUMNS: &str = "wallet_id, chain, address";

/// The `key_source` spelling of each [`KeySource`] variant. The schema's constraint
/// repeats them, and the address round-trip tests check both.
const DERIVED: &str = "derived";
const IMPORTED: &str = "imported";
const SUBADDRESS: &str = "subaddress";

/// Every column `map` reads.
macro_rules! columns {
    () => {
        "id, wallet_id, chain, address, key_source, derivation_path, vault_entry, \
         subaddress_major, subaddress_minor, script_kind, derivation_index, label, created_at"
    };
}

statements! {
    INSERT =
        concat!(
            "INSERT INTO addresses \
               (wallet_id, chain, address, key_source, derivation_path, vault_entry, \
                subaddress_major, subaddress_minor, script_kind, derivation_index, label, \
                created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             RETURNING ",
            columns!(),
        );

    GET = concat!("SELECT ", columns!(), " FROM addresses WHERE id = ?1");

    /// The unique index on `(wallet_id, chain, address)` already carries this order, so
    /// the query needs no sort.
    LIST =
        concat!(
            "SELECT ",
            columns!(),
            " FROM addresses \
             WHERE wallet_id = ?1 AND (?2 IS NULL OR chain = ?2) \
             ORDER BY chain ASC, address ASC",
        );

    SET_LABEL = concat!("UPDATE addresses SET label = ?2 WHERE id = ?1 RETURNING ", columns!());

    DELETE = "DELETE FROM addresses WHERE id = ?1";

    /// Writes about an address are filed under the wallet and chain its row names, never
    /// the caller's.
    OWNER = "SELECT wallet_id, chain FROM addresses WHERE id = ?1";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressRow {
    pub id: i64,
    pub wallet_id: i64,
    pub chain: ChainId,
    pub address: String,
    pub source: KeySource,
    pub script_kind: Option<ScriptKind>,
    pub derivation_index: Option<u32>,
    pub label: Option<String>,
    pub created_at_unix_secs: i64,
}

/// `derivation_index` is the caller's own numbering. The engine takes it from the
/// derivation path or the subaddress minor index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAddress {
    pub wallet_id: i64,
    pub chain: ChainId,
    pub address: String,
    pub source: KeySource,
    pub script_kind: Option<ScriptKind>,
    pub derivation_index: Option<u32>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AddressOwner {
    pub(crate) wallet_id: i64,
    pub(crate) chain: ChainId,
}

const CHAIN: Column = Column::new(ADDRESS, "chain");
const SCRIPT_KIND: Column = Column::new(ADDRESS, "script_kind");
const KEY_SOURCE: Column = Column::new(ADDRESS, "key_source");
const DERIVATION_PATH: Column = Column::new(ADDRESS, "derivation_path");
const VAULT_ENTRY: Column = Column::new(ADDRESS, "vault_entry");
const SUBADDRESS_MAJOR: Column = Column::new(ADDRESS, "subaddress_major");
const SUBADDRESS_MINOR: Column = Column::new(ADDRESS, "subaddress_minor");
const DERIVATION_INDEX: Column = Column::new(ADDRESS, "derivation_index");

fn map(row: &Row<'_>) -> Result<AddressRow, StoreError> {
    Ok(AddressRow {
        id: row.get("id")?,
        wallet_id: row.get("wallet_id")?,
        chain: codec::wire(row, CHAIN, ChainId::parse)?,
        address: row.get("address")?,
        source: map_source(row)?,
        script_kind: codec::optional_wire(row, SCRIPT_KIND, ScriptKind::parse)?,
        derivation_index: codec::optional_int(row, DERIVATION_INDEX)?,
        label: row.get("label")?,
        created_at_unix_secs: row.get("created_at")?,
    })
}

fn map_source(row: &Row<'_>) -> Result<KeySource, StoreError> {
    let discriminant: String = row.get(KEY_SOURCE.name())?;
    match discriminant.as_str() {
        DERIVED => {
            let path: Option<String> = row.get(DERIVATION_PATH.name())?;
            Ok(KeySource::Derived {
                path: DerivationPath::new(path.ok_or_else(|| DERIVATION_PATH.decode_error())?),
            })
        }
        IMPORTED => {
            let entry: Option<String> = row.get(VAULT_ENTRY.name())?;
            Ok(KeySource::Imported {
                vault_entry: entry.ok_or_else(|| VAULT_ENTRY.decode_error())?,
            })
        }
        SUBADDRESS => Ok(KeySource::Subaddress {
            major: codec::optional_int(row, SUBADDRESS_MAJOR)?
                .ok_or_else(|| SUBADDRESS_MAJOR.decode_error())?,
            minor: codec::optional_int(row, SUBADDRESS_MINOR)?
                .ok_or_else(|| SUBADDRESS_MINOR.decode_error())?,
        }),
        _ => Err(KEY_SOURCE.decode_error()),
    }
}

impl Reader<'_> {
    pub(crate) fn get_address(&self, address_id: i64) -> Result<AddressRow, StoreError> {
        sql::optional(self.connection(), GET, params![address_id], map)?
            .ok_or_else(|| StoreError::not_found(ADDRESS, address_id))
    }

    pub(crate) fn list_addresses(
        &self,
        wallet_id: i64,
        chain: Option<ChainId>,
    ) -> Result<Vec<AddressRow>, StoreError> {
        sql::all(self.connection(), LIST, params![wallet_id, chain.map(ChainId::as_str)], map)
    }
}

impl Writer<'_> {
    pub(crate) fn insert_address(&self, new: &NewAddress) -> Result<AddressRow, StoreError> {
        let NewAddress { wallet_id, chain, address, source, script_kind, derivation_index, label } =
            new;
        // Each variant leaves the detail columns it does not own null, as the schema
        // requires.
        let (discriminant, path, entry, major, minor) = match source {
            KeySource::Derived { path } => (DERIVED, Some(path.as_str()), None, None, None),
            KeySource::Imported { vault_entry } => {
                (IMPORTED, None, Some(vault_entry.as_str()), None, None)
            }
            KeySource::Subaddress { major, minor } => {
                (SUBADDRESS, None, None, Some(i64::from(*major)), Some(i64::from(*minor)))
            }
        };
        sql::required(
            self.connection(),
            INSERT,
            params![
                wallet_id,
                chain.as_str(),
                address,
                discriminant,
                path,
                entry,
                major,
                minor,
                script_kind.map(ScriptKind::as_str),
                derivation_index.map(i64::from),
                label,
                self.now_unix_secs(),
            ],
            map,
            "an insert returned no address",
        )
        .referring_to(WALLET, *wallet_id)
        .constrained(ADDRESS, UNIQUE_COLUMNS)
    }

    pub(crate) fn address_owner(&self, address_id: i64) -> Result<AddressOwner, StoreError> {
        sql::optional(self.connection(), OWNER, params![address_id], |row| {
            Ok(AddressOwner {
                wallet_id: row.get("wallet_id")?,
                chain: codec::wire(row, CHAIN, ChainId::parse)?,
            })
        })?
        .ok_or_else(|| StoreError::not_found(ADDRESS, address_id))
    }

    pub(crate) fn set_address_label(
        &self,
        address_id: i64,
        label: Option<&str>,
    ) -> Result<AddressRow, StoreError> {
        sql::optional(self.connection(), SET_LABEL, params![address_id, label], map)?
            .ok_or_else(|| StoreError::not_found(ADDRESS, address_id))
    }

    /// The schema cascades the delete to the address's balances, transactions, and
    /// cursors. An imported address's vault entry is the caller's to remove.
    pub(crate) fn delete_address(&self, address_id: i64) -> Result<(), StoreError> {
        let removed = sql::run(self.connection(), DELETE, params![address_id])?;
        if removed == 0 {
            return Err(StoreError::not_found(ADDRESS, address_id));
        }
        Ok(())
    }
}
