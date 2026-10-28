use jelly_core::{Amount, AssetId, CoreError};
use jelly_store::BalanceRow;
use serde::Serialize;

use crate::WalletEngine;

/// One address's balance in one asset. The interface reads the address itself from its
/// own record.
///
/// There's no write time, because each pass restates the balance, so its age is the
/// chain's sync state.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceRecord {
    pub address_id: i64,
    pub asset: AssetId,
    pub amount: Amount,
}

impl From<BalanceRow> for BalanceRecord {
    fn from(row: BalanceRow) -> Self {
        Self { address_id: row.address_id, asset: row.asset, amount: row.amount }
    }
}

/// One asset's total across the wallet. The interface reads its scale from the asset's
/// metadata, as it does for a balance.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetTotal {
    pub asset: AssetId,
    pub amount: Amount,
}

impl From<jelly_store::AssetTotal> for AssetTotal {
    fn from(total: jelly_store::AssetTotal) -> Self {
        Self { asset: total.asset, amount: total.amount }
    }
}

impl WalletEngine {
    pub async fn list_balances(&self) -> Result<Vec<BalanceRecord>, CoreError> {
        let session = self.require_session().await?;
        let rows = self.inner.store.list_balances(session.wallet_id()).await?;
        Ok(rows.into_iter().map(BalanceRecord::from).collect())
    }

    pub async fn list_balance_totals(&self) -> Result<Vec<AssetTotal>, CoreError> {
        let session = self.require_session().await?;
        let totals = self.inner.store.balance_totals(session.wallet_id()).await?;
        Ok(totals.into_iter().map(AssetTotal::from).collect())
    }
}
