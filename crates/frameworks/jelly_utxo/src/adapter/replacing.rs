use bitcoin::ScriptBuf;
use jelly_core::{FeeRate, TxDirection};

use crate::adapter::fee::FeeContext;
use crate::adapter::state::Descendants;
use crate::adapter::watched::{Watched, sources_led_by, sources_of, spenders};
use crate::build::BuiltTx;
use crate::cursor::CursorPending;
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::replacement::{ReplacementRequest, build_replacement, min_replacement_rate};
use crate::utxo::Utxo;

/// The sources and recipient of the original, recovered from the chain so they
/// survive a restart.
///
/// The recipient is the first output paying none of the addresses the original
/// spends from, or the first output if they all do. The change is the last other
/// output that pays the wallet. Getting the change wrong can only shift which output
/// pays the higher fee, never redirect funds.
#[derive(Debug, Clone)]
pub(super) struct Replacing {
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
    pub(super) fn resolve(
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
    pub(super) fn min_rate(&self, params: &UtxoParams) -> Result<FeeRate, UtxoError> {
        min_replacement_rate(params, &self.original, self.descendants.fee)
    }

    pub(super) fn sources(&self) -> &[Watched] {
        &self.sources
    }

    /// The original as a pass leaves a send pending, in the direction the engine
    /// recorded it, which is a self-transfer only when it paid one of its own sources.
    pub(super) fn pending(&self) -> CursorPending {
        let direction =
            if self.sources.iter().any(|source| source.address == self.recipient_address) {
                TxDirection::SelfTransfer
            } else {
                TxDirection::Outgoing
            };
        CursorPending { txid: self.original.txid(), direction, spends: self.original.spends() }
    }

    /// How many later transactions the replacement cancels.
    pub(super) const fn evicted(&self) -> usize {
        self.descendants.count
    }

    /// `extras` are coins the planner may add when the change can't cover the higher
    /// fee. It drops unconfirmed ones, since BIP-125 rule 2 forbids adding them.
    pub(super) fn build(
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
