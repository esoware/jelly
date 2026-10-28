//! The `CashTokens` prefix Bitcoin Cash puts at the start of an output's script field,
//! before the locking script.
//!
//! The wallet doesn't manage tokens, so it only steps over the prefix to find the
//! locking script.

const PREFIX_TOKEN: u8 = 0xef;

const CATEGORY_BYTES: usize = 32;

const HAS_COMMITMENT_LENGTH: u8 = 0x40;
const HAS_AMOUNT: u8 = 0x10;

/// The locking script behind a token prefix, or `None` when `script` does not open
/// with one that parses.
pub(crate) fn locking_script(script: &[u8]) -> Option<&[u8]> {
    let (&PREFIX_TOKEN, rest) = script.split_first()? else {
        return None;
    };
    let (&bitfield, mut rest) = rest.get(CATEGORY_BYTES..)?.split_first()?;
    if bitfield & HAS_COMMITMENT_LENGTH != 0 {
        let (length, after) = compact_size(rest)?;
        rest = after.get(usize::try_from(length).ok()?..)?;
    }
    if bitfield & HAS_AMOUNT != 0 {
        rest = compact_size(rest)?.1;
    }
    Some(rest)
}

/// A `CompactSize` integer and the bytes after it.
fn compact_size(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let (&first, rest) = bytes.split_first()?;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        value => return Some((u64::from(value), rest)),
    };
    let (body, rest) = rest.split_at_checked(width)?;
    let mut value = [0u8; 8];
    value.get_mut(..width)?.copy_from_slice(body);
    Some((u64::from_le_bytes(value), rest))
}

#[cfg(test)]
mod tests {
    use bitcoin::Transaction;
    use bitcoin::consensus::deserialize;
    use serde::Deserialize;

    use super::*;

    const VECTORS: &str = include_str!("../tests/vectors/bitcoin_cash/token_prefixes.json");

    #[derive(Deserialize)]
    struct File {
        valid: Vec<String>,
    }

    /// The `CashTokens` spec's valid prefix encodings, each followed by a P2PKH
    /// locking script that it must step over exactly.
    #[test]
    fn every_published_prefix_is_stepped_over_to_the_script_behind_it() {
        let file: File = serde_json::from_str(VECTORS).unwrap();
        assert!(file.valid.len() >= 20, "{}", file.valid.len());
        let script = hex::decode("76a914751e76e8199196d454941c45d1b3a323f1433bd688ac").unwrap();
        for prefix in &file.valid {
            let mut bytes = hex::decode(prefix).unwrap();
            bytes.extend_from_slice(&script);
            assert_eq!(locking_script(&bytes), Some(script.as_slice()), "{prefix}");
        }
    }

    const MAINNET: &str = include_str!("../tests/vectors/bitcoin_cash/token_transaction.json");

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Mainnet {
        tx: String,
        token_vout: usize,
        locking_script: String,
    }

    /// A token output from a mined Bitcoin Cash transaction, decoded the way a sync
    /// pass decodes transactions.
    #[test]
    fn a_mainnet_token_output_steps_over_to_its_p2pkh_script() {
        let file: Mainnet = serde_json::from_str(MAINNET).unwrap();
        let tx: Transaction = deserialize(&hex::decode(&file.tx).unwrap()).unwrap();
        let output = &tx.output[file.token_vout];
        assert_eq!(output.script_pubkey.as_bytes().first(), Some(&PREFIX_TOKEN));
        assert_eq!(
            locking_script(output.script_pubkey.as_bytes()).map(hex::encode),
            Some(file.locking_script)
        );
    }

    #[test]
    fn a_script_without_a_prefix_has_no_token_locking_script() {
        let script = hex::decode("76a914751e76e8199196d454941c45d1b3a323f1433bd688ac").unwrap();
        assert_eq!(locking_script(&script), None);
        assert_eq!(locking_script(&[]), None);
    }

    #[test]
    fn a_prefix_cut_short_does_not_parse() {
        let mut bytes = vec![PREFIX_TOKEN];
        bytes.extend_from_slice(&[0xbb; CATEGORY_BYTES]);
        assert_eq!(locking_script(&bytes), None, "no bitfield");
        bytes.push(HAS_COMMITMENT_LENGTH | 0x20);
        bytes.push(0x05);
        bytes.extend_from_slice(&[0xcc; 4]);
        assert_eq!(locking_script(&bytes), None, "a commitment shorter than its length");
        assert_eq!(compact_size(&[0xfd, 0x01]), None, "a two-byte integer with one byte");
    }
}
