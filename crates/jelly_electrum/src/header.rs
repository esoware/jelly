use bitcoin::block;
use bitcoin::consensus::deserialize;

/// The header's own fields. A merge-mined header may continue past them.
const HEADER_BYTES: usize = 80;

/// The version bit a merge-mined header sets when its parent chain's proof of work
/// follows the eighty bytes, as Namecoin's `AuxPoW` defined it and Dogecoin adopted.
const AUXPOW_VERSION_BIT: i32 = 1 << 8;

/// The eighty-byte header at the front of `text`.
///
/// Anything after it is refused unless the chain is merge-mined and the header's
/// version says a proof follows. A merge-mined header sent without its proof, as
/// `ElectrumX` does under a checkpoint, is accepted too.
pub(crate) fn header_from_hex(text: &str, is_merge_mined: bool) -> Option<block::Header> {
    let bytes = hex::decode(text.trim()).ok()?;
    let (fields, proof) = bytes.split_at_checked(HEADER_BYTES)?;
    let header: block::Header = deserialize(fields).ok()?;
    let has_proof = is_merge_mined && header.version.to_consensus() & AUXPOW_VERSION_BIT != 0;
    (proof.is_empty() || has_proof).then_some(header)
}

#[cfg(test)]
mod tests {
    use bitcoin::consensus::serialize;

    use super::*;

    /// Dogecoin's block 5,000,000 as `ElectrumX` serves it, its eighty bytes followed
    /// by the Litecoin proof of work that mined it.
    const MERGE_MINED: &str = include_str!("../tests/vectors/auxpow_header.json");

    fn merge_mined() -> (String, String) {
        let file: serde_json::Value = serde_json::from_str(MERGE_MINED).unwrap();
        let text =
            |key: &str| file.get(key).and_then(serde_json::Value::as_str).unwrap().to_owned();
        (text("hex"), text("hash"))
    }

    #[test]
    fn a_merge_mined_header_reads_as_its_eighty_bytes() {
        let (hex_text, hash) = merge_mined();
        assert!(hex_text.len() > HEADER_BYTES * 2, "the proof follows the header");
        let header = header_from_hex(&hex_text, true).unwrap();
        assert_eq!(header.block_hash().to_string(), hash);
        assert_ne!(header.version.to_consensus() & AUXPOW_VERSION_BIT, 0);
    }

    #[test]
    fn trailing_bytes_are_refused_on_a_chain_that_is_not_merge_mined() {
        let (hex_text, _) = merge_mined();
        assert_eq!(header_from_hex(&hex_text, false), None);
    }

    #[test]
    fn a_header_without_the_auxpow_bit_must_end_at_eighty_bytes() {
        let (hex_text, _) = merge_mined();
        let mut bytes = hex::decode(&hex_text).unwrap();
        let mut header: block::Header = deserialize(&bytes[..HEADER_BYTES]).unwrap();
        header.version =
            block::Version::from_consensus(header.version.to_consensus() & !AUXPOW_VERSION_BIT);
        bytes[..HEADER_BYTES].copy_from_slice(&serialize(&header));
        assert_eq!(header_from_hex(&hex::encode(&bytes), true), None);
        let fields = &bytes[..HEADER_BYTES];
        assert!(header_from_hex(&hex::encode(fields), true).is_some());
    }

    #[test]
    fn a_short_header_is_refused() {
        let (hex_text, _) = merge_mined();
        let short = hex_text.get(..(HEADER_BYTES - 1) * 2).unwrap();
        assert_eq!(header_from_hex(short, true), None);
        assert_eq!(header_from_hex("not hex", true), None);
    }
}
