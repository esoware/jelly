use bitcoin::consensus::deserialize;
use bitcoin::{Script, ScriptBuf, Transaction};
use jelly_core::{
    Amount, FeeLevel, FeeRate, KeyMaterial, RawTx, RetryPolicy, SendAmount, SignedTx, TxId,
    TxPayload, TxRequest, UnsignedTx,
};

use crate::adapter::client::UtxoClient;
use crate::adapter::fee::FeeContext;
use crate::adapter::watched::{Watched, sources_of};
use crate::build::{BuildRequest, BuiltTx, build_tx};
use crate::coin::Utxo;
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::payload::UtxoPayload;
use crate::preview::{PreviewQuote, preview};
use crate::script::SpendableScript;
use crate::sign::sign_tx;

/// Everything a build needs that does not depend on the fee rate, so a quote is
/// priced against exactly the transaction that would be built.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// In the request's order. Change goes to the first.
    sources: Vec<Watched>,
    /// The script change pays, the first source's.
    change_script: SpendableScript,
    /// Already through the table's address gate and canonical, so no later step
    /// is the one that discovers a refused form.
    to: String,
    to_script: ScriptBuf,
    /// Absent exactly when the request is a send-max.
    amount: Option<Amount>,
    /// The spendable coins of every source.
    coins: Vec<Utxo>,
    tip_height: u32,
}

impl Plan {
    /// `known_tip` is the tip the last pass saw. The anti-fee-sniping locktime has
    /// to name a height the network already has, so with no pass behind it the
    /// height is fetched. The recipient goes through the gate first, so an address
    /// this build will not pay costs no round trip.
    pub(crate) async fn resolve(
        params: &UtxoParams,
        client: &UtxoClient,
        sources: Vec<Watched>,
        coins: Vec<Utxo>,
        known_tip: Option<u32>,
        request: &TxRequest,
    ) -> Result<Self, UtxoError> {
        let change_script =
            sources.first().map(|source| source.script.clone()).ok_or(UtxoError::NoSource)?;
        let to = params.canonical(&request.to)?;
        let to_script = params.script_pubkey(&to)?;
        let amount = match request.amount {
            SendAmount::Max => None,
            SendAmount::Exact { value } => Some(value),
        };
        let tip_height = tip_height(client, known_tip).await?;
        Ok(Self { sources, change_script, to, to_script, amount, coins, tip_height })
    }

    pub(crate) fn to_script(&self) -> &Script {
        &self.to_script
    }

    pub(crate) const fn is_send_max(&self) -> bool {
        self.amount.is_none()
    }

    pub(crate) const fn change_script(&self) -> &SpendableScript {
        &self.change_script
    }
}

/// The height the anti-fee-sniping locktime names.
pub(crate) async fn tip_height(client: &UtxoClient, known: Option<u32>) -> Result<u32, UtxoError> {
    match known {
        Some(height) => Ok(height),
        None => client.tip_height().await,
    }
}

/// Does no I/O. Everything the network had to say is already in `fee_context` and
/// `plan`.
pub(crate) fn build(
    fee_context: &FeeContext,
    plan: &Plan,
    rate: FeeRate,
) -> Result<BuiltTx, UtxoError> {
    let request = BuildRequest {
        sources: sources_of(&plan.sources),
        to: &plan.to,
        to_script: &plan.to_script,
        amount: plan.amount,
        fee_rate: rate,
        coins: &plan.coins,
        tip_height: plan.tip_height,
        relay_floor: fee_context.relay_floor,
    };
    build_tx(&fee_context.params, &request)
}

/// Decodes the preview from the payload the signer will consume, so nothing the
/// user approved can differ from what gets signed.
///
/// The preview also carries the table's address notes, such as Litecoin's `3...`
/// ambiguity warning. `evicted` counts the later transactions a replacement
/// cancels, which the preview warns about.
pub(crate) fn unsigned(
    built: &BuiltTx,
    fee_context: &FeeContext,
    evicted: usize,
) -> Result<UnsignedTx, UtxoError> {
    let quote = PreviewQuote {
        relay_floor: fee_context.relay_floor,
        fast: fee_context.level(FeeLevel::Fast),
        slow_market: fee_context.slow_market,
        evicted,
    };
    let preview = preview(&fee_context.params, &built.payload, quote)?;
    Ok(UnsignedTx { preview, payload: TxPayload::from_bytes(built.payload.encode()) })
}

/// The engine payload of an unsigned transaction, checked for this chain.
pub(crate) fn payload_of(params: &UtxoParams, tx: &UnsignedTx) -> Result<UtxoPayload, UtxoError> {
    let found = tx.chain();
    if found != params.chain {
        return Err(UtxoError::ForeignChain { expected: params.chain, found });
    }
    UtxoPayload::decode(tx.payload.as_bytes())
}

/// Signs with no I/O, refusing a key that does not reproduce the source it is
/// handed for. `keys` follows the preview's sources, which are the payload's.
///
/// Every table shares the transaction format; the table supplies the address
/// codec and the fork id.
pub(crate) fn sign(
    params: &UtxoParams,
    tx: &UnsignedTx,
    keys: &[KeyMaterial],
) -> Result<SignedTx, UtxoError> {
    let payload = payload_of(params, tx)?;
    if payload.intent().sources != tx.preview.from {
        return Err(UtxoError::payload("the sources differ from the preview's"));
    }
    let (raw, txid) = sign_tx(params, &payload, keys)?.into_parts();
    Ok(SignedTx { raw: RawTx::from_bytes(raw), txid: params.txid(txid) })
}

/// Sends a signed transaction to one endpoint.
///
/// A transaction the network already has is a send that landed, not a failure.
/// Every other refusal reaches the caller as the node's classified rejection,
/// which decides whether a re-send could ever work. Litecoin Core, Bitcoin Cash
/// Node and Dogecoin Core emit Bitcoin Core's reject strings, so the one
/// classification table in the Electrum layer covers every table.
pub(crate) async fn deliver(
    params: &UtxoParams,
    client: &UtxoClient,
    tx: &SignedTx,
) -> Result<TxId, UtxoError> {
    let found = tx.chain();
    if found != params.chain {
        return Err(UtxoError::ForeignChain { expected: params.chain, found });
    }
    let decoded: Transaction =
        deserialize(tx.raw.as_bytes()).map_err(|_| UtxoError::NotATransaction)?;
    match client.broadcast(&decoded).await {
        Ok(txid) => Ok(params.txid(txid)),
        Err(error)
            if error
                .rejection()
                .is_some_and(|rejection| rejection.retry == RetryPolicy::AlreadyLanded) =>
        {
            Ok(tx.txid.clone())
        }
        Err(error) => Err(error),
    }
}
