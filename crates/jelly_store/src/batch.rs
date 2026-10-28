use std::collections::HashMap;
use std::collections::hash_map::Entry;

use jelly_core::{BalanceUpdate, ScanAdvance, TxRecord};

use crate::address::AddressOwner;
use crate::error::StoreError;
use crate::store::Writer;
use crate::tx::TxWrite;

/// Sync rows and their scan progress, committed together. Every write is keyed, so
/// rescanning after a lost batch is safe.
///
/// Reservations don't go through a batch, since `Store::reserve` has to persist them
/// before broadcast.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncBatch {
    /// Confirmation counts are ignored, since they're derived from the tip at read time.
    pub txs: Vec<TxRecord>,
    pub balances: Vec<BalanceUpdate>,
    pub scans: Vec<ScanAdvance>,
}

impl SyncBatch {
    pub const fn is_empty(&self) -> bool {
        self.txs.is_empty() && self.balances.is_empty() && self.scans.is_empty()
    }
}

impl Writer<'_> {
    /// Files each record under its address row's wallet and chain, read once per address.
    /// A batch that names an unknown address fails, and returning that error from the scope
    /// rolls back everything the batch wrote.
    ///
    /// Returns what each of `txs` did, in order. A record that repeats an earlier one in
    /// the same batch finds that row.
    pub fn apply_sync_batch(&self, batch: &SyncBatch) -> Result<Vec<TxWrite>, StoreError> {
        let SyncBatch { txs, balances, scans } = batch;
        let mut owners: HashMap<i64, AddressOwner> = HashMap::new();
        let mut writes = Vec::with_capacity(txs.len());
        for record in txs {
            let owner = match owners.entry(record.address_id) {
                Entry::Occupied(known) => *known.get(),
                Entry::Vacant(slot) => *slot.insert(self.address_owner(record.address_id)?),
            };
            writes.push(self.put_tx(record, owner)?);
        }
        for update in balances {
            self.put_balance(update)?;
        }
        for advance in scans {
            self.put_scan(advance)?;
        }
        tracing::debug!(
            target: "jelly::store::sync",
            txs = txs.len(),
            balances = balances.len(),
            scans = scans.len(),
            "sync batch applied"
        );
        Ok(writes)
    }
}
