use bitcoin::ScriptBuf;
use jelly_core::FeeRate;

use crate::adapter::fee::FeeContext;
use crate::adapter::state::Descendants;
use crate::adapter::watched::{Watched, sources_led_by, sources_of, spenders};
use crate::build::BuiltTx;
use crate::coin::Utxo;
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::replacement::{ReplacementRequest, build_replacement, min_replacement_rate};

/// The sources and recipient of the original, recovered from the chain so a
/// restart loses nothing.
///
/// The funding scripts identify the addresses it spends from. The recipient is the
/// first output paying none of them, or the first output when every output does, and
/// the change is the last other output paying an address of the wallet's.
/// `plan_replacement` keeps every output except the fee source, so misidentifying
/// change can shift which output pays the higher fee but cannot redirect funds.
#[derive(Debug, Clone)]
pub(crate) struct Replacing {
    original: Original,
    /// The change address first when the original has change, then every address
    /// it spends from in input order. Coins a replacement adds come only from these.
    sources: Vec<Watched>,
    /// The script the payment output pays, exactly as the original carries it.
    recipient: ScriptBuf,
    /// The address of `recipient`, which the payload names.
    recipient_address: String,
    /// The mempool transactions descending from the original, which the
    /// replacement evicts with it.
    descendants: Descendants,
}

impl Replacing {
    pub(crate) fn resolve(
        params: &UtxoParams,
        watched: &[Watched],
        original: Original,
        descendants: Descendants,
    ) -> Result<Self, UtxoError> {
        let txid = original.txid();
        let pays = |entry: &Watched, script: &ScriptBuf| entry.script.script_buf() == script;
        let spenders = spenders(watched, &original)?;
        let outputs = &original.tx().output;
        let recipient_index = outputs
            .iter()
            .position(|txout| !spenders.iter().any(|entry| pays(entry, &txout.script_pubkey)))
            .unwrap_or(0);
        let recipient =
            outputs.get(recipient_index).ok_or(UtxoError::RecipientNotFound)?.script_pubkey.clone();
        let change = outputs
            .iter()
            .enumerate()
            .rev()
            .filter(|&(index, _)| index != recipient_index)
            .find_map(|(_, txout)| watched.iter().find(|entry| pays(entry, &txout.script_pubkey)));
        let sources = sources_led_by(change, spenders);
        let recipient_address =
            params.address_of(&recipient).ok_or(UtxoError::UnnameableRecipient { txid })?;
        Ok(Self { original, sources, recipient, recipient_address, descendants })
    }

    /// The cheapest rate the network will take for the replacement.
    pub(crate) fn min_rate(&self, params: &UtxoParams) -> Result<FeeRate, UtxoError> {
        min_replacement_rate(params, &self.original, self.descendants.fee)
    }

    pub(crate) fn sources(&self) -> &[Watched] {
        &self.sources
    }

    /// The transaction being replaced, already resolved against its funding.
    pub(crate) const fn original(&self) -> &Original {
        &self.original
    }

    /// How many later transactions the replacement cancels.
    pub(crate) const fn evicted(&self) -> usize {
        self.descendants.count
    }

    /// `extras` are coins the planner may add when the change cannot cover the
    /// higher fee. The planner drops unconfirmed ones, which BIP-125 rule 2
    /// forbids adding, so a chain whose default spends unconfirmed coins cannot
    /// produce a replacement no node relays.
    pub(crate) fn build(
        &self,
        fee_context: &FeeContext,
        extras: &[Utxo],
        tip_height: u32,
        rate: FeeRate,
    ) -> Result<BuiltTx, UtxoError> {
        let request = ReplacementRequest {
            fee_rate: rate,
            sources: sources_of(&self.sources),
            to: &self.recipient_address,
            to_script: &self.recipient,
            extras,
            tip_height,
            descendant_fee: self.descendants.fee,
        };
        build_replacement(&fee_context.params, &self.original, &request)
    }
}
