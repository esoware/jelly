use std::error::Error;
use std::slice::from_ref;

use bitcoin::consensus::deserialize;
use bitcoin::{OutPoint, Transaction};
use jelly_core::{FeeRate, ScriptKind};

use crate::build::{BuildRequest, BuiltTx, Source, build_tx};
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::preview::PreviewQuote;
use crate::select::{Selection, SelectionRequest, select_coins};
use crate::sign::sign_tx;
use crate::test_util::{
    BITCOIN, BITCOIN_CASH, LITECOIN, TestKey, fixture_txid, test_key, tx_paying, tx_spending,
};
use crate::utxo::{Origin, Utxo, units};

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

pub(crate) const TIP: u32 = 800_000;

/// A height a hundred blocks below [`TIP`], past the confirmation watch window.
pub(crate) const MINED_AT: u32 = 799_900;

/// What the stranger's funding transaction in [`received`] holds.
pub(crate) const FUNDED_SAT: u64 = 200_000;
/// What its payment to our address carries.
pub(crate) const RECEIVED_SAT: u64 = 190_000;

pub(crate) const ONE_PER_VB: FeeRate = FeeRate::from_sat_per_kwu(250);
pub(crate) const TEN_PER_VB: FeeRate = FeeRate::from_sat_per_kwu(2_500);

pub(crate) const FLOOR_QUOTE: PreviewQuote =
    PreviewQuote { relay_floor: ONE_PER_VB, fast: ONE_PER_VB, slow_market: ONE_PER_VB, evicted: 0 };

pub(crate) const fn kwu(value: u64) -> FeeRate {
    FeeRate::from_sat_per_kwu(value)
}

fn utxo(key: &TestKey, seed: u8, value: u64, height: Option<u32>, origin: Origin) -> Utxo {
    Utxo::new(
        OutPoint { txid: fixture_txid(usize::from(seed)), vout: u32::from(seed) },
        units(value),
        key.script().clone(),
        height,
        origin,
    )
}

/// Confirmed at [`TIP`], so no filter drops it.
pub(crate) fn confirmed(key: &TestKey, seed: u8, value: u64) -> Utxo {
    utxo(key, seed, value, Some(TIP), Origin::Ordinary)
}

/// Unconfirmed change from a transaction the wallet sent itself.
pub(crate) fn unconfirmed(key: &TestKey, seed: u8, value: u64) -> Utxo {
    utxo(key, seed, value, None, Origin::Own)
}

/// An unconfirmed payment from someone else.
pub(crate) fn incoming(key: &TestKey, seed: u8, value: u64) -> Utxo {
    utxo(key, seed, value, None, Origin::Ordinary)
}

/// The wallet's own fixture key and a stranger's, on one chain.
#[derive(Debug)]
pub(crate) struct Scene {
    pub(crate) params: UtxoParams,
    pub(crate) ours: TestKey,
    pub(crate) theirs: TestKey,
}

/// The scene's table prices change at one base unit per virtual byte, or at the
/// relay floor if that's higher. Every change figure in the tests assumes that rate.
pub(crate) fn scene(params: &UtxoParams, kind: ScriptKind) -> Fallible<Scene> {
    Ok(Scene {
        params: UtxoParams { long_term_fee_rate: ONE_PER_VB.max(params.min_relay_fee), ..*params },
        ours: test_key(params, kind, 1)?,
        theirs: test_key(params, kind, 2)?,
    })
}

pub(crate) fn bitcoin_scene() -> Fallible<Scene> {
    scene(&BITCOIN, ScriptKind::P2wpkh)
}

pub(crate) fn litecoin_scene() -> Fallible<Scene> {
    scene(&LITECOIN, ScriptKind::P2wpkh)
}

pub(crate) fn bitcoin_cash_scene() -> Fallible<Scene> {
    scene(&BITCOIN_CASH, ScriptKind::P2pkh)
}

