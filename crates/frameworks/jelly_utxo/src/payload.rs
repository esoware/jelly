use std::collections::BTreeMap;

use bitcoin::hashes::Hash as _;
use bitcoin::psbt::raw::ProprietaryKey;
use bitcoin::psbt::{Input as PsbtInput, Psbt};
use bitcoin::{OutPoint, Transaction, TxOut, Txid, Weight};
use jelly_core::Amount;

use crate::coin::{Utxo, to_u64, tx_out, units, value_of};
use crate::error::UtxoError;
use crate::script::SpendableScript;
use crate::select::Strategy;
use crate::weight;

/// The BIP-174 proprietary key prefix every Jelly field carries.
const PREFIX: &[u8] = b"jelly";

/// Proprietary global field subtypes. They're wire format, so a number is never
/// reused for another meaning.
///
/// A source's field is keyed by its position as a big-endian `u32`, so the fields
/// sort in position order. Every other field has an empty key.
const FIELD_SOURCE: u8 = 0;
pub(crate) const FIELD_TO: u8 = 1;
const FIELD_STRATEGY: u8 = 2;
const FIELD_DONATED: u8 = 3;
const FIELD_REPLACES: u8 = 4;
const FIELD_REDUCED_PAYMENT: u8 = 5;
const FIELD_PAYS_FOR: u8 = 6;

/// The payment before and after a replacement took its extra fee out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaymentReduction {
    pub(crate) from: Amount,
    pub(crate) to: Amount,
}

/// The transaction a replacement replaces, and what it cost the recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Replaced {
    pub(crate) txid: Txid,
    pub(crate) reduced_payment: Option<PaymentReduction>,
}

/// What a build decided beyond what the transaction already says.
///
/// The fee, weight, script kinds, and inputs are read back off the PSBT rather
/// than recorded, so a payload can't disagree with itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Intent {
    /// The sending addresses, in the request's order. Change goes to the first, and
    /// the signer refuses a key that doesn't match its address.
    pub(crate) sources: Vec<String>,
    /// Where the payment is going, so the preview can tell the payment from the
    /// change when both pay the same address.
    pub(crate) to: String,
    pub(crate) strategy: Strategy,
    /// Leftover handed to the miner rather than paid back, because a change
    /// output of that size is worth less than it costs.
    pub(crate) donated: Amount,
    pub(crate) replaces: Option<Replaced>,
    /// For a child-pays-for-parent bump, the unconfirmed parent whose change it
    /// spends. Such a child moves nothing else.
    pub(crate) pays_for: Option<Txid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpentOutput {
    pub(crate) outpoint: OutPoint,
    pub(crate) value: Amount,
    pub(crate) script: SpendableScript,
}

impl SpentOutput {
    pub(crate) fn tx_out(&self) -> Result<TxOut, UtxoError> {
        tx_out(self.value, self.script.script_buf().clone())
    }
}

/// A BIP-174 PSBT plus the proprietary fields previews and replacements need, so
/// signing and the preview read the same transaction.
///
/// Parsing requires spendable prevout scripts, inputs that cover the outputs, and
/// the sender, recipient, and strategy fields.
///
/// Every input uses `witness_utxo`, legacy ones included. That's safe only because
/// sync checked values against the funding transactions and the payload never
/// leaves the process. A hardware signer would need the full parent transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UtxoPayload {
    psbt: Psbt,
    intent: Intent,
    inputs: Vec<SpentOutput>,
    fee: Amount,
    weight: Weight,
}

