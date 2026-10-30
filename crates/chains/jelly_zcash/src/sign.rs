use jelly_core::{KeyKind, KeyMaterial, RawTx, SignedTx, TxId, TxPreview};
use jelly_keypair::Secp256k1Key;
use zcash_primitives::transaction::{
    Authorization, Authorized, Transaction, TransactionData, TxVersion,
    sighash::{SignableInput, signature_hash},
    txid::TxIdDigester,
};
use zcash_protocol::{
    consensus::{BlockHeight, BranchId},
    value::Zatoshis,
};
use zcash_script::{pv, script::Component};
use zcash_transparent::{
    address::Script,
    bundle::{self, Bundle, TxIn, TxOut},
    sighash::{self, SighashType, TransparentAuthorizingContext},
};

use crate::address::{pubkey_address, script};
use crate::config::CHAIN;
use crate::error::ZcashError;
use crate::payload::Payment;
use crate::preview;

// Zcash's maximum block size, which also bounds a transaction.
const MAX_TX_BYTES: usize = 2_000_000;
const FOREIGN_KEY: &str = "The signing key does not own these Zcash inputs.";
const OUT_OF_RANGE: &str = "a validated Zcash amount is out of range";

#[derive(Debug)]
struct InputContext {
    amounts: Vec<Zatoshis>,
    scripts: Vec<Script>,
}

impl bundle::Authorization for InputContext {
    type ScriptSig = Script;
}

impl TransparentAuthorizingContext for InputContext {
    fn input_amounts(&self) -> Vec<Zatoshis> {
        self.amounts.clone()
    }

    fn input_scriptpubkeys(&self) -> Vec<Script> {
        self.scripts.clone()
    }
}

#[derive(Debug)]
struct Signing;

impl Authorization for Signing {
    type TransparentAuth = InputContext;
    type SaplingAuth = <Authorized as Authorization>::SaplingAuth;
    type OrchardAuth = <Authorized as Authorization>::OrchardAuth;
}

pub(crate) fn decode(raw: &[u8]) -> Result<Transaction, ZcashError> {
    if raw.len() > MAX_TX_BYTES {
        return Err(ZcashError::Protocol);
    }
    let mut remaining = raw;
    // A v5 transaction carries its own branch id. From an earlier version we only read the
    // txid and the bundles, and neither depends on the branch.
    let tx =
        Transaction::read(&mut remaining, BranchId::Sprout).map_err(|_| ZcashError::Protocol)?;
    if !remaining.is_empty() {
        return Err(ZcashError::Protocol);
    }
    Ok(tx)
}

/// Decodes `raw`, refusing it unless `payment` is valid and `raw` is exactly that payment,
/// signed. Callers only look at whether it was refused.
pub(crate) fn validate_signed(payment: &Payment, raw: &[u8]) -> Result<Transaction, ZcashError> {
    payment.validate()?;
    let tx = decode(raw)?;
    if tx.version() != TxVersion::V5
        || tx.consensus_branch_id() != payment.branch()
        || tx.lock_time() != 0
        || u32::from(tx.expiry_height()) != payment.expiry_height
        || tx.sprout_bundle().is_some()
        || tx.sapling_bundle().is_some()
        || tx.orchard_bundle().is_some()
        || tx.ironwood_bundle().is_some()
    {
        return Err(ZcashError::Protocol);
    }
    let bundle = tx.transparent_bundle().ok_or(ZcashError::Protocol)?;
    if bundle.vin.len() != payment.inputs.len() || bundle.vout != outputs(payment)? {
        return Err(ZcashError::Protocol);
    }
    for (input, coin) in bundle.vin.iter().zip(&payment.inputs) {
        if *input.prevout() != coin.outpoint()?
            || input.sequence() != u32::MAX
            || input.script_sig().0.0.is_empty()
        {
            return Err(ZcashError::Protocol);
        }
    }
    Ok(tx)
}

/// One source's signing key and the compressed public key its address hashes.
struct Signer {
    key: Secp256k1Key,
    pubkey: [u8; 33],
}

