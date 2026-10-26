use jelly_core::{ChainId, PassphrasePolicy, WalletColor};
use rusqlite::{Row, params};

use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::patch::Patch;
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

pub(crate) const WALLET: &str = "wallet";
const WALLET_SETTINGS: &str = "wallet settings";
const CHAIN_STATE: &str = "chain state";

statements! {
    INSERT =
        "INSERT INTO wallets (name, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?3) \
         RETURNING id, name, color, created_at";

    GET = "SELECT id, name, color, created_at FROM wallets WHERE id = ?1";

    LIST = "SELECT id, name, color, created_at FROM wallets ORDER BY id ASC";

    UPDATE =
        "UPDATE wallets SET name = ?2, color = ?3, updated_at = ?4 WHERE id = ?1 \
         RETURNING id, name, color, created_at";

    DELETE = "DELETE FROM wallets WHERE id = ?1";

    INSERT_SETTINGS =
        "INSERT INTO wallet_settings (wallet_id, passphrase_policy, account_index) \
         VALUES (?1, ?2, ?3)";

    GET_SETTINGS =
        "SELECT wallet_id, passphrase_policy, account_index FROM wallet_settings \
         WHERE wallet_id = ?1";

    /// From `?4` on, each even parameter says whether the value after it is written,
    /// which tells keeping from clearing.
    UPSERT_CHAIN_STATE =
        "INSERT INTO wallet_chain_state \
           (wallet_id, chain, enabled, poll_secs, restore_height, synced_height) \
         VALUES (?1, ?2, COALESCE(?3, 1), ?5, ?7, ?9) \
         ON CONFLICT (wallet_id, chain) DO UPDATE SET \
           enabled = COALESCE(?3, enabled), \
           poll_secs = CASE ?4 WHEN 1 THEN ?5 ELSE poll_secs END, \
           restore_height = CASE ?6 WHEN 1 THEN ?7 ELSE restore_height END, \
           synced_height = CASE ?8 WHEN 1 THEN ?9 ELSE synced_height END";

    LIST_CHAIN_STATE =
        "SELECT wallet_id, chain, enabled, poll_secs, restore_height, synced_height \
         FROM wallet_chain_state WHERE wallet_id = ?1 ORDER BY chain ASC";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletRow {
    pub id: i64,
    pub name: String,
    pub color: WalletColor,
    pub created_at_unix_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWallet {
    pub name: String,
    pub color: WalletColor,
    pub passphrase_policy: PassphrasePolicy,
    pub account_index: u32,
}

/// Written once by [`crate::Store::create_wallet`] and read at every unlock.
///
/// Both values decide which keys the wallet derives, so neither can change. An edit
/// would orphan every stored address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalletSettingsRow {
    pub wallet_id: i64,
    pub passphrase_policy: PassphrasePolicy,
    pub account_index: u32,
}

/// A scan cursor derived from a view key is secret, so it lives in the vault rather than
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainStateRow {
    pub wallet_id: i64,
    pub chain: ChainId,
    pub enabled: bool,
    pub poll_secs: Option<u32>,
    pub restore_height: Option<u64>,
    pub synced_height: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChainStatePatch {
    pub enabled: Option<bool>,
    pub poll_secs: Patch<u32>,
    pub restore_height: Patch<u64>,
    pub synced_height: Patch<u64>,
}

const COLOR: Column = Column::new(WALLET, "color");
const PASSPHRASE_POLICY: Column = Column::new(WALLET_SETTINGS, "passphrase_policy");
const ACCOUNT_INDEX: Column = Column::new(WALLET_SETTINGS, "account_index");
const STATE_CHAIN: Column = Column::new(CHAIN_STATE, "chain");
const POLL_SECS: Column = Column::new(CHAIN_STATE, "poll_secs");
const RESTORE_HEIGHT: Column = Column::new(CHAIN_STATE, "restore_height");
const SYNCED_HEIGHT: Column = Column::new(CHAIN_STATE, "synced_height");

fn map(row: &Row<'_>) -> Result<WalletRow, StoreError> {
    Ok(WalletRow {
        id: row.get("id")?,
        name: row.get("name")?,
        color: codec::wire(row, COLOR, WalletColor::parse)?,
        created_at_unix_secs: row.get("created_at")?,
    })
}

fn map_settings(row: &Row<'_>) -> Result<WalletSettingsRow, StoreError> {
    Ok(WalletSettingsRow {
        wallet_id: row.get("wallet_id")?,
        passphrase_policy: codec::wire(row, PASSPHRASE_POLICY, PassphrasePolicy::parse)?,
        account_index: codec::int(row, ACCOUNT_INDEX)?,
    })
}

fn map_state(row: &Row<'_>) -> Result<ChainStateRow, StoreError> {
    Ok(ChainStateRow {
        wallet_id: row.get("wallet_id")?,
        chain: codec::wire(row, STATE_CHAIN, ChainId::parse)?,
        enabled: row.get("enabled")?,
        poll_secs: codec::optional_int(row, POLL_SECS)?,
        restore_height: codec::optional_int(row, RESTORE_HEIGHT)?,
        synced_height: codec::optional_int(row, SYNCED_HEIGHT)?,
    })
}

impl Reader<'_> {
    pub(crate) fn get_wallet(&self, wallet_id: i64) -> Result<WalletRow, StoreError> {
        sql::optional(self.connection(), GET, params![wallet_id], map)?
            .ok_or_else(|| StoreError::not_found(WALLET, wallet_id))
    }

    pub(crate) fn list_wallets(&self) -> Result<Vec<WalletRow>, StoreError> {
        sql::all(self.connection(), LIST, [], map)
    }

    pub(crate) fn get_wallet_settings(
        &self,
        wallet_id: i64,
    ) -> Result<WalletSettingsRow, StoreError> {
        sql::optional(self.connection(), GET_SETTINGS, params![wallet_id], map_settings)?
            .ok_or_else(|| StoreError::not_found(WALLET_SETTINGS, wallet_id))
    }

    pub(crate) fn list_chain_states(
        &self,
        wallet_id: i64,
    ) -> Result<Vec<ChainStateRow>, StoreError> {
        sql::all(self.connection(), LIST_CHAIN_STATE, params![wallet_id], map_state)
    }
}

impl Writer<'_> {
    /// Creates the settings row too, so no wallet exists without one.
    pub(crate) fn create_wallet(&self, new: &NewWallet) -> Result<WalletRow, StoreError> {
        let NewWallet { name, color, passphrase_policy, account_index } = new;
        let wallet = sql::required(
            self.connection(),
            INSERT,
            params![name, color.as_str(), self.now_unix_secs()],
            map,
            "an insert returned no wallet",
        )
        .constrained(WALLET, "id")?;
        sql::run(
            self.connection(),
            INSERT_SETTINGS,
            params![wallet.id, passphrase_policy.as_str(), i64::from(*account_index)],
        )
        .constrained(WALLET_SETTINGS, "wallet_id")?;
        Ok(wallet)
    }

    pub(crate) fn update_wallet(
        &self,
        wallet_id: i64,
        name: &str,
        color: &WalletColor,
    ) -> Result<WalletRow, StoreError> {
        sql::optional(
            self.connection(),
            UPDATE,
            params![wallet_id, name, color.as_str(), self.now_unix_secs()],
            map,
        )?
        .ok_or_else(|| StoreError::not_found(WALLET, wallet_id))
    }

    /// The schema cascades the delete to the wallet's settings, chain state, contacts,
    /// addresses, and through them every balance, transaction, and cursor.
    pub(crate) fn delete_wallet(&self, wallet_id: i64) -> Result<(), StoreError> {
        let removed = sql::run(self.connection(), DELETE, params![wallet_id])?;
        if removed == 0 {
            return Err(StoreError::not_found(WALLET, wallet_id));
        }
        Ok(())
    }

    pub(crate) fn set_chain_state(
        &self,
        wallet_id: i64,
        chain: ChainId,
        patch: ChainStatePatch,
    ) -> Result<(), StoreError> {
        let restore_height =
            codec::optional_u64_to_db(patch.restore_height.value(), RESTORE_HEIGHT)?;
        let synced_height = codec::optional_u64_to_db(patch.synced_height.value(), SYNCED_HEIGHT)?;
        sql::run(
            self.connection(),
            UPSERT_CHAIN_STATE,
            params![
                wallet_id,
                chain.as_str(),
                patch.enabled,
                patch.poll_secs.is_touched(),
                patch.poll_secs.value().map(i64::from),
                patch.restore_height.is_touched(),
                restore_height,
                patch.synced_height.is_touched(),
                synced_height,
            ],
        )
        .referring_to(WALLET, wallet_id)?;
        Ok(())
    }
}
