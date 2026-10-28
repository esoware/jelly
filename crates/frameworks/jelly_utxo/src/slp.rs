//! The Simple Ledger Protocol, whose message in a transaction's first output assigns
//! tokens and mint batons to the outputs after it.
//!
//! The wallet doesn't manage tokens, so it reads a message only for the outputs it
//! names, which a plain spend would burn. Checking that the tokens are valid would take
//! the whole token graph, so like Electron Cash, the wallet holds back any output a
//! well-formed message names.

use bitcoin::Transaction;

const OP_RETURN: u8 = 0x6a;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;

const LOKAD_ID: &[u8] = b"SLP\x00";

const TOKEN_TYPE_FUNGIBLE: u64 = 0x01;
const TOKEN_TYPE_NFT_CHILD: u64 = 0x41;
const TOKEN_TYPE_NFT_GROUP: u64 = 0x81;

const TOKEN_ID_BYTES: usize = 32;
const QUANTITY_BYTES: usize = 8;
const MAX_DECIMALS: u64 = 9;
const MAX_SEND_OUTPUTS: usize = 19;
/// Outputs 0 and 1 are the message and the minted tokens, so a baton sits after them.
const FIRST_BATON_VOUT: u64 = 2;

/// Whether output `vout` of `tx` is one the SLP message in its first output assigns
/// tokens or a mint baton to.
pub(crate) fn carries_tokens(tx: &Transaction, vout: u32) -> bool {
    tx.output
        .first()
        .and_then(|first| token_vouts(first.script_pubkey.as_bytes()))
        .is_some_and(|vouts| vouts.contains(&vout))
}

/// The outputs the SLP message `script` assigns tokens or a mint baton to.
///
/// `None` means it isn't a well-formed message of token type 1, NFT1 child, or NFT1
/// group, which by the specification carries no tokens at all.
///
/// An output named with a quantity of zero carries nothing and is left out.
fn token_vouts(script: &[u8]) -> Option<Vec<u32>> {
    let chunks = pushes(script)?;
    let [lokad_id, type_field, kind, fields @ ..] = chunks.as_slice() else {
        return None;
    };
    if *lokad_id != LOKAD_ID {
        return None;
    }
    let token_type = integer(type_field, 1, 2)?;
    if !matches!(token_type, TOKEN_TYPE_FUNGIBLE | TOKEN_TYPE_NFT_CHILD | TOKEN_TYPE_NFT_GROUP) {
        return None;
    }
    let is_child = token_type == TOKEN_TYPE_NFT_CHILD;
    match *kind {
        b"GENESIS" => genesis(fields, is_child),
        b"MINT" if !is_child => mint(fields),
        b"SEND" => send(fields),
        _ => None,
    }
}

/// An NFT1 child is one indivisible token with no baton.
fn genesis(fields: &[&[u8]], is_child: bool) -> Option<Vec<u32>> {
    let [_ticker, _name, _document_url, document_hash, decimals_field, baton_field, quantity_field] =
        *fields
    else {
        return None;
    };
    if !matches!(document_hash.len(), 0 | TOKEN_ID_BYTES) {
        return None;
    }
    let decimals = integer(decimals_field, 1, 1)?;
    let baton = baton_vout(baton_field).ok()?;
    let quantity = integer(quantity_field, QUANTITY_BYTES, QUANTITY_BYTES)?;
    if decimals > MAX_DECIMALS || (is_child && (baton.is_some() || decimals != 0 || quantity != 1))
    {
        return None;
    }
    Some(minted(quantity, baton))
}

fn mint(fields: &[&[u8]]) -> Option<Vec<u32>> {
    let [token_id, baton_field, quantity_field] = *fields else {
        return None;
    };
    if token_id.len() != TOKEN_ID_BYTES {
        return None;
    }
    let baton = baton_vout(baton_field).ok()?;
    let quantity = integer(quantity_field, QUANTITY_BYTES, QUANTITY_BYTES)?;
    Some(minted(quantity, baton))
}

fn send(fields: &[&[u8]]) -> Option<Vec<u32>> {
    let [token_id, quantities @ ..] = fields else {
        return None;
    };
    if token_id.len() != TOKEN_ID_BYTES
        || quantities.is_empty()
        || quantities.len() > MAX_SEND_OUTPUTS
    {
        return None;
    }
    let mut vouts = Vec::with_capacity(quantities.len());
    for (vout, quantity) in (1u32..).zip(quantities) {
        if integer(quantity, QUANTITY_BYTES, QUANTITY_BYTES)? > 0 {
            vouts.push(vout);
        }
    }
    Some(vouts)
}

/// A field the message may not hold, which makes the whole message carry no tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Malformed;

/// GENESIS and MINT put their tokens on output 1 and the baton where the message says.
fn minted(quantity: u64, baton: Option<u32>) -> Vec<u32> {
    let mut vouts = Vec::with_capacity(2);
    if quantity > 0 {
        vouts.push(1);
    }
    vouts.extend(baton);
    vouts
}

/// An empty field means there's no baton.
fn baton_vout(field: &[u8]) -> Result<Option<u32>, Malformed> {
    if field.is_empty() {
        return Ok(None);
    }
    let vout = integer(field, 1, 1).ok_or(Malformed)?;
    if vout < FIRST_BATON_VOUT {
        return Err(Malformed);
    }
    u32::try_from(vout).map(Some).map_err(|_| Malformed)
}