/// Pairs each of `payment`'s sources with its key from `keys`, which is in source order,
/// and refuses a key that doesn't own its source.
fn signers(payment: &Payment, keys: &[KeyMaterial]) -> Result<Vec<Signer>, ZcashError> {
    if keys.len() != payment.sources.len()
        || keys.iter().any(|key| key.kind() != KeyKind::Secp256k1)
    {
        return Err(ZcashError::Invalid(FOREIGN_KEY));
    }
    let mut signers = Vec::with_capacity(keys.len());
    for (source, material) in payment.sources.iter().zip(keys) {
        let key = material.with_bytes(Secp256k1Key::from_secret_bytes)?;
        let pubkey = key.public_compressed();
        if pubkey_address(&pubkey)? != source.address {
            return Err(ZcashError::Invalid(FOREIGN_KEY));
        }
        signers.push(Signer { key, pubkey });
    }
    Ok(signers)
}

/// The payment, then the change when there is any.
fn outputs(payment: &Payment) -> Result<Vec<TxOut>, ZcashError> {
    let mut outputs = vec![TxOut::new(
        Zatoshis::from_u64(payment.amount_zat).map_err(|_| ZcashError::Internal(OUT_OF_RANGE))?,
        script(&payment.to)?,
    )];
    if payment.change_zat > 0 {
        outputs.push(TxOut::new(
            Zatoshis::from_u64(payment.change_zat)
                .map_err(|_| ZcashError::Internal(OUT_OF_RANGE))?,
            script(payment.change_address()?)?,
        ));
    }
    Ok(outputs)
}

/// Signs each input of `payment` with the key of the source that holds its coin.
///
/// `payment` must have passed `Payment::decode` and still match `approved`. `keys` is in
/// source order, and a key that doesn't own its source is refused before anything is
/// signed.
pub(crate) fn sign(
    payment: &Payment,
    approved: &TxPreview,
    keys: &[KeyMaterial],
) -> Result<SignedTx, ZcashError> {
    if preview::of(payment) != *approved {
        return Err(ZcashError::Invalid("The Zcash payment no longer matches its preview."));
    }
    let signers = signers(payment, keys)?;
    let mut amounts = Vec::with_capacity(payment.inputs.len());
    let mut scripts = Vec::with_capacity(payment.inputs.len());
    let mut owners = Vec::with_capacity(payment.inputs.len());
    let mut inputs = Vec::with_capacity(payment.inputs.len());
    for coin in &payment.inputs {
        let owner = payment
            .sources
            .iter()
            .position(|source| source.address == coin.address)
            .and_then(|position| signers.get(position))
            .ok_or(ZcashError::Invalid(FOREIGN_KEY))?;
        amounts.push(
            Zatoshis::from_u64(coin.value_zat).map_err(|_| ZcashError::Internal(OUT_OF_RANGE))?,
        );
        scripts.push(script(&coin.address)?);
        owners.push(owner);
        inputs.push(TxIn::from_parts(coin.outpoint()?, Script::default(), u32::MAX));
    }
    let outputs = outputs(payment)?;
    let context = InputContext { amounts, scripts };
    let data = TransactionData::<Signing>::from_parts(
        TxVersion::V5,
        payment.branch(),
        0,
        BlockHeight::from_u32(payment.expiry_height),
        Some(Bundle { vin: inputs, vout: outputs.clone(), authorization: context }),
        None,
        None,
        None,
    );
    let bundle = data
        .transparent_bundle()
        .ok_or(ZcashError::Internal("a Zcash payment has no transparent bundle"))?;
    let digests = data.digest(TxIdDigester);
    let too_long = || ZcashError::Internal("a Zcash signature does not fit a script");
    let mut signed_inputs = Vec::with_capacity(bundle.vin.len());
    for (index, ((input, value), (spent, owner))) in bundle
        .vin
        .iter()
        .zip(&bundle.authorization.amounts)
        .zip(bundle.authorization.scripts.iter().zip(&owners))
        .enumerate()
    {
        let signable = sighash::SignableInput::from_parts(
            bundle,
            SighashType::ALL,
            index,
            spent,
            spent,
            *value,
        )
        .map_err(|_| ZcashError::Internal("a Zcash input is not signable"))?;
        let digest = signature_hash(&data, &SignableInput::Transparent(signable), &digests);
        let mut signature = owner.key.sign_prehash_der(digest.as_ref())?;
        signature.push(SighashType::ALL.encode());
        let unlock = Component(vec![
            pv::push_value(&signature).ok_or_else(too_long)?,
            pv::push_value(&owner.pubkey).ok_or_else(too_long)?,
        ]);
        signed_inputs.push(TxIn::from_parts(
            input.prevout().clone(),
            Script::from(unlock),
            u32::MAX,
        ));
    }
    let signed = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        payment.branch(),
        0,
        BlockHeight::from_u32(payment.expiry_height),
        Some(Bundle { vin: signed_inputs, vout: outputs, authorization: bundle::Authorized }),
        None,
        None,
        None,
    )
    .freeze()
    .map_err(|_| ZcashError::Internal("a signed Zcash transaction is not valid"))?;
    let mut raw = Vec::new();
    signed
        .write(&mut raw)
        .map_err(|_| ZcashError::Internal("a signed Zcash transaction did not serialize"))?;
    Ok(SignedTx { raw: RawTx::from_bytes(raw), txid: TxId::new(CHAIN, signed.txid().to_string()) })
}

