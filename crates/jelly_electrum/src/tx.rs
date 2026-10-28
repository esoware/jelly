use bitcoin::absolute::LockTime;
use bitcoin::consensus::{Decodable as _, deserialize};
use bitcoin::transaction::Version;
use bitcoin::{Transaction, TxIn, TxOut, Witness};

const SEGWIT_MARKER: u8 = 0x00;
const WITNESS_FLAG: u8 = 0x01;
const MWEB_FLAG: u8 = 0x08;
const EMPTY_MWEB_BODY: u8 = 0x00;

/// A transaction from `blockchain.transaction.get`, plus whether it came in Litecoin's
/// `HogEx` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedTx {
    pub tx: Transaction,
    /// Litecoin's MWEB integrating transaction. Every output after the first is a
    /// peg-out, which Litecoin Core won't let anyone spend until it's
    /// `PEGOUT_MATURITY` blocks deep.
    pub is_hogex: bool,
}

/// A transaction in its canonical serialization, or in Litecoin's MWEB one with
/// an empty MWEB body.
///
/// Unlike merge-mined headers, this needs no chain flag: no other chain sets flag bit
/// 0x08, and the caller's txid check still binds whatever bytes decode.
pub(crate) fn tx_from_bytes(bytes: &[u8]) -> Option<ServedTx> {
    if let Ok(tx) = deserialize::<Transaction>(bytes) {
        return Some(ServedTx { tx, is_hogex: false });
    }
    decode_hogex(bytes).map(|tx| ServedTx { tx, is_hogex: true })
}

/// Decodes Litecoin's MWEB serialization of a transaction with no MWEB body.
///
/// Litecoin marks an MWEB transaction with bit 0x08 of the segwit flag and puts
/// an optional MWEB body, one zero byte when absent, between the witnesses and
/// the lock time. Litecoin Core reads that flag with an absent body as a `HogEx`, and
/// serves every `HogEx` this way at its default `-rpcserialversion`. The txid covers
/// neither the bit nor the body. A present body is refused, since skipping it would
/// take the whole MWEB format.
fn decode_hogex(bytes: &[u8]) -> Option<Transaction> {
    let mut reader = bytes;
    let version = Version::consensus_decode_from_finite_reader(&mut reader).ok()?;
    let Some((&[SEGWIT_MARKER, flag], rest)) = reader.split_first_chunk::<2>() else {
        return None;
    };
    if flag & MWEB_FLAG == 0 || flag & !(MWEB_FLAG | WITNESS_FLAG) != 0 {
        return None;
    }
    reader = rest;
    let mut inputs = Vec::<TxIn>::consensus_decode_from_finite_reader(&mut reader).ok()?;
    let outputs = Vec::<TxOut>::consensus_decode_from_finite_reader(&mut reader).ok()?;
    if flag & WITNESS_FLAG != 0 {
        for txin in &mut inputs {
            txin.witness = Witness::consensus_decode_from_finite_reader(&mut reader).ok()?;
        }
    }
    let Some((&EMPTY_MWEB_BODY, rest)) = reader.split_first() else { return None };
    reader = rest;
    let lock_time = LockTime::consensus_decode_from_finite_reader(&mut reader).ok()?;
    reader.is_empty().then_some(Transaction { version, lock_time, input: inputs, output: outputs })
}
