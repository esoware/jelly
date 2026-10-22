use std::collections::BTreeSet;

use async_trait::async_trait;
use bitcoin::{OutPoint, Txid};
use jelly_core::{ChainError, FeeBump, FeeQuote, FeeRate, FeeSelection, TxId, UnsignedTx};

use crate::adapter::UtxoAdapter;
use crate::adapter::child_paying::ChildPaying;
use crate::adapter::fee::{self, FeeContext};
use crate::adapter::replacing::Replacing;
use crate::adapter::send;
use crate::build::BuiltTx;
use crate::coin::Utxo;
use crate::cpfp::Unconfirmed;
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::BumpMethod;

/// Bitcoin Core's and Litecoin Core's `DEFAULT_ANCESTOR_LIMIT` less one, the most
/// unconfirmed transactions a child may wait on and still be relayed.
const MAX_PACKAGE: usize = 24;

enum Bumping {
    Replace { replacing: Replacing, extras: Vec<Utxo> },
    Child { child: ChildPaying, coins: Vec<Utxo> },
}

/// A bump resolved against the chain, which builds at any rate with no I/O, so a
/// quote prices its selection and every level against the same coins.
struct ResolvedBump {
    bumping: Bumping,
    fee_context: FeeContext,
    tip_height: u32,
    /// The cheapest rate the network takes for the bump.
    floor: FeeRate,
}

impl ResolvedBump {
    fn build(&self, rate: FeeRate) -> Result<BuiltTx, UtxoError> {
        match self.bumping {
            Bumping::Replace { ref replacing, ref extras } => {
                replacing.build(&self.fee_context, extras, self.tip_height, rate)
            }
            Bumping::Child { ref child, ref coins } => {
                child.build(&self.fee_context, coins, self.tip_height, rate)
            }
        }
    }

    const fn evicted(&self) -> usize {
        match self.bumping {
            Bumping::Replace { ref replacing, .. } => replacing.evicted(),
            Bumping::Child { .. } => 0,
        }
    }

    /// The outpoints the original spends, when the bump replaces it.
    fn replaced_spends(&self) -> Option<Vec<OutPoint>> {
        match self.bumping {
            Bumping::Replace { ref replacing, .. } => Some(replacing.original().spends()),
            Bumping::Child { .. } => None,
        }
    }
}

impl UtxoAdapter {
    /// The transaction `txid` names, resolved against its funding, for a bump to speed
    /// up.
    ///
    /// One the last pass saw in a block is refused. The network would refuse its bump
    /// anyway, but only after the user approved and signed one. A transaction no pass
    /// has seen reads as zero confirmations, so no bump that could have worked is
    /// refused.
    async fn pending_original(&self, txid: &TxId) -> Result<Original, UtxoError> {
        self.ensure_readable()?;
        let parsed = txid.value().parse::<Txid>().map_err(|_| UtxoError::NotATxid)?;
        if self.state.read().confirmations(&parsed) > 0 {
            return Err(UtxoError::AlreadyConfirmed { txid: parsed });
        }
        self.client.original(parsed).await
    }

    /// Quotes and builds resolve the same bump, as the table's bump method makes it.
    /// Only a build claims coins.
    async fn resolve_bump(&self, txid: &TxId) -> Result<ResolvedBump, UtxoError> {
        let method =
            self.params.bump_method.ok_or(UtxoError::NoBumpMethod { chain: self.params.chain })?;
        let original = self.pending_original(txid).await?;
        match method {
            BumpMethod::Replace => {
                let descendants = self.state.read().descendants(original.txid())?;
                let replacing =
                    Replacing::resolve(&self.params, &self.watch_set(), original, descendants)?;
                let fee_context = self.fee_context().await;
                let floor = replacing.min_rate(&fee_context.params)?;
                let (extras, known_tip) = self.snapshot(replacing.sources()).await?;
                let tip_height = send::tip_height(&self.client, known_tip).await?;
                Ok(ResolvedBump {
                    bumping: Bumping::Replace { replacing, extras },
                    fee_context,
                    tip_height,
                    floor,
                })
            }
            BumpMethod::ChildPaysForParent => {
                let ancestors = self.unconfirmed_ancestors(&original).await?;
                let child = ChildPaying::resolve(&self.watch_set(), &original, ancestors)?;
                let fee_context = self.fee_context().await;
                let floor = child.min_rate()?;
                let (coins, known_tip) = self.snapshot(child.sources()).await?;
                let tip_height = send::tip_height(&self.client, known_tip).await?;
                Ok(ResolvedBump {
                    bumping: Bumping::Child { child, coins },
                    fee_context,
                    tip_height,
                    floor,
                })
            }
        }
    }