#[cfg(test)]
mod tests {
    use jelly_keypair::verification_context;
    use secp256k1::{Message, PublicKey, ecdsa::Signature};
    use serde::Deserialize;
    use zcash_script::script::Code;

    use super::*;
    use crate::fixtures::{RECIPIENT, key, key_address};
    use crate::payload::{Source, Utxo};

    #[derive(Deserialize)]
    struct Vector {
        tx: String,
        txid: String,
        amounts: Vec<u64>,
        scripts: Vec<String>,
        index: usize,
        sighash: String,
    }

    fn vectors() -> Vec<Vector> {
        serde_json::from_str(include_str!("../tests/vectors/zip244.json")).unwrap()
    }

    fn with_context(tx: &Transaction, context: InputContext) -> TransactionData<Signing> {
        let bundle = tx.transparent_bundle().unwrap();
        TransactionData::<Signing>::from_parts(
            tx.version(),
            tx.consensus_branch_id(),
            tx.lock_time(),
            tx.expiry_height(),
            Some(Bundle {
                vin: bundle
                    .vin
                    .iter()
                    .map(|input| {
                        TxIn::from_parts(
                            input.prevout().clone(),
                            input.script_sig().clone(),
                            input.sequence(),
                        )
                    })
                    .collect(),
                vout: bundle.vout.clone(),
                authorization: context,
            }),
            tx.sprout_bundle().cloned(),
            tx.sapling_bundle().cloned(),
            tx.orchard_bundle().cloned(),
        )
    }

    #[test]
    fn official_zip244_vectors_pin_byte_order_amounts_scripts_and_signing_hashes() {
        let vectors = vectors();
        assert_eq!(vectors.len(), 3);
        for vector in vectors {
            let tx = decode(&hex::decode(&vector.tx).unwrap()).unwrap();
            assert_eq!(hex::encode(tx.txid().as_ref()), vector.txid);
            let context = InputContext {
                amounts: vector
                    .amounts
                    .into_iter()
                    .map(|value| Zatoshis::from_u64(value).unwrap())
                    .collect(),
                scripts: vector
                    .scripts
                    .into_iter()
                    .map(|script| Script(Code(hex::decode(script).unwrap())))
                    .collect(),
            };
            let data = with_context(&tx, context);
            let bundle = data.transparent_bundle().unwrap();
            let script = &bundle.authorization.scripts[vector.index];
            let value = bundle.authorization.amounts[vector.index];
            let input = sighash::SignableInput::from_parts(
                bundle,
                SighashType::ALL,
                vector.index,
                script,
                script,
                value,
            )
            .unwrap();
            let digest = signature_hash(
                &data,
                &SignableInput::Transparent(input),
                &data.digest(TxIdDigester),
            );
            assert_eq!(hex::encode(digest.as_ref()), vector.sighash);
        }
    }

