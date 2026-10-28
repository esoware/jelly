use bitcoin::{OutPoint, Transaction, Txid, Weight};
use jelly_core::{Amount, FeeRate};

use crate::coin::{Origin, Utxo, value_of};
use crate::error::UtxoError;
use crate::fee;
use crate::prevout::PrevoutMap;
use crate::script::SpendableScript;

/// An unconfirmed transaction a bump speeds up, with the outputs it spends.
///
/// Every input's previous output was found among the funding transactions, so
/// the fee and rate come from those transactions, not from what a server reports.
#[derive(Debug, Clone)]
pub(crate) struct Original {
    txid: Txid,
    tx: Transaction,
    inputs: Vec<Utxo>,
    fee: Amount,
    weight: Weight,
    rate: FeeRate,
}

impl Original {
    /// Resolves `tx` against the transactions that funded it.
    ///
    /// The inputs get no confirmation height, since BIP-125 rule 2 only constrains
    /// inputs a replacement adds, and `Origin::Ordinary`, since a mempool already
    /// accepted `tx` spending them, so maturity is moot.
    pub(crate) fn new(tx: Transaction, funding: &[Transaction]) -> Result<Self, UtxoError> {
        let known = PrevoutMap::index(funding);
        let mut inputs = Vec::with_capacity(tx.input.len());
        for (index, txin) in tx.input.iter().enumerate() {
            let txout =
                known.get(txin.previous_output).ok_or(UtxoError::MissingPrevout { index })?;
            inputs.push(Utxo::new(
                txin.previous_output,
                value_of(txout),
                SpendableScript::new(txout.script_pubkey.clone())?,
                None,
                Origin::Ordinary,
            ));
        }

        let txid = tx.compute_txid();
        let spent = Utxo::total(&inputs)?;
        let paid = Amount::checked_sum(tx.output.iter().map(value_of))
            .ok_or_else(|| UtxoError::overflow("an output total"))?;
        let fee = spent.checked_sub(paid).ok_or(UtxoError::OriginalOverspends { txid })?;
        let weight = tx.weight();
        Ok(Self { txid, tx, inputs, fee, weight, rate: fee::rate_for(fee, weight)? })
    }

    pub(crate) const fn txid(&self) -> Txid {
        self.txid
    }

    pub(crate) const fn tx(&self) -> &Transaction {
        &self.tx
    }

    /// The coins it spends, in its own input order.
    pub(crate) fn inputs(&self) -> &[Utxo] {
        &self.inputs
    }

    /// What it actually paid, its inputs less its outputs.
    pub(crate) const fn fee(&self) -> Amount {
        self.fee
    }

    /// Weight of the signed transaction, which is what the network charged.
    pub(crate) const fn weight(&self) -> Weight {
        self.weight
    }

    /// The rate it actually paid, which a replacement has to beat.
    pub(crate) const fn rate(&self) -> FeeRate {
        self.rate
    }

    /// BIP-125 rule 1 signaling, at least one input with a sequence below
    /// `0xfffffffe`.
    pub(crate) fn is_replaceable(&self) -> bool {
        self.tx.input.iter().any(|txin| txin.sequence.is_rbf())
    }

    pub(crate) fn spends(&self) -> Vec<OutPoint> {
        self.tx.input.iter().map(|txin| txin.previous_output).collect()
    }

    /// The total fee of everything a replacement evicts, this transaction and its
    /// descendants. BIP-125 rule 3 makes the replacement pay at least this.
    pub(crate) fn evicted_fee(&self, descendant_fee: Amount) -> Result<Amount, UtxoError> {
        self.fee
            .checked_add(descendant_fee)
            .ok_or_else(|| UtxoError::overflow("the fees a replacement evicts"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coin::units;
    use crate::fee::rate_for;
    use crate::fixtures::{ONE_PER_VB, bitcoin_scene, sent_original};

    const FUNDED_SAT: u64 = 500_000;
    const PAID_SAT: u64 = 100_000;

    #[test]
    fn an_original_carries_the_fee_and_rate_it_actually_paid() {
        let scene = bitcoin_scene().unwrap();
        let (original, _) = sent_original(&scene, Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        assert_eq!(original.inputs().len(), 1);
        assert_eq!(original.inputs().first().map(Utxo::value), Some(units(FUNDED_SAT)));
        assert!(original.is_replaceable());
        assert_eq!(original.rate(), rate_for(original.fee(), original.weight()).unwrap());
        assert!(original.fee() > Amount::ZERO);
        assert_eq!(original.txid(), original.tx().compute_txid());
    }

    #[test]
    fn an_original_whose_funding_is_missing_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let (original, _) = sent_original(&scene, Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        assert!(matches!(
            Original::new(original.tx().clone(), &[]),
            Err(UtxoError::MissingPrevout { index: 0 })
        ));
    }
}