impl UtxoPayload {
    /// `inputs` must be the coins the transaction spends, in input order. A
    /// payload from outside the crate arrives through [`Self::decode`].
    pub(crate) fn assemble(
        tx: Transaction,
        inputs: &[Utxo],
        intent: &Intent,
    ) -> Result<Self, UtxoError> {
        if tx.input.len() != inputs.len() {
            return Err(UtxoError::payload("the input list does not match the transaction"));
        }
        let mut psbt = Psbt::from_unsigned_tx(tx)
            .map_err(|_| UtxoError::payload("the transaction is already signed"))?;
        psbt.inputs = inputs
            .iter()
            .map(|coin| {
                Ok(PsbtInput { witness_utxo: Some(coin.tx_out()?), ..PsbtInput::default() })
            })
            .collect::<Result<_, UtxoError>>()?;
        psbt.proprietary = stamp(intent)?;
        Self::from_psbt(psbt)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, UtxoError> {
        let psbt =
            Psbt::deserialize(bytes).map_err(|_| UtxoError::payload("the psbt does not parse"))?;
        Self::from_psbt(psbt)
    }

    /// A serialized PSBT, so any PSBT reader can inspect it.
    pub(crate) fn encode(&self) -> Vec<u8> {
        self.psbt.serialize()
    }

    #[cfg(test)]
    pub(crate) const fn psbt(&self) -> &Psbt {
        &self.psbt
    }

    pub(crate) const fn intent(&self) -> &Intent {
        &self.intent
    }

    pub(crate) const fn unsigned_tx(&self) -> &Transaction {
        &self.psbt.unsigned_tx
    }

    /// The key the outpoint claims hold this payload's coins under.
    pub(crate) fn unsigned_txid(&self) -> Txid {
        self.psbt.unsigned_tx.compute_txid()
    }

    /// The outputs this payload commits to spending, in input order.
    pub(crate) fn inputs(&self) -> &[SpentOutput] {
        &self.inputs
    }

    /// The outputs this payload spends, as the `TxOut`s a sighash cache takes.
    pub(crate) fn prevouts(&self) -> Result<Vec<TxOut>, UtxoError> {
        self.inputs.iter().map(SpentOutput::tx_out).collect()
    }

    pub(crate) fn outpoints(&self) -> Vec<OutPoint> {
        self.inputs.iter().map(|input| input.outpoint).collect()
    }

    pub(crate) const fn fee(&self) -> Amount {
        self.fee
    }

    /// Weight of the signed transaction, predicted from the input script kinds
    /// and the output scripts.
    pub(crate) const fn predicted_weight(&self) -> Weight {
        self.weight
    }

    fn from_psbt(psbt: Psbt) -> Result<Self, UtxoError> {
        if psbt.inputs.len() != psbt.unsigned_tx.input.len() {
            return Err(UtxoError::payload("the psbt has the wrong number of input maps"));
        }
        let mut inputs = Vec::with_capacity(psbt.inputs.len());
        for (index, (txin, map)) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs).enumerate() {
            let txout = map.witness_utxo.as_ref().ok_or(UtxoError::MissingPrevout { index })?;
            let script = SpendableScript::new(txout.script_pubkey.clone())?;
            inputs.push(SpentOutput {
                outpoint: txin.previous_output,
                value: value_of(txout),
                script,
            });
        }

        let spent = Amount::checked_sum(inputs.iter().map(|input| input.value))
            .ok_or_else(|| UtxoError::overflow("an input total"))?;
        let paid = Amount::checked_sum(psbt.unsigned_tx.output.iter().map(value_of))
            .ok_or_else(|| UtxoError::overflow("an output total"))?;
        let fee = spent
            .checked_sub(paid)
            .ok_or_else(|| UtxoError::payload("it spends more than its inputs hold"))?;

        let weight = weight::predict_weight(
            inputs.iter().map(|input| input.script.kind()),
            psbt.unsigned_tx.output.iter().map(|txout| txout.script_pubkey.as_script()),
        );

        let intent = read_intent(&psbt)?;
        Ok(Self { psbt, intent, inputs, fee, weight })
    }
}

fn key(subtype: u8) -> ProprietaryKey {
    ProprietaryKey { prefix: PREFIX.to_vec(), subtype, key: Vec::new() }
}

pub(crate) fn source_key(position: u32) -> ProprietaryKey {
    ProprietaryKey {
        prefix: PREFIX.to_vec(),
        subtype: FIELD_SOURCE,
        key: position.to_be_bytes().to_vec(),
    }
}

