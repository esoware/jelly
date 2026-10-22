use bitcoin::bech32::segwit;
use bitcoin::hashes::Hash as _;
use bitcoin::{PubkeyHash, Script, ScriptBuf, ScriptHash, base58};
use jelly_core::{AddressError, ChainId};

/// The `Base58Check` version bytes of one chain's two legacy address forms.
#[derive(Debug, Clone, Copy)]
pub struct Base58Versions {
    pub p2pkh: u8,
    pub p2sh: u8,
    /// A superseded P2SH version byte the chain's node software still accepts when
    /// decoding. It is read and never written, so one script never has two spellings.
    pub superseded_p2sh: Option<u8>,
}

/// A legacy address, which pays either a public key hash or a script hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legacy {
    PubkeyHash(PubkeyHash),
    ScriptHash(ScriptHash),
}

impl Legacy {
    pub fn script_pubkey(self) -> ScriptBuf {
        match self {
            Self::PubkeyHash(hash) => ScriptBuf::new_p2pkh(&hash),
            Self::ScriptHash(hash) => ScriptBuf::new_p2sh(&hash),
        }
    }

    /// The legacy form `script_pubkey` pays, when it is one.
    pub fn of(script_pubkey: &Script) -> Option<Self> {
        let bytes = script_pubkey.as_bytes();
        if script_pubkey.is_p2pkh() {
            let hash: [u8; 20] = bytes.get(3..23)?.try_into().ok()?;
            Some(Self::PubkeyHash(PubkeyHash::from_byte_array(hash)))
        } else if script_pubkey.is_p2sh() {
            let hash: [u8; 20] = bytes.get(2..22)?.try_into().ok()?;
            Some(Self::ScriptHash(ScriptHash::from_byte_array(hash)))
        } else {
            None
        }
    }
}

impl Base58Versions {
    /// Reads a legacy address of `chain`.
    ///
    /// A chain with segwit addresses of its own reads those before it gets here, so a
    /// string the segwit decoder accepts is another network's address, and it is
    /// reported as such rather than as a broken base58 checksum.
    pub fn decode(self, chain: ChainId, value: &str) -> Result<Legacy, AddressError> {
        if segwit::decode(value).is_ok() {
            return Err(AddressError::WrongNetwork { chain });
        }
        let decoded = base58::decode_check(value).map_err(|error| match error {
            // The decoder stops at the first invalid byte, so that byte's first occurrence
            // is its position.
            base58::Error::Decode(invalid) => {
                let byte = invalid.invalid_base58_character();
                let position = value.bytes().position(|candidate| candidate == byte);
                AddressError::Malformed { chain, position: position.unwrap_or_default() }
            }
            base58::Error::TooShort(_) => AddressError::WrongLength { chain },
            base58::Error::IncorrectChecksum(_) | _ => AddressError::Checksum { chain },
        })?;
        let (version, payload) =
            decoded.split_first().ok_or(AddressError::WrongLength { chain })?;
        let hash: [u8; 20] = payload.try_into().map_err(|_| AddressError::WrongLength { chain })?;
        if *version == self.p2pkh {
            Ok(Legacy::PubkeyHash(PubkeyHash::from_byte_array(hash)))
        } else if *version == self.p2sh || Some(*version) == self.superseded_p2sh {
            Ok(Legacy::ScriptHash(ScriptHash::from_byte_array(hash)))
        } else {
            Err(AddressError::WrongNetwork { chain })
        }
    }

    pub(crate) fn encode(self, legacy: Legacy) -> String {
        let (version, hash) = match legacy {
            Legacy::PubkeyHash(hash) => (self.p2pkh, hash.to_byte_array()),
            Legacy::ScriptHash(hash) => (self.p2sh, hash.to_byte_array()),
        };
        let mut payload = [version; 21];
        let [_, body @ ..] = &mut payload;
        *body = hash;
        base58::encode_check(&payload)
    }
}

/// Whether `text` is a legacy address, which its base58 checksum settles.
///
/// The alphabet does not, since a legacy address can avoid every character base32
/// lacks, as about one P2SH address in twenty does. A `CashAddr` payload passes the
/// four-byte checksum about once in four billion tries.
pub fn is_legacy(text: &str) -> bool {
    base58::decode_check(text).is_ok()
}