/// An unsigned big-endian integer of `min_bytes` to `max_bytes` bytes.
fn integer(field: &[u8], min_bytes: usize, max_bytes: usize) -> Option<u64> {
    if field.len() < min_bytes || field.len() > max_bytes {
        return None;
    }
    let mut bytes = [0u8; 8];
    let start = bytes.len().checked_sub(field.len())?;
    bytes.get_mut(start..)?.copy_from_slice(field);
    Some(u64::from_be_bytes(bytes))
}

/// The data pushed after `OP_RETURN`.
///
/// The specification admits only the push opcodes `0x01` to `0x4e`, so `OP_0`, the
/// number opcodes, and anything else invalidate the whole message, as does a script
/// that ends inside a push.
fn pushes(script: &[u8]) -> Option<Vec<&[u8]>> {
    let (&OP_RETURN, mut rest) = script.split_first()? else {
        return None;
    };
    let mut chunks = Vec::new();
    while let Some((&opcode, after)) = rest.split_first() {
        let (length, after) = match opcode {
            0x01..=0x4b => (usize::from(opcode), after),
            OP_PUSHDATA1 => {
                let (&length, after) = after.split_first()?;
                (usize::from(length), after)
            }
            OP_PUSHDATA2 => {
                let (length, after) = after.split_first_chunk::<2>()?;
                (usize::from(u16::from_le_bytes(*length)), after)
            }
            OP_PUSHDATA4 => {
                let (length, after) = after.split_first_chunk::<4>()?;
                (usize::try_from(u32::from_le_bytes(*length)).ok()?, after)
            }
            _ => return None,
        };
        let (data, after) = after.split_at_checked(length)?;
        chunks.push(data);
        rest = after;
    }
    Some(chunks)
}

#[cfg(test)]
mod tests {
    use bitcoin::consensus::deserialize;
    use serde::Deserialize;

    use super::*;

    const SCRIPTS: &str = include_str!("../tests/vectors/bitcoin_cash/slp_scripts.json");
    const TRANSACTIONS: &str = include_str!("../tests/vectors/bitcoin_cash/slp_transactions.json");

    #[derive(Deserialize)]
    struct Scripts {
        cases: Vec<ScriptCase>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ScriptCase {
        msg: String,
        script: String,
        is_valid: bool,
    }

    #[derive(Deserialize)]
    struct Transactions {
        transactions: Vec<Mainnet>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Mainnet {
        note: String,
        txid: String,
        tx: String,
        token_vouts: Vec<u32>,
    }

    /// Every message the SLP unit test suite calls valid parses, and every one it
    /// refuses, for whatever reason, assigns no tokens.
    #[test]
    fn the_specifications_script_vectors_parse_exactly_when_they_are_valid() {
        let file: Scripts = serde_json::from_str(SCRIPTS).unwrap();
        assert_eq!(file.cases.len(), 98);
        for case in &file.cases {
            let script = hex::decode(&case.script).unwrap();
            assert_eq!(token_vouts(&script).is_some(), case.is_valid, "{}", case.msg);
        }
    }

    #[test]
    fn a_vector_names_the_outputs_its_message_assigns() {
        let file: Scripts = serde_json::from_str(SCRIPTS).unwrap();
        let vouts_of = |message: &str| {
            let case = file.cases.iter().find(|case| case.msg == message).expect(message);
            token_vouts(&hex::decode(&case.script).unwrap()).expect(message)
        };
        assert_eq!(vouts_of("OK: minimal GENESIS"), [1]);
        assert_eq!(vouts_of("OK: Genesis with mint_baton_vout=255"), [1, 255]);
        assert_eq!(vouts_of("OK: MINT with mint_baton_vout=2"), [1, 2]);
        assert_eq!(vouts_of("OK: typical 2-output SEND"), [1, 2]);
        assert_eq!(vouts_of("OK: SEND with 19 token output amounts").len(), 19);
        assert!(vouts_of("OK: all output amounts 0").is_empty());
    }

    /// Real token transactions of every type, decoded the way the sync pass decodes
    /// a coin's funding transaction.
    #[test]
    fn mainnet_token_transactions_hold_exactly_their_token_outputs() {
        let file: Transactions = serde_json::from_str(TRANSACTIONS).unwrap();
        assert_eq!(file.transactions.len(), 8);
        for case in &file.transactions {
            let tx: Transaction = deserialize(&hex::decode(&case.tx).unwrap()).unwrap();
            assert_eq!(tx.compute_txid().to_string(), case.txid, "{}", case.note);
            let held: Vec<u32> = (0..u32::try_from(tx.output.len()).unwrap())
                .filter(|vout| carries_tokens(&tx, *vout))
                .collect();
            assert_eq!(held, case.token_vouts, "{}", case.note);
        }
    }

    #[test]
    fn a_transaction_without_a_message_in_its_first_output_holds_nothing() {
        let file: Transactions = serde_json::from_str(TRANSACTIONS).unwrap();
        let send = file.transactions.iter().find(|case| case.txid.starts_with("90b5")).unwrap();
        let mut tx: Transaction = deserialize(&hex::decode(&send.tx).unwrap()).unwrap();
        tx.output.rotate_left(1);
        assert!((0..4).all(|vout| !carries_tokens(&tx, vout)));
        assert!(!carries_tokens(&Transaction { output: Vec::new(), ..tx }, 1));
    }
}
