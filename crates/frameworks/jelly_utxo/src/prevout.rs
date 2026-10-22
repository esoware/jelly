use std::collections::{BTreeSet, HashMap};

use bitcoin::{OutPoint, Transaction, TxOut, Txid};

/// Transaction outputs indexed by outpoint, for looking up what other
/// transactions spend.
///
/// A transaction does not carry its inputs' values or scripts, so without this a
/// fee is unknowable and an incoming payment has no sender.
#[derive(Debug, Default, Clone)]
pub(crate) struct PrevoutMap {
    known: HashMap<OutPoint, TxOut>,
}

impl PrevoutMap {
    /// Indexes every output of `txs`, so a transaction that spends a sibling in
    /// the same batch needs no extra fetch.
    pub(crate) fn index<'a>(txs: impl IntoIterator<Item = &'a Transaction>) -> Self {
        let mut index = Self::default();
        for tx in txs {
            index.add(tx);
        }
        index
    }

    pub(crate) fn add(&mut self, tx: &Transaction) {
        let txid = tx.compute_txid();
        for (index, txout) in tx.output.iter().enumerate() {
            let vout = u32::try_from(index).unwrap_or(u32::MAX);
            self.known.insert(OutPoint { txid, vout }, txout.clone());
        }
    }

    pub(crate) fn get(&self, outpoint: OutPoint) -> Option<&TxOut> {
        self.known.get(&outpoint)
    }

    /// The transactions still needed to price and attribute `txs`, each once and in
    /// txid order. A coinbase has no real inputs, so it needs none.
    pub(crate) fn missing_parents<'a>(
        &self,
        txs: impl IntoIterator<Item = &'a Transaction>,
    ) -> BTreeSet<Txid> {
        txs.into_iter()
            .filter(|tx| !tx.is_coinbase())
            .flat_map(|tx| tx.input.iter().map(|txin| txin.previous_output))
            .filter(|outpoint| !self.known.contains_key(outpoint))
            .map(|outpoint| outpoint.txid)
            .collect()
    }
}
