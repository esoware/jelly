use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use bitcoin::Txid;
use jelly_core::{Amount, confirmations};
use jelly_electrum::HistoryEntry;

use crate::adapter::watched::Watched;
use crate::claim::OutpointClaims;
use crate::coin::Utxo;
use crate::error::UtxoError;
use crate::sync::PendingTx;

#[derive(Debug, Clone, Default)]
struct AddressState {
    coins: Vec<Utxo>,
    /// Confirming height per transaction in this address's history, absent while
    /// the transaction is in the mempool.
    heights: HashMap<Txid, Option<u32>>,
    /// Shared with the next pass, which reads it without holding the state's lock.
    pending: Arc<[PendingTx]>,
}

/// What a replacement evicts besides the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Descendants {
    /// Their total fee, which BIP-125 rule 3 makes the replacement cover on top of
    /// the original's.
    pub(crate) fee: Amount,
    pub(crate) count: usize,
}

/// What the last pass saw, read by builds, confirmation counts, and replacements
/// instead of asking a server again.
///
/// Each pass rebuilds it from scratch. An address's coins come from its unspent set,
/// or from its stored cursor when the history hasn't moved, and its depths come from
/// the history the pass read. Nothing carries over, so the next full pass picks up
/// any cursor a cut pass advanced.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChainState {
    tip: Option<u32>,
    addresses: BTreeMap<i64, AddressState>,
}

impl ChainState {
    /// An empty state for a pass that read `tip_height`.
    pub(crate) const fn at_tip(tip_height: u32) -> Self {
        Self { tip: Some(tip_height), addresses: BTreeMap::new() }
    }

    pub(crate) fn record(
        &mut self,
        address_id: i64,
        coins: Vec<Utxo>,
        history: &[HistoryEntry],
        pending: Vec<PendingTx>,
    ) {
        let heights = history.iter().map(|item| (item.txid, item.confirmation.height())).collect();
        self.addresses.insert(address_id, AddressState { coins, heights, pending: pending.into() });
    }

    /// The tip the last pass saw, absent until there has been one.
    pub(crate) const fn tip(&self) -> Option<u32> {
        self.tip
    }

    /// The mempool transactions the last pass decoded, by address.
    pub(crate) fn pending_by_address(&self) -> BTreeMap<i64, Arc<[PendingTx]>> {
        self.addresses
            .iter()
            .map(|(address_id, state)| (*address_id, Arc::clone(&state.pending)))
            .collect()
    }

    /// The unspent coins of `address_id` minus claimed ones, or `None` until a pass
    /// has reported on it.
    pub(crate) fn spendable(&self, address_id: i64, claims: &OutpointClaims) -> Option<Vec<Utxo>> {
        self.addresses.get(&address_id).map(|state| claims.spendable(&state.coins))
    }

    /// Depth of `txid` against the tip the last pass saw. A transaction no pass
    /// has seen is zero, not an error.
    pub(crate) fn confirmations(&self, txid: &Txid) -> u32 {
        let height =
            self.addresses.values().find_map(|state| state.heights.get(txid).copied().flatten());
        confirmations(height.map(u64::from), u64::from(self.tip.unwrap_or(0)))
    }

    /// Whether the last pass saw `txid` in the mempool and in no block.
    pub(crate) fn is_pending(&self, txid: &Txid) -> bool {
        let mut sightings =
            self.addresses.values().filter_map(|state| state.heights.get(txid)).peekable();
        sightings.peek().is_some() && sightings.all(Option::is_none)
    }

    /// The mempool transactions the last pass saw that descend from `txid`, across
    /// every watched address. A replacement of `txid` evicts all of them.
    pub(crate) fn descendants(&self, txid: Txid) -> Result<Descendants, UtxoError> {
        let mut pending: BTreeMap<Txid, &PendingTx> = BTreeMap::new();
        for tx in self.addresses.values().flat_map(|state| state.pending.iter()) {
            pending.entry(tx.txid).or_insert(tx);
        }
        let mut evicted: HashSet<Txid> = HashSet::from([txid]);
        let mut total = Amount::ZERO;
        let mut has_grown = true;
        while has_grown {
            has_grown = false;
            for tx in pending.values() {
                if evicted.contains(&tx.txid)
                    || !tx.spends.iter().any(|outpoint| evicted.contains(&outpoint.txid))
                {
                    continue;
                }
                let fee = tx.record.fee.ok_or(UtxoError::UnpricedDescendant { txid: tx.txid })?;
                total = total
                    .checked_add(fee)
                    .ok_or_else(|| UtxoError::overflow("the fees a replacement evicts"))?;
                evicted.insert(tx.txid);
                has_grown = true;
            }
        }
        Ok(Descendants { fee: total, count: evicted.len().saturating_sub(1) })
    }

    /// Drops every address that is no longer watched.
    ///
    /// Leaves claims alone. The engine resets the watch set before every pass, so
    /// releasing them here would free every claimed coin each poll.
    pub(crate) fn retain(&mut self, watched: &[Watched]) {
        self.addresses.retain(|id, _| watched.iter().any(|entry| entry.address_id == *id));
    }
}

#[cfg(test)]
mod tests {
    use jelly_electrum::Confirmation;

    use super::*;
    use crate::test_util::fixture_txid;

    #[test]
    fn a_confirmed_sighting_is_not_hidden_by_an_earlier_unconfirmed_address() {
        let txid = fixture_txid(1);
        let mut state = ChainState::at_tip(102);
        state.record(
            1,
            Vec::new(),
            &[HistoryEntry {
                txid,
                confirmation: Confirmation::Mempool { has_unconfirmed_parents: false },
                fee: None,
            }],
            Vec::new(),
        );
        state.record(
            2,
            Vec::new(),
            &[HistoryEntry {
                txid,
                confirmation: Confirmation::Confirmed { height: 100 },
                fee: None,
            }],
            Vec::new(),
        );

        assert_eq!(state.confirmations(&txid), 3);
    }
}