fn stamp(intent: &Intent) -> Result<BTreeMap<ProprietaryKey, Vec<u8>>, UtxoError> {
    if intent.sources.is_empty()
        || intent.sources.iter().any(String::is_empty)
        || intent.to.is_empty()
    {
        return Err(UtxoError::payload("the sender or the recipient is missing"));
    }
    let mut fields = BTreeMap::new();
    for (position, source) in (0u32..).zip(&intent.sources) {
        fields.insert(source_key(position), source.clone().into_bytes());
    }
    fields.insert(key(FIELD_TO), intent.to.clone().into_bytes());
    fields.insert(key(FIELD_STRATEGY), intent.strategy.as_str().as_bytes().to_vec());
    if !intent.donated.is_zero() {
        fields.insert(key(FIELD_DONATED), to_u64(intent.donated)?.to_le_bytes().to_vec());
    }
    if let Some(replaced) = intent.replaces {
        fields.insert(key(FIELD_REPLACES), replaced.txid.to_byte_array().to_vec());
        if let Some(reduction) = replaced.reduced_payment {
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&to_u64(reduction.from)?.to_le_bytes());
            bytes.extend_from_slice(&to_u64(reduction.to)?.to_le_bytes());
            fields.insert(key(FIELD_REDUCED_PAYMENT), bytes);
        }
    }
    if let Some(parent) = intent.pays_for {
        fields.insert(key(FIELD_PAYS_FOR), parent.to_byte_array().to_vec());
    }
    Ok(fields)
}

fn read_intent(psbt: &Psbt) -> Result<Intent, UtxoError> {
    let sources = sources(psbt)?;
    let to = text(psbt, FIELD_TO, "the recipient")?;
    let strategy = Strategy::parse(&text(psbt, FIELD_STRATEGY, "the selection strategy")?)
        .ok_or_else(|| UtxoError::payload("the selection strategy is not one this build knows"))?;
    let donated = psbt
        .proprietary
        .get(&key(FIELD_DONATED))
        .map(|bytes| amount(bytes))
        .transpose()?
        .unwrap_or(Amount::ZERO);
    let replaces = match psbt.proprietary.get(&key(FIELD_REPLACES)) {
        None => None,
        Some(bytes) => {
            let raw: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| UtxoError::payload("the replaced transaction id is not 32 bytes"))?;
            let reduced_payment = match psbt.proprietary.get(&key(FIELD_REDUCED_PAYMENT)) {
                None => None,
                Some(bytes) => {
                    let (before, after) = bytes.split_at_checked(8).ok_or_else(|| {
                        UtxoError::payload("the payment reduction is not two amounts")
                    })?;
                    Some(PaymentReduction { from: amount(before)?, to: amount(after)? })
                }
            };
            Some(Replaced { txid: Txid::from_byte_array(raw), reduced_payment })
        }
    };
    let pays_for = psbt
        .proprietary
        .get(&key(FIELD_PAYS_FOR))
        .map(|bytes| {
            <[u8; 32]>::try_from(bytes.as_slice())
                .map(Txid::from_byte_array)
                .map_err(|_| UtxoError::payload("the parent transaction id is not 32 bytes"))
        })
        .transpose()?;
    Ok(Intent { sources, to, strategy, donated, replaces, pays_for })
}

/// The source fields in position order, which have to run from zero without a gap or
/// a repeat.
fn sources(psbt: &Psbt) -> Result<Vec<String>, UtxoError> {
    let mut sources = Vec::new();
    for (position, (field, value)) in (0u32..).zip(
        psbt.proprietary
            .iter()
            .filter(|(field, _)| field.prefix == PREFIX && field.subtype == FIELD_SOURCE),
    ) {
        let expected = source_key(position);
        if *field != expected {
            return Err(UtxoError::payload("the sending addresses are out of order"));
        }
        let source = String::from_utf8(value.clone())
            .map_err(|_| UtxoError::payload("the sending address"))?;
        sources.push(source);
    }
    if sources.is_empty() {
        return Err(UtxoError::payload("the sending address"));
    }
    Ok(sources)
}

fn text(psbt: &Psbt, subtype: u8, what: &'static str) -> Result<String, UtxoError> {
    let bytes = psbt.proprietary.get(&key(subtype)).ok_or_else(|| UtxoError::payload(what))?;
    String::from_utf8(bytes.clone()).map_err(|_| UtxoError::payload(what))
}

