use bitcoin::consensus::{Encodable as _, serialize};
use bitcoin::hashes::{Hash as _, HashEngine as _, sha256d};
use bitcoin::key::TapTweak as _;
use bitcoin::script::{PushBytes, PushBytesBuf};
use bitcoin::secp256k1::{Keypair, Message, SecretKey};
use bitcoin::sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType};
use bitcoin::{
    CompressedPublicKey, PublicKey, Script, ScriptBuf, Transaction, TxOut, Txid, Witness, ecdsa,
    io, taproot,
};
use jelly_core::{KeyKind, KeyMaterial, random_bytes};
use jelly_keypair::{signing_context, verification_context};

use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::payload::UtxoPayload;
use crate::script::{SpendableScript, UtxoScript};

/// `SIGHASH_ALL`, the only type this build signs with.
const SIGHASH_ALL: u32 = 0x01;

/// `SIGHASH_FORKID`, which Bitcoin Cash requires on every signature.
const SIGHASH_FORKID: u32 = 0x40;

/// A signed transaction with its wire bytes and txid, produced together so
/// nothing can serialize a different transaction than the one signed.
///
/// The encoder emits the BIP-144 marker only when an input carries a witness,
/// because nodes reject a witness serialization with no witness data.
#[derive(Debug, Clone)]
pub(crate) struct Signed {
    raw: Vec<u8>,
    txid: Txid,
}

impl Signed {
    #[cfg(test)]
    pub(crate) fn raw(&self) -> &[u8] {
        &self.raw
    }

    #[cfg(test)]
    pub(crate) const fn txid(&self) -> Txid {
        self.txid
    }

    pub(crate) fn into_parts(self) -> (Vec<u8>, Txid) {
        (self.raw, self.txid)
    }
}

/// Signs every input with the key of the source it spends from, with no network I/O
/// or retained state.
///
/// `keys` holds one key per source of the payload, in its order, and each source's
/// address is re-derived from its key and compared before anything is hashed.
///
/// An input that spends from no source is refused, so nothing leaves unsigned.
pub(crate) fn sign_tx(
    params: &UtxoParams,
    payload: &UtxoPayload,
    keys: &[KeyMaterial],
) -> Result<Signed, UtxoError> {
    let sources = &payload.intent().sources;
    if keys.len() != sources.len() {
        return Err(UtxoError::WrongKey);
    }
    if keys.iter().any(|key| key.kind() != KeyKind::Secp256k1) {
        return Err(UtxoError::signing("the key is not a secp256k1 scalar"));
    }
    let scripts: Vec<SpendableScript> =
        sources.iter().map(|source| params.spendable_script(source)).collect::<Result<_, _>>()?;
    let owned: Vec<Vec<usize>> = scripts
        .iter()
        .map(|own| {
            payload
                .inputs()
                .iter()
                .enumerate()
                .filter(|(_, input)| input.script == *own)
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    let covered = owned.iter().map(Vec::len).fold(0usize, usize::saturating_add);
    if covered != payload.inputs().len() {
        return Err(UtxoError::payload(
            "an input spends from an address the payload does not name",
        ));
    }

    let prevouts = payload.prevouts()?;
    let mut signing = SigningTx::new(payload.unsigned_tx(), &prevouts);
    for (((source, own), ours), key) in sources.iter().zip(&scripts).zip(&owned).zip(keys) {
        let signer = Signer { source, own, ours };
        key.with_bytes(|secret| sign_with(params, &signer, secret, &mut signing))?;
    }
    let raw = serialize(&signing.signed);
    let txid = signing.signed.compute_txid();
    Ok(Signed { raw, txid })
}

/// One source's share of a signing pass.
struct Signer<'a> {
    source: &'a str,
    own: &'a SpendableScript,
    /// The inputs that spend from it.
    ours: &'a [usize],
}

fn sign_with(
    params: &UtxoParams,
    signer: &Signer<'_>,
    secret: &[u8],
    signing: &mut SigningTx<'_>,
) -> Result<(), UtxoError> {
    let mut key = SecretKey::from_slice(secret)
        .map_err(|_| UtxoError::signing("the key is not a valid secp256k1 scalar"))?;
    let mut keypair = Keypair::from_secret_key(signing_context(), &key);
    // The keypair holds its own copy of the scalar from here on, so the loose one
    // goes immediately. `fill` wipes each copy it derives for one signature, and
    // the keypair is wiped on every path out of `Signer::sign`.
    key.non_secure_erase();

    let outcome = signer.sign(params, &keypair, signing);
    keypair.non_secure_erase();
    outcome
}

impl Signer<'_> {
    /// Split out so the caller can wipe the key on every path out of it rather than
    /// at each early return.
    fn sign(
        &self,
        params: &UtxoParams,
        keypair: &Keypair,
        signing: &mut SigningTx<'_>,
    ) -> Result<(), UtxoError> {
        let compressed = keypair.public_key().serialize();
        if params.key_address(self.own.kind().into(), &compressed)? != self.source {
            return Err(UtxoError::WrongKey);
        }
        signing.fill(
            self.own.kind(),
            params.sighash_fork_id,
            self.ours,
            self.own.script_pubkey(),
            keypair,
        )
    }
}

/// Signatures go into a copy of the transaction, because the sighash cache borrows
/// the unsigned one. Every source writes into the same copy.
struct SigningTx<'a> {
    unsigned: &'a Transaction,
    prevouts: &'a [TxOut],
    signed: Transaction,
}

