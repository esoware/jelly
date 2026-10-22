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
    /// The nonce slot a replacement of `txid` reuses.
    ///
    /// Everything but the price comes from the signed original, so a speed-up
    /// is the same payment at a higher price and a cancel is nothing paid to the
    /// same sender, never a different payment. The price either must clear is
    /// the slot's floor, which resolving the draft reads.
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
    /// Pins the nonce to the slot, which makes a node treat the result as a
    /// replacement, and an automatic half is the market's at the chosen level
    /// raised to the slot's floor, as `MetaMask` and Rainbow price a speed-up.
    ///
    /// Only the price and the gas limit come from `fee`. The level and any pinned
    /// half are the caller's, and the nonce and the calldata never are.
    fn price(&self, fee: &FeeSelection) -> Result<Overrides, EvmError> {
        Ok(Overrides { nonce: Some(self.nonce), ..Overrides::price_of(fee)? })
    }

    /// The original's asset, amount, recipient and calldata at a new price, so a
    /// speed-up cannot become a different call.
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

    /// Nothing of the native coin to the sender itself, with no calldata, in the
    /// original's slot. Its gas is estimated rather than assumed, because on
    /// Arbitrum even an empty transfer pays its L1 cost in gas.
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

/// A cancel takes the slot through the same quote, build, sign and broadcast as
/// a speed-up, so it clears the same floor and is watched the same way. Once it
/// lands at depth, the original is replaced by the rule that settles any send
/// whose nonce another transaction took.
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
