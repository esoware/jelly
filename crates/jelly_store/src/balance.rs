use std::collections::BTreeMap;

use jelly_core::{Amount, AssetId, BalanceUpdate};
use rusqlite::{Row, params};

use crate::address::ADDRESS;
use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

const BALANCE: &str = "balance";

statements! {
    UPSERT =
        "INSERT INTO balances (address_id, asset, amount, updated_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (address_id, asset) DO UPDATE SET \
           amount = excluded.amount, updated_at = excluded.updated_at";

    /// Driven from the address side, so the unique address index supplies the order and
    /// each address's balances are one seek.
    LIST =
        "SELECT a.id AS address_id, b.asset AS asset, b.amount AS amount \
         FROM addresses a JOIN balances b ON b.address_id = a.id \
         WHERE a.wallet_id = ?1 \
         ORDER BY a.chain ASC, a.address ASC, b.asset ASC";

    TOTALS =
        "SELECT b.asset AS asset, b.amount AS amount \
         FROM addresses a JOIN balances b ON b.address_id = a.id \
         WHERE a.wallet_id = ?1";
}

/// One address's balance in one asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRow {
    pub address_id: i64,
    pub asset: AssetId,
    pub amount: Amount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetTotal {
    pub asset: AssetId,
    pub amount: Amount,
}

const ASSET: Column = Column::new(BALANCE, "asset");
const AMOUNT: Column = Column::new(BALANCE, "amount");

fn map(row: &Row<'_>) -> Result<BalanceRow, StoreError> {
    Ok(BalanceRow {
        address_id: row.get("address_id")?,
        asset: codec::wire(row, ASSET, AssetId::parse_key)?,
        amount: codec::amount(row, AMOUNT)?,
    })
}

impl Reader<'_> {
    pub(crate) fn list_balances(&self, wallet_id: i64) -> Result<Vec<BalanceRow>, StoreError> {
        sql::all(self.connection(), LIST, params![wallet_id], map)
    }

    /// Sums in Rust because `SQLite` has no integer wide enough for these amounts. An
    /// overflow is an error, not a wrapped value.
    pub(crate) fn balance_totals(&self, wallet_id: i64) -> Result<Vec<AssetTotal>, StoreError> {
        let rows = sql::all(self.connection(), TOTALS, params![wallet_id], |row| {
            Ok((codec::wire(row, ASSET, AssetId::parse_key)?, codec::amount(row, AMOUNT)?))
        })?;
        let mut sums: BTreeMap<AssetId, Amount> = BTreeMap::new();
        for (asset, amount) in rows {
            let running = sums.entry(asset).or_insert(Amount::ZERO);
            *running = running
                .checked_add(amount)
                .ok_or(StoreError::Overflow { entity: BALANCE, column: AMOUNT.name() })?;
        }
        Ok(sums.into_iter().map(|(asset, amount)| AssetTotal { asset, amount }).collect())
    }
}

impl Writer<'_> {
    pub(crate) fn put_balance(&self, update: &BalanceUpdate) -> Result<(), StoreError> {
        sql::run(
            self.connection(),
            UPSERT,
            params![
                update.address_id,
                update.asset.key(),
                codec::amount_to_blob(update.balance),
                self.now_unix_secs(),
            ],
        )
        .referring_to(ADDRESS, update.address_id)?;
        Ok(())
    }
}