impl<'a> SigningTx<'a> {
    fn new(unsigned: &'a Transaction, prevouts: &'a [TxOut]) -> Self {
        Self { unsigned, prevouts, signed: unsigned.clone() }
    }

    /// `fork_id` is [`UtxoParams::sighash_fork_id`].
    fn fill(
        &mut self,
        kind: UtxoScript,
        fork_id: Option<u32>,
        ours: &[usize],
        own_script_pubkey: &Script,
        keypair: &Keypair,
    ) -> Result<(), UtxoError> {
        let tx = self.unsigned;
        let prevouts = self.prevouts;
        let mut cache = SighashCache::new(tx);
        let public = PublicKey::new(keypair.public_key());
        let compressed = CompressedPublicKey(keypair.public_key());

        for index in ours.iter().copied() {
            let value = prevouts.get(index).ok_or(UtxoError::MissingPrevout { index })?.value;
            let input = self
                .signed
                .input
                .get_mut(index)
                .ok_or(UtxoError::signing("an input index is out of range"))?;
            match kind {
                UtxoScript::P2tr => {
                    let sighash = cache
                        .taproot_key_spend_signature_hash(
                            index,
                            &Prevouts::All(prevouts),
                            TapSighashType::Default,
                        )
                        .map_err(|_| UtxoError::signing("the bip-341 sighash failed"))?;
                    let auxiliary = random_bytes::<32>()
                        .map_err(|_| UtxoError::signing("secure random bytes are unavailable"))?;
                    let mut tweaked = keypair.tap_tweak(verification_context(), None).to_keypair();
                    let signature = signing_context().sign_schnorr_with_aux_rand(
                        &Message::from_digest(sighash.to_byte_array()),
                        &tweaked,
                        &auxiliary,
                    );
                    tweaked.non_secure_erase();
                    input.witness = Witness::p2tr_key_spend(&taproot::Signature {
                        signature,
                        sighash_type: TapSighashType::Default,
                    });
                    input.script_sig = ScriptBuf::new();
                }
                UtxoScript::P2wpkh => {
                    let signature =
                        segwit_v0(&mut cache, index, own_script_pubkey, value, keypair)?;
                    input.witness = Witness::p2wpkh(&signature, &public.inner);
                    input.script_sig = ScriptBuf::new();
                }
                UtxoScript::P2shP2wpkh => {
                    // BIP-49 nests P2WPKH in P2SH, so the sighash uses the
                    // `OP_0 <keyhash>` redeem script rather than the P2SH output.
                    let redeem = ScriptBuf::new_p2wpkh(&compressed.wpubkey_hash());
                    let signature =
                        segwit_v0(&mut cache, index, redeem.as_script(), value, keypair)?;
                    input.witness = Witness::p2wpkh(&signature, &public.inner);
                    let push: &PushBytes = redeem
                        .as_bytes()
                        .try_into()
                        .map_err(|_| UtxoError::signing("the redeem script is unpushable"))?;
                    input.script_sig = ScriptBuf::builder().push_slice(push).into_script();
                }
                UtxoScript::P2pkh => {
                    let (digest, sighash_type) = match fork_id {
                        None => {
                            let sighash = cache
                                .legacy_signature_hash(index, own_script_pubkey, SIGHASH_ALL)
                                .map_err(|_| UtxoError::signing("the legacy sighash failed"))?;
                            (sighash.to_byte_array(), SIGHASH_ALL)
                        }
                        Some(fork_id) => {
                            let sighash_type = fork_id_sighash_type(fork_id)?;
                            let digest =
                                fork_id_digest(tx, index, own_script_pubkey, value, sighash_type)?;
                            (digest, sighash_type)
                        }
                    };
                    // A script pushes the DER signature with the low byte of its sighash
                    // type, whatever the digest committed to.
                    let mut secret = keypair.secret_key();
                    let signed =
                        signing_context().sign_ecdsa(&Message::from_digest(digest), &secret);
                    secret.non_secure_erase();
                    let mut encoded = signed.serialize_der().to_vec();
                    encoded.extend(sighash_type.to_le_bytes().first());
                    let signature = PushBytesBuf::try_from(encoded)
                        .map_err(|_| UtxoError::signing("the signature is unpushable"))?;
                    input.script_sig =
                        ScriptBuf::builder().push_slice(signature).push_key(&public).into_script();
                    input.witness = Witness::new();
                }
            }
        }
        Ok(())
    }
}

/// `SIGHASH_ALL | SIGHASH_FORKID` with the fork id in the upper 24 bits.
fn fork_id_sighash_type(fork_id: u32) -> Result<u32, UtxoError> {
    fork_id
        .checked_shl(8)
        .filter(|shifted| shifted >> 8 == fork_id)
        .map(|shifted| shifted | SIGHASH_FORKID | SIGHASH_ALL)
        .ok_or(UtxoError::signing("the fork id does not fit the sighash type"))
}

