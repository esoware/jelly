use bitcoin::{OutPoint, Script, ScriptBuf, TxOut, Weight};
use jelly_core::{Amount, FeeRate};

use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::script::{SpendableScript, UtxoScript};
use crate::weight;

/// A rate is base units per thousand weight units, so scaling a value by this
/// instead of dividing the rate keeps [`Utxo::effective_milli`] and the waste
/// metric exact in integers.
const MILLI: i128 = 1000;

/// What created a coin, which decides how deep it has to be before a node lets a
/// transaction spend it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Ordinary,
    Coinbase,
    /// An output after the first of a Litecoin `HogEx`.
    Pegout,
    /// An output of a transaction that spends only the wallet's own coins, such
    /// as change. Only the wallet can replace that transaction, so it's safe to
    /// spend unconfirmed. Bitcoin Core calls this a trusted coin.
    Own,
}

/// One of the wallet's outputs, with its script kind, confirmation height, and
/// origin.
///
/// `verified_unspents` takes the value from the funding transaction, not
/// `listunspent`, because a legacy input's sighash doesn't commit to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Utxo {
    outpoint: OutPoint,
    value: Amount,
    script: SpendableScript,
    height: Option<u32>,
    origin: Origin,
    /// Whether it carries `CashTokens` or SLP tokens. Spending it into plain outputs
    /// would burn them, so it counts toward the balance but is never selected. A
    /// token wallet on the same keys can still move it.
    has_tokens: bool,
}

impl Utxo {
    pub(crate) const fn new(
        outpoint: OutPoint,
        value: Amount,
        script: SpendableScript,
        height: Option<u32>,
        origin: Origin,
    ) -> Self {
        Self { outpoint, value, script, height, origin, has_tokens: false }
    }

    #[must_use]
    pub(crate) const fn holding_tokens(mut self, has_tokens: bool) -> Self {
        self.has_tokens = has_tokens;
        self
    }

    pub(crate) const fn outpoint(&self) -> OutPoint {
        self.outpoint
    }

    pub(crate) const fn value(&self) -> Amount {
        self.value
    }

    pub(crate) fn script_pubkey(&self) -> &Script {
        self.script.script_pubkey()
    }

    pub(crate) const fn kind(&self) -> UtxoScript {
        self.script.kind()
    }

    /// The confirming height, absent while the funding transaction is unconfirmed.
    pub(crate) const fn height(&self) -> Option<u32> {
        self.height
    }

    pub(crate) const fn is_confirmed(&self) -> bool {
        self.height.is_some()
    }

    /// Confirmed, or unconfirmed change the wallet made itself. A stranger can
    /// replace their own unconfirmed payment and take back a coin spent from it.
    pub(crate) const fn is_trusted(&self) -> bool {
        self.is_confirmed() || matches!(self.origin, Origin::Own)
    }

    pub(crate) const fn origin(&self) -> Origin {
        self.origin
    }

    pub(crate) const fn has_tokens(&self) -> bool {
        self.has_tokens
    }

    /// Whether a transaction in the block after `tip_height` may spend it without
    /// a node refusing it or tokens being lost.
    pub(crate) fn is_selectable(&self, params: &UtxoParams, tip_height: u32) -> bool {
        !self.has_tokens && self.is_mature(params, tip_height)
    }

    /// Whether a transaction in the block after `tip_height` may spend it, which
    /// is the height a node checks a mempool spend against.
    fn is_mature(&self, params: &UtxoParams, tip_height: u32) -> bool {
        let maturity = match self.origin {
            Origin::Ordinary | Origin::Own => return true,
            Origin::Coinbase => params.coinbase_maturity,
            Origin::Pegout => params.pegout_maturity,
        };
        self.height
            .is_some_and(|height| tip_height.saturating_add(1).saturating_sub(height) >= maturity)
    }

    pub(crate) const fn input_weight(&self) -> Weight {
        weight::input_weight(self.script.kind())
    }

    /// The previous output an input spending this coin names.
    pub(crate) fn tx_out(&self) -> Result<TxOut, UtxoError> {
        tx_out(self.value, self.script.script_buf().clone())
    }

    /// Value net of what this input costs to spend, in thousandths of a base unit.
    ///
    /// An `i128`, since a scaled `u64` doesn't fit in `i64` and the result goes
    /// negative for a coin that costs more to spend than it's worth.
    pub(crate) fn effective_milli(&self, rate: FeeRate) -> i128 {
        let value_milli = milli(self.value).unwrap_or(i128::MAX);
        let cost_milli =
            i128::from(rate.sat_per_kwu()).saturating_mul(i128::from(self.input_weight().to_wu()));
        value_milli.saturating_sub(cost_milli)
    }

    pub(crate) fn total(coins: &[Self]) -> Result<Amount, UtxoError> {
        Amount::checked_sum(coins.iter().map(Self::value))
            .ok_or_else(|| UtxoError::overflow("an input total"))
    }
}

pub(crate) fn units(value: u64) -> Amount {
    Amount::from(value)
}

/// Only for amounts built from `u64` output values, so a failure is a bug, but it's
/// still reported instead of asserted away.
pub(crate) fn to_u64(amount: Amount) -> Result<u64, UtxoError> {
    u64::try_from(amount.base_units())
        .map_err(|_| UtxoError::overflow("an amount past any output value"))
}

/// `amount` in thousandths of a base unit.
pub(crate) fn milli(amount: Amount) -> Result<i128, UtxoError> {
    i128::try_from(amount.base_units())
        .ok()
        .and_then(|units| units.checked_mul(MILLI))
        .ok_or_else(|| UtxoError::overflow("an amount in thousandths of a base unit"))
}

pub(crate) fn value_of(txout: &TxOut) -> Amount {
    units(txout.value.to_sat())
}

/// The one place a `TxOut` is built, so the conversion out of [`Amount`] is
/// checked once.
pub(crate) fn tx_out(value: Amount, script_pubkey: ScriptBuf) -> Result<TxOut, UtxoError> {
    Ok(TxOut { value: bitcoin::Amount::from_sat(to_u64(value)?), script_pubkey })
}

#[cfg(test)]
mod tests {
    use jelly_core::ScriptKind;

    use super::*;
    use crate::fixtures::MINED_AT;
    use crate::test_util::{BITCOIN, LITECOIN, fixture_txid, test_key};

    /// The first block that may spend a coinbase or peg-out output is the coin's
    /// own height plus the maturity, so the tip has to be one below that.
    #[test]
    fn a_maturing_coin_waits_for_its_chains_depth() {
        let ours = test_key(&LITECOIN, ScriptKind::P2wpkh, 1).unwrap();
        let coin = |vout, origin| {
            Utxo::new(
                OutPoint { txid: fixture_txid(5), vout },
                units(190_000),
                ours.script().clone(),
                Some(MINED_AT),
                origin,
            )
        };
        let coinbase = coin(0, Origin::Coinbase);
        let pegout = coin(1, Origin::Pegout);
        assert!(!coinbase.is_mature(&LITECOIN, MINED_AT + 98));
        assert!(coinbase.is_mature(&LITECOIN, MINED_AT + 99));
        assert!(!pegout.is_mature(&LITECOIN, MINED_AT + 4));
        assert!(pegout.is_mature(&LITECOIN, MINED_AT + 5));
        assert!(pegout.is_mature(&BITCOIN, MINED_AT), "Bitcoin has no peg-out to wait for");
        assert!(coin(2, Origin::Ordinary).is_mature(&LITECOIN, MINED_AT));
    }
}