    /// Two sources, the first holding one coin and the second two, each input signed
    /// by its own source's key over the ZIP 244 digest of its own script.
    #[test]
    fn generated_signatures_authenticate_every_input_by_its_own_sources_key() {
        let (first, second) = (key(7), key(8));
        let (first_address, second_address) = (key_address(7), key_address(8));
        let coin = |txid: &str, index, value_zat, address: &str| Utxo {
            txid: txid.repeat(32),
            index,
            value_zat,
            address: address.to_owned(),
        };
        let payment = Payment {
            sources: vec![
                Source { address_id: 1, address: first_address.clone() },
                Source { address_id: 2, address: second_address.clone() },
            ],
            to: RECIPIENT.to_owned(),
            target_height: 3_514_159,
            expiry_height: Payment::expiry_height(3_514_159).unwrap(),
            inputs: vec![
                coin("22", 2, 200_000, &second_address),
                coin("11", 0, 100_000, &first_address),
                coin("33", 1, 50_000, &second_address),
            ],
            amount_zat: 300_000,
            change_zat: 35_000,
            fee_zat: 15_000,
            dropped_change_zat: 0,
        };
        payment.validate().unwrap();
        let approved = preview::of(&payment);
        assert_eq!(approved.from, [first_address.clone(), second_address]);
        let keys = [first.key_material(), second.key_material()];
        let signed = sign(&payment, &approved, &keys).unwrap();
        let tx = decode(signed.raw.as_bytes()).unwrap();
        let scripts: Vec<Script> =
            payment.inputs.iter().map(|coin| script(&coin.address).unwrap()).collect();
        let context = InputContext {
            amounts: payment
                .inputs
                .iter()
                .map(|coin| Zatoshis::from_u64(coin.value_zat).unwrap())
                .collect(),
            scripts: scripts.clone(),
        };
        let data = with_context(&tx, context);
        let bundle = data.transparent_bundle().unwrap();
        assert_eq!(bundle.vout[1].script_pubkey(), &script(&first_address).unwrap());
        for (index, input) in bundle.vin.iter().enumerate() {
            let bytes = &input.script_sig().0.0;
            let signature_length = usize::from(bytes[0]);
            assert_eq!(bytes[signature_length], 1);
            assert_eq!(bytes[signature_length.saturating_add(1)], 33);
            let signature = Signature::from_der(&bytes[1..signature_length]).unwrap();
            let pubkey =
                PublicKey::from_slice(&bytes[signature_length.saturating_add(2)..]).unwrap();
            let signable = sighash::SignableInput::from_parts(
                bundle,
                SighashType::ALL,
                index,
                &scripts[index],
                &scripts[index],
                bundle.authorization.amounts[index],
            )
            .unwrap();
            let digest = signature_hash(
                &data,
                &SignableInput::Transparent(signable),
                &data.digest(TxIdDigester),
            );
            verification_context()
                .verify_ecdsa(&Message::from_digest(*digest.as_ref()), &signature, &pubkey)
                .unwrap();
            assert_eq!(pubkey_address(&pubkey.serialize()).unwrap(), payment.inputs[index].address);
        }

        let swapped = [second.key_material(), first.key_material()];
        let refused = sign(&payment, &approved, &swapped);
        assert!(matches!(refused, Err(ZcashError::Invalid(FOREIGN_KEY))), "{refused:?}");
        let missing = [first.key_material()];
        let refused = sign(&payment, &approved, &missing);
        assert!(matches!(refused, Err(ZcashError::Invalid(FOREIGN_KEY))), "{refused:?}");
    }

    #[test]
    fn the_transaction_decoder_refuses_trailing_and_truncated_bytes() {
        let mut bytes = hex::decode(&vectors()[0].tx).unwrap();
        bytes.push(0);
        decode(&bytes).unwrap_err();
        bytes.truncate(bytes.len().saturating_sub(2));
        decode(&bytes).unwrap_err();
    }
}
