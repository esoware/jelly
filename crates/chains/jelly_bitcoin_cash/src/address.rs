use bitcoin::bech32::Fe32;
use bitcoin::hashes::Hash as _;
use bitcoin::opcodes::all::{OP_EQUAL, OP_HASH256};
use bitcoin::{PubkeyHash, Script, ScriptBuf, ScriptHash};
use jelly_core::{Address, AddressCodec, AddressError, ChainId, ScriptKind};
use jelly_utxo::{Base58Versions, Legacy, is_legacy};

use crate::bitcoin_cash::SPEC;
use crate::cashaddr::{self, CashAddr};

const CHAIN: ChainId = SPEC.chain;

/// The mainnet `CashAddr` prefix, which is also the payment request scheme. Text
/// without a prefix is read against it.
const PREFIX: &str = SPEC.uri_scheme;

/// Bitcoin's version bytes, kept at the split. A legacy address is accepted as written
/// and never produced.
const LEGACY: Base58Versions = Base58Versions { p2pkh: 0x00, p2sh: 0x05, superseded_p2sh: None };

/// `CashAddr` type bits. A token-aware type pays the same script as its plain twin and
/// only tells a token wallet that the recipient accepts `CashTokens`.
const P2PKH: u8 = 0;
const P2SH: u8 = 1;
const TOKEN_AWARE_P2PKH: u8 = 2;
const TOKEN_AWARE_P2SH: u8 = 3;

/// Reads `CashAddr`, with or without its prefix and in either case, and the legacy
/// base58 forms. Writes `CashAddr`.
///
/// Every known `CashAddr` type maps to a standard script, the 32-byte P2SH form
/// included, so Jelly can pay forms it doesn't derive.
#[derive(Debug)]
pub(crate) struct CashAddrCodec;

impl AddressCodec for CashAddrCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        Ok(Address::assume_canonical(CHAIN, parse(value)?.canonical))
    }

    fn script_pubkey(&self, value: &str) -> Result<Vec<u8>, AddressError> {
        Ok(parse(value)?.script.into_bytes())
    }

    fn script_kind(&self, value: &str) -> Option<ScriptKind> {
        parse(value).ok()?.kind
    }

    fn address_of(&self, script_pubkey: &[u8]) -> Option<String> {
        let address = match Legacy::of(Script::from_bytes(script_pubkey)) {
            Some(Legacy::PubkeyHash(hash)) => CashAddr::new(PREFIX, P2PKH, hash.as_byte_array()),
            Some(Legacy::ScriptHash(hash)) => CashAddr::new(PREFIX, P2SH, hash.as_byte_array()),
            None => CashAddr::new(PREFIX, P2SH, &p2sh32_hash(script_pubkey)?),
        };
        Some(address?.encode())
    }
}

/// `kind` is set only for P2PKH, the one single-key script here.
struct Parsed {
    canonical: String,
    script: ScriptBuf,
    kind: Option<ScriptKind>,
}

/// Anything that isn't a legacy address is read as `CashAddr` if it has a prefix or is
/// all base32, and as a broken legacy address otherwise.
fn parse(value: &str) -> Result<Parsed, AddressError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    let is_base32 = trimmed.chars().all(|character| Fe32::from_char(character).is_ok());
    if !is_legacy(trimmed) && (trimmed.contains(':') || is_base32) {
        parse_cashaddr(trimmed)
    } else {
        parse_legacy(trimmed)
    }
}

fn parse_cashaddr(value: &str) -> Result<Parsed, AddressError> {
    let decoded = CashAddr::decode(value, PREFIX).map_err(|error| match error {
        cashaddr::Error::InvalidCharacter { position }
        | cashaddr::Error::MixedCase { position } => {
            AddressError::Malformed { chain: CHAIN, position }
        }
        cashaddr::Error::Checksum => AddressError::Checksum { chain: CHAIN },
        cashaddr::Error::Length => AddressError::WrongLength { chain: CHAIN },
        cashaddr::Error::ReservedBit => AddressError::Invalid { chain: CHAIN },
    })?;
    if decoded.prefix != PREFIX {
        return Err(AddressError::WrongNetwork { chain: CHAIN });
    }
    let hash = decoded.hash.as_slice();
    let (script, kind) = match decoded.address_type() {
        P2PKH | TOKEN_AWARE_P2PKH => {
            let pubkey_hash = PubkeyHash::from_slice(hash)
                .map_err(|_| AddressError::WrongLength { chain: CHAIN })?;
            (ScriptBuf::new_p2pkh(&pubkey_hash), Some(ScriptKind::P2pkh))
        }
        P2SH | TOKEN_AWARE_P2SH => {
            let script = if let Ok(long_hash) = <[u8; 32]>::try_from(hash) {
                p2sh32_script(long_hash)
            } else {
                let script_hash = ScriptHash::from_slice(hash)
                    .map_err(|_| AddressError::WrongLength { chain: CHAIN })?;
                ScriptBuf::new_p2sh(&script_hash)
            };
            (script, None)
        }
        _ => return Err(AddressError::Invalid { chain: CHAIN }),
    };
    Ok(Parsed { canonical: decoded.encode(), script, kind })
}

fn parse_legacy(value: &str) -> Result<Parsed, AddressError> {
    let legacy = LEGACY.decode(CHAIN, value)?;
    let kind = match legacy {
        Legacy::PubkeyHash(_) => Some(ScriptKind::P2pkh),
        Legacy::ScriptHash(_) => None,
    };
    Ok(Parsed { canonical: value.to_owned(), script: legacy.script_pubkey(), kind })
}

/// `OP_HASH256 <32 bytes> OP_EQUAL`, the P2SH form Bitcoin Cash added in May 2023.
fn p2sh32_script(hash: [u8; 32]) -> ScriptBuf {
    ScriptBuf::builder()
        .push_opcode(OP_HASH256)
        .push_slice(hash)
        .push_opcode(OP_EQUAL)
        .into_script()
}

fn p2sh32_hash(script_pubkey: &[u8]) -> Option<[u8; 32]> {
    let [op_hash, push, hash @ .., op_equal] = script_pubkey else {
        return None;
    };
    if *op_hash != OP_HASH256.to_u8() || *push != 32 || *op_equal != OP_EQUAL.to_u8() {
        return None;
    }
    hash.try_into().ok()
}