    async fn quote_bump_at(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, UtxoError> {
        let resolved = self.resolve_bump(txid).await?;
        let rate = fee::bump_rate_of(fee, &resolved.fee_context, resolved.floor)?;
        let built = resolved.build(rate)?;
        let levels = fee::level_fees(&resolved.fee_context, |level_rate| {
            Ok(resolved.build(level_rate.max(resolved.floor))?.selection.fee)
        });
        Ok(FeeQuote {
            fee: built.selection.fee,
            custom: Some(fee::custom(rate)),
            max_amount: None,
            levels,
        })
    }

    async fn prepare_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, UtxoError> {
        let resolved = self.resolve_bump(txid).await?;
        let rate = fee::bump_rate_of(fee, &resolved.fee_context, resolved.floor)?;
        let built = resolved.build(rate)?;
        let payload = &built.payload;
        match resolved.replaced_spends() {
            // A replacement deliberately spends what it replaces, so the claim the
            // original build holds moves to it rather than conflicting with it.
            Some(spends) => {
                self.claims.take_over(payload.unsigned_txid(), &payload.outpoints(), &spends);
            }
            None => self.claims.claim(payload.unsigned_txid(), &payload.outpoints())?,
        }
        send::unsigned(&built, &resolved.fee_context, resolved.evicted())
    }

    /// Every transaction `original` waits on that the last pass saw still in the
    /// mempool, each priced from its own funding.
    ///
    /// The funding of every input is in a watched history, because it paid the
    /// wallet, so the wallet knows whether it is still unconfirmed, and so on up.
    async fn unconfirmed_ancestors(
        &self,
        original: &Original,
    ) -> Result<Vec<Unconfirmed>, UtxoError> {
        let mut found = Vec::new();
        let mut seen = BTreeSet::from([original.txid()]);
        let mut waiting: Vec<Txid> = original.spends().iter().map(|spent| spent.txid).collect();
        while let Some(txid) = waiting.pop() {
            if !seen.insert(txid) || !self.state.read().is_pending(&txid) {
                continue;
            }
            if found.len() >= MAX_PACKAGE.saturating_sub(1) {
                return Err(UtxoError::AncestorLimit { txid: original.txid() });
            }
            let ancestor = self.client.original(txid).await?;
            waiting.extend(ancestor.spends().iter().map(|spent| spent.txid));
            found.push(Unconfirmed::from(&ancestor));
        }
        Ok(found)
    }
}

/// A bump on this table is a replacement or a child, as its bump method says, and
/// the adapter offers none on a table without one.
#[async_trait]
impl FeeBump for UtxoAdapter {
    async fn quote_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        self.quote_bump_at(txid, fee)
            .await
            .map_err(|error| error.into_chain_error(self.params.chain))
    }

    /// Uses the caller's fee selection, where a level pays its rate, or the network's
    /// floor for the bump when that is higher, and the original transaction's sources,
    /// recipient, and change, including after restart.
    ///
    /// A replacement preserves outputs except its designated fee source, and a child
    /// pays only back to the wallet, so neither can redirect the payment.
    async fn build_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        self.prepare_bump(txid, fee)
            .await
            .map_err(|error| error.into_chain_error(self.params.chain))
    }
}
