use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainError, FeeBump, FeeQuote, FeeSelection, SendAmount, TxCancel, TxId,
    UnsignedTx,
};

use super::draft::Draft;
use super::{EvmAdapter, txid_of};
use crate::error::EvmError;
use crate::fee::Overrides;
use crate::watched;

#[derive(Debug)]
struct Replacing {
    from_address_id: i64,
    from: String,
    to: String,
    asset: AssetId,
    amount: Amount,
    nonce: u64,
    calldata: Option<Vec<u8>>,
}

impl EvmAdapter {
    /// The sent original whose nonce slot a replacement of `txid` reuses.
    ///
    /// Everything but the price comes from the original, so a speed-up is the
    /// same payment at a higher price and a cancel pays nothing to the sender
    /// itself. `resolve` checks either price against the slot's floor.
    fn replacing(&self, txid: &TxId) -> Result<Replacing, EvmError> {
        let id = txid_of(txid)?;
        let not_ours = || EvmError::NotOurs { txid: txid.value().to_owned() };
        let entry = self
            .ledger
            .lock()
            .entry(id)
            .filter(|held| held.is_sent())
            .cloned()
            .ok_or_else(not_ours)?;
        if entry.landing.is_some() {
            return Err(EvmError::AlreadyConfirmed { txid: txid.value().to_owned() });
        }
        let from_address_id =
            watched::address_of(&self.watch_set(), entry.from).ok_or_else(not_ours)?;
        Ok(Replacing {
            from_address_id,
            from: entry.from.to_string(),
            to: entry.to.to_string(),
            asset: entry.asset,
            amount: entry.amount,
            nonce: entry.nonce,
            calldata: entry.calldata.as_ref().map(|calldata| calldata.to_vec()),
        })
    }
}

impl Replacing {
    /// Pins the nonce to the original's slot, which makes a node treat the result
    /// as a replacement. The level, any pinned half and the gas limit come from
    /// `fee`. An automatic half is the market's price at that level raised to the
    /// slot's floor, which is how `MetaMask` and Rainbow price a speed-up.
    fn price(&self, fee: &FeeSelection) -> Result<Overrides, EvmError> {
        Ok(Overrides { nonce: Some(self.nonce), ..Overrides::price_of(fee)? })
    }

    /// The original's asset, amount, recipient and calldata at a new price, so a
    /// speed-up can't become a different call.
    fn speed_up<'a>(&'a self, fee: &FeeSelection) -> Result<Draft<'a>, EvmError> {
        Ok(Draft {
            from_address_id: self.from_address_id,
            to: &self.to,
            asset: self.asset,
            amount: SendAmount::Exact { value: self.amount },
            overrides: Overrides { calldata: self.calldata.clone(), ..self.price(fee)? },
            is_cancel: false,
        })
    }

    /// Zero native coin to the sender itself, with no calldata, in the original's
    /// slot. Gas is estimated, not assumed to be 21000, because on Arbitrum even
    /// an empty transfer pays its L1 cost in gas.
    fn cancel<'a>(&'a self, native: AssetId, fee: &FeeSelection) -> Result<Draft<'a>, EvmError> {
        Ok(Draft {
            from_address_id: self.from_address_id,
            to: &self.from,
            asset: native,
            amount: SendAmount::Exact { value: Amount::ZERO },
            overrides: self.price(fee)?,
            is_cancel: true,
        })
    }
}

/// A replacement is a second transaction at the same nonce and a higher price.
/// Only one transaction per nonce can ever confirm.
#[async_trait]
impl FeeBump for EvmAdapter {
    async fn quote_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        let replacing = self.replacing(txid)?;
        Ok(self.resolve(&replacing.speed_up(fee)?).await?.quote()?)
    }

    async fn build_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        let replacing = self.replacing(txid)?;
        Ok(self.prepare(&replacing.speed_up(fee)?).await?)
    }
}

/// A cancel goes through the same quote, build, sign and broadcast as a speed-up,
/// so it clears the same floor and is watched the same way. Once it's deep
/// enough, the original settles as replaced, like any send whose nonce another
/// transaction took.
#[async_trait]
impl TxCancel for EvmAdapter {
    async fn quote_cancel(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        let replacing = self.replacing(txid)?;
        Ok(self.resolve(&replacing.cancel(self.params.native(), fee)?).await?.quote()?)
    }

    async fn build_cancel(
        &self,
        txid: &TxId,
        fee: &FeeSelection,
    ) -> Result<UnsignedTx, ChainError> {
        let replacing = self.replacing(txid)?;
        Ok(self.prepare(&replacing.cancel(self.params.native(), fee)?).await?)
    }
}