fn amount(bytes: &[u8]) -> Result<Amount, UtxoError> {
    let raw: [u8; 8] =
        bytes.try_into().map_err(|_| UtxoError::payload("an amount field is not eight bytes"))?;
    Ok(units(u64::from_le_bytes(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{ONE_PER_VB, bitcoin_scene};

    /// The field numbers are wire format, so changing one changes what an existing
    /// unsigned transaction means.
    #[test]
    fn the_proprietary_field_numbers_are_fixed() {
        assert_eq!(PREFIX, b"jelly");
        assert_eq!(
            [
                FIELD_SOURCE,
                FIELD_TO,
                FIELD_STRATEGY,
                FIELD_DONATED,
                FIELD_REPLACES,
                FIELD_REDUCED_PAYMENT,
                FIELD_PAYS_FOR
            ],
            [0, 1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn a_payload_round_trips_as_a_psbt() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let bytes = built.payload.encode();

        let raw = Psbt::deserialize(&bytes).unwrap();
        assert_eq!(raw.unsigned_tx, *built.payload.unsigned_tx());
        assert_eq!(raw.inputs.len(), raw.unsigned_tx.input.len());
        assert!(raw.inputs.iter().all(|input| input.witness_utxo.is_some()));
        assert_eq!(
            raw.proprietary.len(),
            3,
            "the source, to and the strategy; nothing was donated"
        );

        let back = UtxoPayload::decode(&bytes).unwrap();
        assert_eq!(back, built.payload);
        assert_eq!(back.intent().sources, [scene.ours.address()]);
        assert_eq!(back.intent().to, scene.theirs.address());
        assert_eq!(back.intent().strategy, built.selection.strategy);
        assert_eq!(back.fee(), built.selection.fee);
        assert_eq!(back.predicted_weight(), built.selection.weight);
        assert_eq!(back.encode(), bytes, "the encoding is canonical");
    }

    #[test]
    fn a_payload_that_is_not_a_psbt_is_refused_rather_than_signed() {
        assert!(matches!(UtxoPayload::decode(b""), Err(UtxoError::Payload { .. })));
        assert!(matches!(UtxoPayload::decode(b"not a psbt"), Err(UtxoError::Payload { .. })));
    }

    #[test]
    fn a_psbt_with_no_intent_is_not_a_payload() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let mut stripped = built.payload.psbt().clone();
        stripped.proprietary.clear();
        assert!(matches!(
            UtxoPayload::decode(&stripped.serialize()),
            Err(UtxoError::Payload { .. })
        ));
    }

    #[test]
    fn a_payload_missing_a_previous_output_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let mut blinded = built.payload.psbt().clone();
        for input in &mut blinded.inputs {
            input.witness_utxo = None;
        }
        assert!(matches!(
            UtxoPayload::decode(&blinded.serialize()),
            Err(UtxoError::MissingPrevout { index: 0 })
        ));
    }

    #[test]
    fn a_payload_that_spends_more_than_it_holds_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let mut inflated = built.payload.psbt().clone();
        for output in &mut inflated.unsigned_tx.output {
            output.value = bitcoin::Amount::from_sat(1_000_000);
        }
        assert!(matches!(
            UtxoPayload::decode(&inflated.serialize()),
            Err(UtxoError::Payload { .. })
        ));
    }

    mod psbt {
        use serde::Deserialize;

        use super::*;

        const PSBT: &str = include_str!("../tests/vectors/psbt.json");

        #[derive(Deserialize)]
        struct File {
            invalid: Vec<Invalid>,
        }

        #[derive(Deserialize)]
        struct Invalid {
            name: String,
            hex: String,
        }

        #[test]
        fn every_malformed_psbt_is_refused() {
            let file: File = serde_json::from_str(PSBT).unwrap();
            assert!(file.invalid.len() >= 5);
            for case in &file.invalid {
                let raw = hex::decode(&case.hex).unwrap();
                assert!(Psbt::deserialize(&raw).is_err(), "{} was accepted", case.name);
                assert!(
                    UtxoPayload::decode(&raw).is_err(),
                    "{} was accepted as a payload",
                    case.name
                );
            }
        }
    }
}