/// Bitcoin Cash's replay-protected digest, the BIP-143 algorithm over a legacy
/// input with `sighash_type` committed whole, which is what makes a Bitcoin node
/// refuse the signature.
///
/// Only `SIGHASH_ALL` is built, so every input, sequence, and output is hashed.
/// The spent output carries no token prefix, because a coin with tokens is never
/// spent.
fn fork_id_digest(
    tx: &Transaction,
    index: usize,
    script_code: &Script,
    value: bitcoin::Amount,
    sighash_type: u32,
) -> Result<[u8; 32], UtxoError> {
    let txin = tx.input.get(index).ok_or(UtxoError::signing("an input index is out of range"))?;
    let encode = || -> Result<[u8; 32], io::Error> {
        let mut prevouts = sha256d::Hash::engine();
        let mut sequences = sha256d::Hash::engine();
        for input in &tx.input {
            input.previous_output.consensus_encode(&mut prevouts)?;
            input.sequence.consensus_encode(&mut sequences)?;
        }
        let mut outputs = sha256d::Hash::engine();
        for output in &tx.output {
            output.consensus_encode(&mut outputs)?;
        }
        let mut preimage = sha256d::Hash::engine();
        tx.version.consensus_encode(&mut preimage)?;
        preimage.input(sha256d::Hash::from_engine(prevouts).as_byte_array());
        preimage.input(sha256d::Hash::from_engine(sequences).as_byte_array());
        txin.previous_output.consensus_encode(&mut preimage)?;
        script_code.consensus_encode(&mut preimage)?;
        value.consensus_encode(&mut preimage)?;
        txin.sequence.consensus_encode(&mut preimage)?;
        preimage.input(sha256d::Hash::from_engine(outputs).as_byte_array());
        tx.lock_time.consensus_encode(&mut preimage)?;
        sighash_type.consensus_encode(&mut preimage)?;
        Ok(sha256d::Hash::from_engine(preimage).to_byte_array())
    };
    encode().map_err(|_| UtxoError::signing("the fork id sighash failed"))
}

/// One BIP-143 signature over `script_code`, with the `SIGHASH_ALL` byte attached.
/// `secp256k1` signs with RFC-6979 nonces and normalizes to low S, so it is
/// deterministic.
fn segwit_v0(
    cache: &mut SighashCache<&Transaction>,
    index: usize,
    script_code: &Script,
    value: bitcoin::Amount,
    keypair: &Keypair,
) -> Result<ecdsa::Signature, UtxoError> {
    let sighash = cache
        .p2wpkh_signature_hash(index, script_code, value, EcdsaSighashType::All)
        .map_err(|_| UtxoError::signing("the bip-143 sighash failed"))?;
    let mut secret = keypair.secret_key();
    let signature =
        signing_context().sign_ecdsa(&Message::from_digest(sighash.to_byte_array()), &secret);
    secret.non_secure_erase();
    Ok(ecdsa::Signature::sighash_all(signature))
}

#[cfg(test)]
mod tests {
    use bitcoin::Amount as BtcAmount;
    use bitcoin::consensus::deserialize;
    use bitcoin::secp256k1::{XOnlyPublicKey, ecdsa as ecdsa_sig, schnorr};
    use jelly_core::ScriptKind;
    use jelly_core::test_util::key_material;
    use jelly_keypair::Secp256k1Key;
    use serde::Deserialize;

    use super::*;
    use crate::coin::{Utxo, units};
    use crate::fixtures::{Fallible, ONE_PER_VB, bitcoin_scene, confirmed, litecoin_scene, scene};
    use crate::payload::source_key;
    use crate::test_util::{BITCOIN, TestKey, test_key};

