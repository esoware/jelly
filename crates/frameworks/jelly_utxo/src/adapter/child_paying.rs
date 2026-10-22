use bitcoin::OutPoint;
use jelly_core::FeeRate;

use crate::adapter::fee::FeeContext;
use crate::adapter::watched::{Watched, sources_led_by, sources_of, spenders};
use crate::build::BuiltTx;
use crate::coin::Utxo;
use crate::cpfp::{ChildRequest, Package, Unconfirmed, build_child};
use crate::error::UtxoError;
use crate::original::Original;

/// A send to speed up with a child, recovered from the chain so a restart loses
/// nothing.
///
/// The change is the last output paying one of the wallet's addresses other than the
/// payment, which is the first output paying none of them. A send that paid only the
/// wallet's own addresses, as an earlier child did, has no payment, so its last output
/// of the wallet's is the change, and a child can be sped up in turn.
#[derive(Debug, Clone)]
pub(crate) struct ChildPaying {
    package: Package,
    /// The address the change pays first, which the child pays back, then every
    /// address the send spends from. Coins the child adds come only from these.
    sources: Vec<Watched>,
    change: OutPoint,
}

impl ChildPaying {
    pub(crate) fn resolve(
        watched: &[Watched],
        original: &Original,
        ancestors: Vec<Unconfirmed>,
    ) -> Result<Self, UtxoError> {
        let txid = original.txid();
        let spenders = spenders(watched, original)?;
        let owner_of = |script: &bitcoin::Script| {
            watched.iter().find(|entry| entry.script.script_pubkey() == script)
        };
        let outputs = &original.tx().output;
        let payment = outputs.iter().position(|txout| owner_of(&txout.script_pubkey).is_none());
        let (index, owner) = outputs
            .iter()
            .enumerate()
            .rev()
            .filter(|&(index, _)| Some(index) != payment)
            .find_map(|(index, txout)| owner_of(&txout.script_pubkey).map(|owner| (index, owner)))
            .ok_or(UtxoError::NoChange { txid })?;
        let vout = u32::try_from(index).map_err(|_| UtxoError::overflow("an output index"))?;
        Ok(Self {
            package: Package::new(Unconfirmed::from(original), ancestors),
            sources: sources_led_by(Some(owner), spenders),
            change: OutPoint { txid, vout },
        })
    }

    pub(crate) fn sources(&self) -> &[Watched] {
        &self.sources
    }

    /// The cheapest rate that still raises what the package pays.
    pub(crate) fn min_rate(&self) -> Result<FeeRate, UtxoError> {
        let current = self.package.rate()?;
        Ok(FeeRate::from_sat_per_kwu(current.sat_per_kwu().saturating_add(1)))
    }

    /// `coins` are the spendable coins of the sources, the change among them unless
    /// something already spends it.
    pub(crate) fn build(
        &self,
        fee_context: &FeeContext,
        coins: &[Utxo],
        tip_height: u32,
        rate: FeeRate,
    ) -> Result<BuiltTx, UtxoError> {
        let change = coins
            .iter()
            .find(|coin| coin.outpoint() == self.change)
            .ok_or(UtxoError::ChangeSpent { txid: self.package.parent() })?;
        let request = ChildRequest {
            rate,
            sources: sources_of(&self.sources),
            change,
            extras: coins,
            tip_height,
            relay_floor: fee_context.relay_floor,
        };
        build_child(&fee_context.params, &self.package, &request)
    }
}