impl Scene {
    /// Confirmed coins of the given values, each with its own outpoint.
    pub(crate) fn coins(&self, values: &[u64]) -> Vec<Utxo> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let seed = u8::try_from(index).unwrap_or(u8::MAX).saturating_add(1);
                confirmed(&self.ours, seed, *value)
            })
            .collect()
    }

    /// A `None` amount is a send-max, from the scene's own key alone.
    pub(crate) fn request<'a>(
        &'a self,
        amount: Option<u64>,
        rate: FeeRate,
        coins: &'a [Utxo],
    ) -> BuildRequest<'a> {
        BuildRequest {
            sources: self.own_source().to_vec(),
            to: self.theirs.address(),
            to_script: self.theirs.script().script_pubkey(),
            amount: amount.map(units),
            fee_rate: rate,
            coins,
            tip_height: TIP,
            relay_floor: self.params.min_relay_fee,
        }
    }

    pub(crate) fn own_source(&self) -> [Source<'_>; 1] {
        [Source { address: self.ours.address(), script: self.ours.script() }]
    }

    pub(crate) fn build(
        &self,
        amount: Option<u64>,
        rate: FeeRate,
        coins: &[Utxo],
    ) -> Result<BuiltTx, UtxoError> {
        build_tx(&self.params, &self.request(amount, rate, coins))
    }

    /// A build pooling the coins of every key in `keys`, change to the first.
    pub(crate) fn build_from(
        &self,
        keys: &[&TestKey],
        amount: Option<u64>,
        rate: FeeRate,
        coins: &[Utxo],
    ) -> Result<BuiltTx, UtxoError> {
        let sources = keys
            .iter()
            .map(|key| Source { address: key.address(), script: key.script() })
            .collect();
        let request = BuildRequest { sources, ..self.request(amount, rate, coins) };
        build_tx(&self.params, &request)
    }

    /// A selection paying the stranger, with change back to our own address.
    pub(crate) fn selection(&self, target: u64, rate: FeeRate) -> SelectionRequest<'_> {
        SelectionRequest {
            target: units(target),
            should_drain: false,
            fee_rate: rate,
            long_term_fee_rate: ONE_PER_VB,
            recipient_script_pubkey: self.theirs.script().script_pubkey(),
            change_script: self.ours.script(),
        }
    }

    pub(crate) fn select(
        &self,
        request: &SelectionRequest<'_>,
        coins: &[Utxo],
    ) -> Result<Selection, UtxoError> {
        select_coins(&self.params, request, coins)
    }
}

/// Our Bitcoin key and a stranger's, the stranger's funding transaction, and the
/// payment it makes to our address.
pub(crate) struct Received {
    pub(crate) ours: TestKey,
    pub(crate) theirs: TestKey,
    pub(crate) funding: Transaction,
    pub(crate) payment: Transaction,
}

/// Two transactions is the smallest history with a knowable fee and sender.
pub(crate) fn received() -> Fallible<Received> {
    let ours = test_key(&BITCOIN, ScriptKind::P2wpkh, 1)?;
    let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2)?;
    let funding = tx_paying(&[(theirs.script_pubkey(), FUNDED_SAT)]);
    let payment = tx_spending(&funding, 0, &[(ours.script_pubkey(), RECEIVED_SAT)]);
    Ok(Received { ours, theirs, funding, payment })
}

/// A transaction `scene` built and signed out of one confirmed coin worth `funded`,
/// resolved as the original a bump speeds up, with the transaction that funded it.
///
/// A `None` amount is a send-max.
pub(crate) fn sent_original(
    scene: &Scene,
    amount: Option<u64>,
    rate: FeeRate,
    funded: u64,
) -> Fallible<(Original, Vec<Transaction>)> {
    let coin = confirmed(&scene.ours, 1, funded);
    let built = scene.build(amount, rate, from_ref(&coin))?;
    let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()])?;
    let funding = vec![tx_paying(&[(scene.ours.script_pubkey(), funded)])];
    // The input has to name the funding fixture's outpoint for the original to
    // resolve.
    let mut tx: Transaction = deserialize(signed.raw())?;
    if let (Some(txin), Some(parent)) = (tx.input.first_mut(), funding.first()) {
        txin.previous_output = OutPoint { txid: parent.compute_txid(), vout: 0 };
    }
    let original = Original::new(tx, &funding)?;
    Ok((original, funding))
}
