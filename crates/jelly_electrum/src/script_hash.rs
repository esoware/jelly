use std::fmt;

use bitcoin::Script;
use serde::{Serialize, Serializer};
use sha2::{Digest as _, Sha256};

const HASH_BYTES: usize = 32;

/// Electrum's name for an output script, the `SHA-256` of the script bytes
/// reversed.
///
/// The hex form goes on the wire, so `Display` and `Serialize` produce the same 64
/// characters.
pub(crate) struct ElectrumScriptHash([u8; HASH_BYTES]);

impl ElectrumScriptHash {
    pub(crate) fn of(script: &Script) -> Self {
        let mut bytes: [u8; HASH_BYTES] = Sha256::digest(script.as_bytes()).into();
        bytes.reverse();
        Self(bytes)
    }
}

impl fmt::Display for ElectrumScriptHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl Serialize for ElectrumScriptHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::ScriptBuf;

    use super::*;

    /// Both vectors are published in the Electrum protocol basics document.
    #[test]
    fn the_published_scripthash_vectors_match() {
        let p2pkh = ScriptBuf::from_hex("76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac")
            .expect("a script");
        assert_eq!(
            ElectrumScriptHash::of(&p2pkh).to_string(),
            "8b01df4e368ea28f8dc0423bcf7a4923e3a12d307c875e47a0cfbf90b5c39161"
        );

        let p2pk = ScriptBuf::from_hex(
            "4104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f\
             35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac",
        )
        .expect("a script");
        assert_eq!(
            ElectrumScriptHash::of(&p2pk).to_string(),
            "740485f380ff6379d11ef6fe7d7cdd68aea7f8bd0d953d9fdf3531fb7d531833"
        );
    }

    /// What a request carries and what a log line shows have to be the same hex
    /// string.
    #[test]
    fn a_scripthash_serializes_as_the_hex_it_displays() {
        let script = ScriptBuf::from_hex("0014000102030405060708090a0b0c0d0e0f1011121314")
            .expect("a script");
        let hash = ElectrumScriptHash::of(&script);
        let text = hash.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(serde_json::to_string(&hash).expect("it serializes"), format!("\"{text}\""));
    }
}
