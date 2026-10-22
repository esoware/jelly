use jelly_core::WatchedAddress;

use crate::build::Source;
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::script::SpendableScript;

/// One watched address, resolved to the script it pays through the table's
/// address gate. The script carries its kind, so nothing downstream classifies
/// the address again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Watched {
    pub(crate) address_id: i64,
    pub(crate) address: String,
    pub(crate) script: SpendableScript,
}

/// Rejects the whole watch set if any address is unspendable, because skipping
/// one could report a false zero balance.
pub(crate) fn resolve(
    params: &UtxoParams,
    addresses: &[WatchedAddress],
) -> Result<Vec<Watched>, UtxoError> {
    let mut watched = Vec::with_capacity(addresses.len());
    for entry in addresses {
        let found = entry.chain();
        if found != params.chain {
            return Err(UtxoError::ForeignChain { expected: params.chain, found });
        }
        let address = params.canonical(entry.address.value())?;
        let script = params.spendable_script(&address)?;
        watched.push(Watched { address_id: entry.id, address, script });
    }
    Ok(watched)
}

/// The build-side view of `watched`, borrowed in its order.
pub(crate) fn sources_of(watched: &[Watched]) -> Vec<Source<'_>> {
    watched.iter().map(|entry| Source { address: &entry.address, script: &entry.script }).collect()
}

/// The entries the inputs of `original` spend from, in input order, each once.
///
/// With none there is no address to fund a bump and no key to sign it, so that is
/// [`UtxoError::NotOurs`].
pub(crate) fn spenders(
    watched: &[Watched],
    original: &Original,
) -> Result<Vec<Watched>, UtxoError> {
    let mut spenders: Vec<Watched> = Vec::new();
    for coin in original.inputs() {
        if let Some(entry) =
            watched.iter().find(|entry| entry.script.script_pubkey() == coin.script_pubkey())
            && !spenders.contains(entry)
        {
            spenders.push(entry.clone());
        }
    }
    if spenders.is_empty() {
        return Err(UtxoError::NotOurs { txid: original.txid() });
    }
    Ok(spenders)
}

/// The sources a bump may draw coins from: `first` when there is one, which is where
/// the bump pays back to, then every one of `spenders` that is not it.
pub(crate) fn sources_led_by(first: Option<&Watched>, spenders: Vec<Watched>) -> Vec<Watched> {
    let mut sources: Vec<Watched> = first.into_iter().cloned().collect();
    for spender in spenders {
        if !sources.contains(&spender) {
            sources.push(spender);
        }
    }
    sources
}