    fn sign_one(kind: ScriptKind) -> Fallible<Signed> {
        let scene = scene(&BITCOIN, kind)?;
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000]))?;
        Ok(sign_tx(&scene.params, &built.payload, &[scene.ours.material()])?)
    }

    fn transaction(signed: &Signed) -> Fallible<Transaction> {
        Ok(deserialize(signed.raw())?)
    }

    #[test]
    fn each_script_kind_signs_into_its_own_slot() {
        let taproot = transaction(&sign_one(ScriptKind::P2tr).unwrap()).unwrap();
        let input = taproot.input.first().expect("an input");
        assert_eq!(input.witness.len(), 1, "one 64-byte schnorr signature");
        assert_eq!(input.witness.iter().next().map(<[u8]>::len), Some(64));
        assert!(input.script_sig.is_empty());

        let native = transaction(&sign_one(ScriptKind::P2wpkh).unwrap()).unwrap();
        let input = native.input.first().expect("an input");
        assert_eq!(input.witness.len(), 2, "a signature and a public key");
        assert!(input.script_sig.is_empty());

        let wrapped = transaction(&sign_one(ScriptKind::P2shP2wpkh).unwrap()).unwrap();
        let input = wrapped.input.first().expect("an input");
        assert_eq!(input.witness.len(), 2);
        assert_eq!(input.script_sig.len(), 23, "a single push of the redeem script");

        let legacy = transaction(&sign_one(ScriptKind::P2pkh).unwrap()).unwrap();
        let input = legacy.input.first().expect("an input");
        assert!(input.witness.is_empty());
        assert!(!input.script_sig.is_empty());
    }

    #[test]
    fn only_a_spend_with_an_empty_script_signature_keeps_its_identity() {
        for kind in UtxoScript::ALL.map(ScriptKind::from) {
            let scene = scene(&BITCOIN, kind).unwrap();
            let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
            let unsigned = built.payload.unsigned_txid();
            let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
            let is_stable = matches!(kind, ScriptKind::P2tr | ScriptKind::P2wpkh);
            assert_eq!(signed.txid() == unsigned, is_stable, "{kind:?}");
        }
    }

    /// The encoder emits the BIP-144 marker only when an input carries a witness,
    /// because nodes reject a witness serialization with no witness data.
    #[test]
    fn a_witnessless_transaction_goes_out_in_the_legacy_form() {
        // The version is followed directly by the input count, with no 0x00 0x01
        // marker pair.
        let legacy = sign_one(ScriptKind::P2pkh).unwrap();
        assert_ne!(legacy.raw().get(4..6), Some(&[0x00u8, 0x01][..]));
        assert_eq!(legacy.raw().get(4..5), Some(&[0x01u8][..]), "one input, unmarked");

        let segwit = sign_one(ScriptKind::P2wpkh).unwrap();
        assert_eq!(segwit.raw().get(4..6), Some(&[0x00u8, 0x01][..]));
    }

    #[test]
    fn ecdsa_signing_is_deterministic() {
        for kind in [ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh, ScriptKind::P2pkh] {
            let scene = scene(&BITCOIN, kind).unwrap();
            let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
            let first = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
            let second = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
            assert_eq!(first.raw(), second.raw(), "{kind:?}");
            assert_eq!(first.txid(), second.txid(), "{kind:?}");
        }
    }

    #[test]
    fn taproot_signatures_use_fresh_randomness_without_changing_the_txid() {
        let scene = scene(&BITCOIN, ScriptKind::P2tr).unwrap();
        let built =
            scene.build(Some(600_000), ONE_PER_VB, &scene.coins(&[500_000, 300_000])).unwrap();
        let first = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
        let second = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
        assert_ne!(first.raw(), second.raw());
        assert_eq!(first.txid(), second.txid());
        assert_eq!(first.txid(), built.payload.unsigned_txid());

        let prevouts = built.payload.prevouts().unwrap();
        let program = scene.ours.script().script_pubkey().as_bytes().get(2..34).unwrap();
        let output_key = XOnlyPublicKey::from_slice(program).unwrap();
        let first_tx = transaction(&first).unwrap();
        let second_tx = transaction(&second).unwrap();
        assert_eq!(first_tx.input.len(), 2);
        let mut cache = SighashCache::new(built.payload.unsigned_tx());
        for (index, (first_input, second_input)) in
            first_tx.input.iter().zip(&second_tx.input).enumerate()
        {
            assert_ne!(first_input.witness, second_input.witness);
            let sighash = cache
                .taproot_key_spend_signature_hash(
                    index,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .unwrap();
            for input in [first_input, second_input] {
                let signature =
                    schnorr::Signature::from_slice(input.witness.iter().next().unwrap()).unwrap();
                verification_context()
                    .verify_schnorr(
                        &signature,
                        &Message::from_digest(sighash.to_byte_array()),
                        &output_key,
                    )
                    .unwrap();
            }
        }
    }

    /// The signature verifies over the BIP-143 digest with the address's own key.
    #[test]
    fn a_segwit_v0_signature_verifies_against_the_address_key() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
        let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();

        let prevouts = built.payload.prevouts().unwrap();
        let value = prevouts.first().expect("a prevout").value;
        let mut cache = SighashCache::new(built.payload.unsigned_tx());
        let sighash = cache
            .p2wpkh_signature_hash(
                0,
                scene.ours.script().script_pubkey(),
                value,
                EcdsaSighashType::All,
            )
            .unwrap();

        let tx = transaction(&signed).unwrap();
        let input = tx.input.first().expect("an input");
        let items: Vec<&[u8]> = input.witness.iter().collect();
        let signature = ecdsa::Signature::from_slice(items.first().expect("a signature")).unwrap();
        let key = Secp256k1Key::from_secret_bytes(scene.ours.secret()).unwrap();
        verification_context()
            .verify_ecdsa(
                &Message::from_digest(sighash.to_byte_array()),
                &signature.signature,
                &PublicKey::from_slice(&key.public_compressed()).unwrap().inner,
            )
            .unwrap();
        assert_eq!(
            items.get(1).map(|bytes| bytes.to_vec()),
            Some(key.public_compressed().to_vec())
        );
    }

    /// The taproot spend signs with the tweaked key, whose BIP-341 negations are easy
    /// to get wrong.
    #[test]
    fn a_taproot_signature_verifies_against_the_output_key() {
        let scene = scene(&BITCOIN, ScriptKind::P2tr).unwrap();
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
        let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();

        let prevouts = built.payload.prevouts().unwrap();
        let mut cache = SighashCache::new(built.payload.unsigned_tx());
        let sighash = cache
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&prevouts), TapSighashType::Default)
            .unwrap();

        let tx = transaction(&signed).unwrap();
        let input = tx.input.first().expect("an input");
        let raw = input.witness.iter().next().expect("a signature");
        let signature = schnorr::Signature::from_slice(raw).unwrap();
        // The output key is the 32 bytes the script pays.
        let program = scene.ours.script().script_pubkey().as_bytes().get(2..34).expect("a program");
        let output_key = XOnlyPublicKey::from_slice(program).unwrap();
        verification_context()
            .verify_schnorr(&signature, &Message::from_digest(sighash.to_byte_array()), &output_key)
            .unwrap();
    }

    #[test]
    fn a_key_that_does_not_own_the_address_is_refused_before_anything_is_hashed() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
        assert!(matches!(
            sign_tx(&scene.params, &built.payload, &[scene.theirs.material()]),
            Err(UtxoError::WrongKey)
        ));
    }

    #[test]
    fn a_key_of_the_wrong_curve_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
        let ed25519 = key_material(KeyKind::Ed25519Seed, 7);
        assert!(matches!(
            sign_tx(&scene.params, &built.payload, &[ed25519]),
            Err(UtxoError::Signing { .. })
        ));
    }

    /// The declared source becomes another address the key owns, so the ownership
    /// check would pass and the input would be left for no one to sign.
    #[test]
    fn an_input_no_source_owns_is_refused_rather_than_left_unsigned() {
        let scene = bitcoin_scene().unwrap();
        let elsewhere = test_key(&BITCOIN, ScriptKind::P2wpkh, 9).unwrap();
        let built = scene.build(Some(100_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();

        let mut moved = built.payload.psbt().clone();
        moved.proprietary.insert(source_key(0), elsewhere.address().as_bytes().to_vec());
        let payload = UtxoPayload::decode(&moved.serialize()).unwrap();
        assert!(matches!(
            sign_tx(&scene.params, &payload, &[elsewhere.material()]),
            Err(UtxoError::Payload { .. })
        ));
    }

    /// Our own fixture keys, one of each kind, and coins of each worth `value`.
    fn mixed(kinds: &[ScriptKind], value: u64) -> Fallible<(Vec<TestKey>, Vec<Utxo>)> {
        let mut keys = Vec::new();
        let mut coins = Vec::new();
        for (seed, kind) in (1u8..).zip(kinds) {
            let key = test_key(&BITCOIN, *kind, seed)?;
            coins.push(confirmed(&key, seed, value));
            keys.push(key);
        }
        Ok((keys, coins))
    }

    /// One transaction spends a legacy, a wrapped, a native, and a taproot coin of
    /// four addresses.
    ///
    /// Each is signed by its own key in its own slot, and every signature verifies
    /// against the digest its kind commits to.
    #[test]
    fn every_input_of_a_mixed_transaction_is_signed_by_its_own_address() {
        let scene = bitcoin_scene().unwrap();
        let kinds =
            [ScriptKind::P2pkh, ScriptKind::P2shP2wpkh, ScriptKind::P2wpkh, ScriptKind::P2tr];
        let (keys, coins) = mixed(&kinds, 100_000).unwrap();
        let sources: Vec<&TestKey> = keys.iter().collect();
        let built = scene.build_from(&sources, None, ONE_PER_VB, &coins).unwrap();
        assert_eq!(built.payload.inputs().len(), 4);
        assert_eq!(
            built.payload.intent().sources,
            keys.iter().map(|key| key.address().to_owned()).collect::<Vec<_>>()
        );
        let mut predicted: Vec<ScriptKind> =
            built.payload.inputs().iter().map(|input| input.script.kind().into()).collect();
        predicted.sort_by_key(|kind| kind.as_str());
        let mut expected = kinds.to_vec();
        expected.sort_by_key(|kind| kind.as_str());
        assert_eq!(predicted, expected);

        let materials: Vec<KeyMaterial> = keys.iter().map(TestKey::material).collect();
        let signed = sign_tx(&scene.params, &built.payload, &materials).unwrap();
        let tx = transaction(&signed).unwrap();
        assert_eq!(signed.txid(), tx.compute_txid());
        let prevouts = built.payload.prevouts().unwrap();
        let unsigned = built.payload.unsigned_tx();
        for (index, input) in built.payload.inputs().iter().enumerate() {
            let key = keys.iter().find(|key| key.script() == &input.script).unwrap();
            let public = PublicKey::from_slice(key.pubkey()).unwrap();
            let txin = &tx.input[index];
            let mut cache = SighashCache::new(unsigned);
            match input.script.kind() {
                UtxoScript::P2tr => {
                    let digest = cache
                        .taproot_key_spend_signature_hash(
                            index,
                            &Prevouts::All(&prevouts),
                            TapSighashType::Default,
                        )
                        .unwrap();
                    let program = input.script.script_pubkey().as_bytes().get(2..34).unwrap();
                    let signature =
                        schnorr::Signature::from_slice(txin.witness.iter().next().unwrap())
                            .unwrap();
                    verification_context()
                        .verify_schnorr(
                            &signature,
                            &Message::from_digest(digest.to_byte_array()),
                            &XOnlyPublicKey::from_slice(program).unwrap(),
                        )
                        .unwrap();
                }
                UtxoScript::P2wpkh | UtxoScript::P2shP2wpkh => {
                    let code =
                        ScriptBuf::new_p2wpkh(&CompressedPublicKey(public.inner).wpubkey_hash());
                    let digest = cache
                        .p2wpkh_signature_hash(
                            index,
                            &code,
                            prevouts[index].value,
                            EcdsaSighashType::All,
                        )
                        .unwrap();
                    let items: Vec<&[u8]> = txin.witness.iter().collect();
                    let signature = ecdsa::Signature::from_slice(items[0]).unwrap();
                    verification_context()
                        .verify_ecdsa(
                            &Message::from_digest(digest.to_byte_array()),
                            &signature.signature,
                            &public.inner,
                        )
                        .unwrap();
                    assert_eq!(items[1], key.pubkey());
                }
                UtxoScript::P2pkh => {
                    let digest = cache
                        .legacy_signature_hash(index, input.script.script_pubkey(), SIGHASH_ALL)
                        .unwrap();
                    let (signature, sighash_byte, pubkey) =
                        p2pkh_parts(&Transaction { input: vec![txin.clone()], ..tx.clone() });
                    assert_eq!(u32::from(sighash_byte), SIGHASH_ALL);
                    assert_eq!(pubkey, public);
                    verification_context()
                        .verify_ecdsa(
                            &Message::from_digest(digest.to_byte_array()),
                            &signature,
                            &public.inner,
                        )
                        .unwrap();
                }
            }
        }
    }

    /// The keys arrive in the sources' order, so a swapped pair, a missing key, or
    /// one key too many is refused before anything is signed.
    #[test]
    fn the_keys_must_match_the_sources_one_for_one() {
        let scene = bitcoin_scene().unwrap();
        let (keys, coins) = mixed(&[ScriptKind::P2wpkh, ScriptKind::P2tr], 100_000).unwrap();
        let sources: Vec<&TestKey> = keys.iter().collect();
        let built = scene.build_from(&sources, None, ONE_PER_VB, &coins).unwrap();
        let swapped = [keys[1].material(), keys[0].material()];
        let missing = [keys[0].material()];
        let extra = [keys[0].material(), keys[1].material(), scene.theirs.material()];
        for wrong in [&swapped[..], &missing[..], &extra[..]] {
            assert!(matches!(
                sign_tx(&scene.params, &built.payload, wrong),
                Err(UtxoError::WrongKey)
            ));
        }
    }

    /// Change goes to the first source even when none of its coins was needed, and
    /// its key then has nothing to sign, which is not an error.
    #[test]
    fn a_first_source_that_only_receives_change_still_signs() {
        let scene = bitcoin_scene().unwrap();
        let first = test_key(&BITCOIN, ScriptKind::P2wpkh, 3).unwrap();
        let second = test_key(&BITCOIN, ScriptKind::P2pkh, 4).unwrap();
        let coins = vec![confirmed(&second, 4, 900_000)];
        let built =
            scene.build_from(&[&first, &second], Some(100_000), ONE_PER_VB, &coins).unwrap();
        let change = built.selection.change.expect("change");
        let outputs = &built.payload.unsigned_tx().output;
        assert_eq!(outputs[1].script_pubkey, first.script_pubkey());
        assert_eq!(units(outputs[1].value.to_sat()), change);
        assert_eq!(
            built.payload.intent().sources,
            [first.address().to_owned(), second.address().to_owned()]
        );
        sign_tx(&scene.params, &built.payload, &[first.material(), second.material()]).unwrap();

        let exact = built.selection.recipient_value.checked_add(built.selection.fee).unwrap();
        let spent = Utxo::total(&coins).unwrap();
        assert_eq!(exact.checked_add(change), Some(spent));
    }

    #[test]
    fn litecoin_signs_the_same_way_bitcoin_does() {
        let ltc = litecoin_scene().unwrap();
        let built = ltc.build(Some(100_000), ONE_PER_VB, &ltc.coins(&[500_000])).unwrap();
        let signed = sign_tx(&ltc.params, &built.payload, &[ltc.ours.material()]).unwrap();
        assert_eq!(signed.txid(), built.payload.unsigned_txid());
        let tx = transaction(&signed).unwrap();
        assert_eq!(tx.input.first().map(|input| input.witness.len()), Some(2));

        // A Bitcoin key derived from the same secret controls a different address, so
        // the codec is what the ownership check actually depends on.
        let btc = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        assert_ne!(btc.address(), ltc.ours.address());
        assert_eq!(btc.secret(), ltc.ours.secret());
    }

    /// The script signature of a one-input P2PKH spend, as the signature without
    /// its sighash byte, that byte, and the public key.
    fn p2pkh_parts(tx: &Transaction) -> (ecdsa_sig::Signature, u8, PublicKey) {
        let script_sig = &tx.input.first().expect("an input").script_sig;
        let pushes: Vec<Vec<u8>> = script_sig
            .instructions()
            .map(|instruction| {
                instruction.unwrap().push_bytes().expect("a push").as_bytes().to_vec()
            })
            .collect();
        let [with_type, key] = pushes.as_slice() else { panic!("{pushes:?}") };
        let (&sighash_byte, der) = with_type.split_last().expect("a signature");
        (
            ecdsa_sig::Signature::from_der(der).unwrap(),
            sighash_byte,
            PublicKey::from_slice(key).unwrap(),
        )
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Spend {
        txid: String,
        tx: String,
        input: usize,
        value: u64,
        script_pubkey: String,
    }

    impl Spend {
        fn parts(&self) -> (Transaction, ScriptBuf, BtcAmount) {
            let tx: Transaction = deserialize(&hex::decode(&self.tx).unwrap()).unwrap();
            assert_eq!(tx.compute_txid().to_string(), self.txid);
            assert_eq!(self.input, 0);
            let script = ScriptBuf::from(hex::decode(&self.script_pubkey).unwrap());
            (tx, script, BtcAmount::from_sat(self.value))
        }
    }

    mod fork_id {
        use super::*;
        use crate::fixtures::bitcoin_cash_scene;

        const VECTORS: &str = include_str!("../tests/vectors/bitcoin_cash/sighash.json");

        #[derive(Deserialize)]
        struct File {
            digests: Vec<Vector>,
            mainnet: Spend,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Vector {
            tx: String,
            script_code: String,
            input: usize,
            sighash_type: u32,
            digest: String,
        }

        fn file() -> File {
            serde_json::from_str(VECTORS).unwrap()
        }

        /// Bitcoin Cash Node's own vectors, which commit the whole sighash type,
        /// fork id bits included.
        #[test]
        fn the_digest_matches_bitcoin_cash_nodes_vectors() {
            let cases = file().digests;
            assert_eq!(cases.len(), 11);
            for case in &cases {
                let tx: Transaction = deserialize(&hex::decode(&case.tx).unwrap()).unwrap();
                let script_code = ScriptBuf::from(hex::decode(&case.script_code).unwrap());
                let mut digest = fork_id_digest(
                    &tx,
                    case.input,
                    &script_code,
                    BtcAmount::ZERO,
                    case.sighash_type,
                )
                .unwrap();
                digest.reverse();
                assert_eq!(hex::encode(digest), case.digest, "{}", case.sighash_type);
            }
        }

        /// A spend the network accepted verifies against this digest under its own
        /// key, so the digest is the one Bitcoin Cash nodes check.
        #[test]
        fn a_mainnet_signature_verifies_against_the_digest() {
            let (tx, script, value) = file().mainnet.parts();
            let (signature, sighash_byte, key) = p2pkh_parts(&tx);
            assert_eq!(sighash_byte, 0x41);
            let digest =
                fork_id_digest(&tx, 0, &script, value, fork_id_sighash_type(0).unwrap()).unwrap();
            verification_context()
                .verify_ecdsa(&Message::from_digest(digest), &signature, &key.inner)
                .unwrap();
        }

        /// Every Bitcoin Cash signature carries `SIGHASH_FORKID`, and none of them
        /// verifies against the digest Bitcoin checks, which is the replay
        /// protection.
        #[test]
        fn a_bitcoin_cash_signature_is_replay_protected() {
            let scene = bitcoin_cash_scene().unwrap();
            let coins = scene.coins(&[500_000, 300_000]);
            let built = scene.build(Some(600_000), ONE_PER_VB, &coins).unwrap();
            let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
            let tx = transaction(&signed).unwrap();
            assert_eq!(tx.input.len(), 2);
            let prevouts = built.payload.prevouts().unwrap();
            let script = scene.ours.script().script_pubkey();
            let unsigned = built.payload.unsigned_tx();
            for (index, (input, prevout)) in tx.input.iter().zip(&prevouts).enumerate() {
                let (signature, sighash_byte, key) =
                    p2pkh_parts(&Transaction { input: vec![input.clone()], ..tx.clone() });
                assert_eq!(sighash_byte, 0x41, "SIGHASH_ALL with SIGHASH_FORKID");
                assert!(input.witness.is_empty());
                let digest = fork_id_digest(unsigned, index, script, prevout.value, 0x41).unwrap();
                verification_context()
                    .verify_ecdsa(&Message::from_digest(digest), &signature, &key.inner)
                    .unwrap();
                let legacy = SighashCache::new(unsigned)
                    .legacy_signature_hash(index, script, SIGHASH_ALL)
                    .unwrap();
                assert!(
                    verification_context()
                        .verify_ecdsa(
                            &Message::from_digest(legacy.to_byte_array()),
                            &signature,
                            &key.inner
                        )
                        .is_err()
                );
            }
            assert_eq!(signed.txid(), tx.compute_txid());
            assert_ne!(signed.txid(), built.payload.unsigned_txid(), "a legacy spend's id moves");
        }

        #[test]
        fn a_fork_id_that_overflows_the_sighash_type_is_refused() {
            assert_eq!(fork_id_sighash_type(0).unwrap(), 0x41);
            assert_eq!(fork_id_sighash_type(0x00ff_ffff).unwrap(), 0xffff_ff41);
            fork_id_sighash_type(0x0100_0000).unwrap_err();
        }
    }

    mod dogecoin {
        use super::*;
        use crate::test_util::DOGECOIN;

        const VECTORS: &str = include_str!("../tests/vectors/dogecoin/sighash.json");

        #[derive(Deserialize)]
        struct File {
            mainnet: Spend,
        }

        /// Dogecoin kept Bitcoin's legacy sighash, so a spend the network accepted
        /// verifies against it.
        #[test]
        fn a_mainnet_signature_verifies_against_the_legacy_digest() {
            let file: File = serde_json::from_str(VECTORS).unwrap();
            let (tx, script, _) = file.mainnet.parts();
            let (signature, sighash_byte, key) = p2pkh_parts(&tx);
            assert_eq!(u32::from(sighash_byte), SIGHASH_ALL);
            let digest =
                SighashCache::new(&tx).legacy_signature_hash(0, &script, SIGHASH_ALL).unwrap();
            verification_context()
                .verify_ecdsa(&Message::from_digest(digest.to_byte_array()), &signature, &key.inner)
                .unwrap();
        }

        #[test]
        fn a_dogecoin_spend_signs_with_the_legacy_digest() {
            let scene = scene(&DOGECOIN, ScriptKind::P2pkh).unwrap();
            let built = scene
                .build(Some(5_000_000), DOGECOIN.min_tx_fee, &scene.coins(&[90_000_000]))
                .unwrap();
            let signed = sign_tx(&scene.params, &built.payload, &[scene.ours.material()]).unwrap();
            let tx = transaction(&signed).unwrap();
            let (signature, sighash_byte, key) = p2pkh_parts(&tx);
            assert_eq!(u32::from(sighash_byte), SIGHASH_ALL);
            let script = scene.ours.script().script_pubkey();
            let digest = SighashCache::new(built.payload.unsigned_tx())
                .legacy_signature_hash(0, script, SIGHASH_ALL)
                .unwrap();
            verification_context()
                .verify_ecdsa(&Message::from_digest(digest.to_byte_array()), &signature, &key.inner)
                .unwrap();
        }
    }

    mod bip143 {
        use super::*;

        const BIP143: &str = include_str!("../tests/vectors/bip143.json");

        #[derive(Deserialize)]
        struct File {
            cases: Vec<Case>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Case {
            name: String,
            script_kind: String,
            unsigned_tx: String,
            input_index: usize,
            script_code: String,
            value: u64,
            private_key: String,
            public_key: String,
            sighash: String,
            signature: String,
        }

        /// The two published examples, signed by the path a payload takes. The
        /// native case signs over the script it spends, and the wrapped case over
        /// the redeem script the signer derives from the key.
        #[test]
        fn the_published_segwit_v0_examples_sign_the_same_way() {
            let file: File = serde_json::from_str(BIP143).unwrap();
            assert_eq!(file.cases.len(), 2);
            for case in &file.cases {
                let secret =
                    SecretKey::from_slice(&hex::decode(&case.private_key).unwrap()).unwrap();
                let keypair = Keypair::from_secret_key(signing_context(), &secret);
                let kind = UtxoScript::of(ScriptKind::parse(&case.script_kind).unwrap()).unwrap();
                let tx: Transaction =
                    deserialize(&hex::decode(&case.unsigned_tx).unwrap()).unwrap();
                let script_code = ScriptBuf::from(hex::decode(&case.script_code).unwrap());
                let sighash = SighashCache::new(&tx)
                    .p2wpkh_signature_hash(
                        case.input_index,
                        &script_code,
                        BtcAmount::from_sat(case.value),
                        EcdsaSighashType::All,
                    )
                    .unwrap();
                assert_eq!(hex::encode(sighash.to_byte_array()), case.sighash, "{}", case.name);
                let prevouts: Vec<TxOut> = (0..tx.input.len())
                    .map(|index| TxOut {
                        value: BtcAmount::from_sat(if index == case.input_index {
                            case.value
                        } else {
                            0
                        }),
                        script_pubkey: script_code.clone(),
                    })
                    .collect();

                let mut signing = SigningTx::new(&tx, &prevouts);
                signing.fill(kind, None, &[case.input_index], &script_code, &keypair).unwrap();
                let items: Vec<String> = signing
                    .signed
                    .input
                    .get(case.input_index)
                    .map(|input| input.witness.iter().map(hex::encode).collect())
                    .unwrap_or_default();
                assert_eq!(
                    items,
                    vec![format!("{}01", case.signature), case.public_key.clone()],
                    "{}",
                    case.name
                );
            }
        }
    }

    mod bip341 {
        use super::*;

        const BIP341: &str = include_str!("../tests/vectors/bip341.json");

        #[derive(Deserialize)]
        struct File {
            cases: Vec<Case>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Case {
            name: String,
            private_key: String,
            internal_key: String,
            script_pubkey: String,
            unsigned_tx: String,
            input_index: usize,
            prevouts: Vec<Prevout>,
            signature: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Prevout {
            value: u64,
            script_pubkey: String,
        }

        fn prevouts(case: &Case) -> Fallible<Vec<TxOut>> {
            case.prevouts
                .iter()
                .map(|prevout| {
                    Ok(TxOut {
                        value: BtcAmount::from_sat(prevout.value),
                        script_pubkey: ScriptBuf::from(hex::decode(&prevout.script_pubkey)?),
                    })
                })
                .collect()
        }

        #[test]
        fn the_key_path_signature_and_vector_verify_against_the_same_digest() {
            let file: File = serde_json::from_str(BIP341).unwrap();
            for case in &file.cases {
                let secret =
                    SecretKey::from_slice(&hex::decode(&case.private_key).unwrap()).unwrap();
                let keypair = Keypair::from_secret_key(signing_context(), &secret);
                let (xonly, _) = XOnlyPublicKey::from_keypair(&keypair);
                assert_eq!(hex::encode(xonly.serialize()), case.internal_key, "{}", case.name);

                let tx: Transaction =
                    deserialize(&hex::decode(&case.unsigned_tx).unwrap()).unwrap();
                let prevouts = prevouts(case).unwrap();
                let script_pubkey = ScriptBuf::from(hex::decode(&case.script_pubkey).unwrap());
                let mut signing = SigningTx::new(&tx, &prevouts);
                signing
                    .fill(UtxoScript::P2tr, None, &[case.input_index], &script_pubkey, &keypair)
                    .unwrap();
                let witness =
                    &signing.signed.input.get(case.input_index).expect("a signed input").witness;
                assert_eq!(witness.len(), 1, "{}", case.name);
                let signature =
                    schnorr::Signature::from_slice(witness.iter().next().unwrap()).unwrap();
                let vector =
                    schnorr::Signature::from_slice(&hex::decode(&case.signature).unwrap()).unwrap();
                let output_key =
                    XOnlyPublicKey::from_slice(script_pubkey.as_bytes().get(2..34).unwrap())
                        .unwrap();
                let sighash = SighashCache::new(&tx)
                    .taproot_key_spend_signature_hash(
                        case.input_index,
                        &Prevouts::All(&prevouts),
                        TapSighashType::Default,
                    )
                    .unwrap();
                for signature in [signature, vector] {
                    verification_context()
                        .verify_schnorr(
                            &signature,
                            &Message::from_digest(sighash.to_byte_array()),
                            &output_key,
                        )
                        .unwrap();
                }
            }
        }
    }
}
