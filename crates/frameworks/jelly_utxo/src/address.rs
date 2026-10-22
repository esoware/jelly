use bitcoin::bech32::primitives::decode::SegwitHrpstringError;
use bitcoin::bech32::{Fe32, Hrp, segwit};
use bitcoin::{CompressedPublicKey, Script, ScriptBuf, WitnessProgram, WitnessVersion};
use jelly_core::{Address, AddressCodec, AddressError, ChainId, ScriptKind};
use jelly_keypair::verification_context;

use crate::legacy::{Base58Versions, Legacy};
use crate::script::UtxoScript;

/// A 21-byte payload and its checksum never take more base58 characters than this, so
/// longer text is refused by its length before any character is read.
const MAX_LEGACY_CHARS: usize = 50;

/// The codec of a chain that writes Bitcoin's address forms: segwit addresses in bech32
/// and bech32m under the chain's human-readable part, and the base58 P2PKH and P2SH
/// forms under its version bytes.
///
/// A form this wallet derives no key for is still a form it can pay, so P2WSH and
/// unknown witness versions produce an output script but answer `None` to
/// [`AddressCodec::script_kind`].
#[derive(Debug)]
pub struct UtxoCodec {
    pub chain: ChainId,
    /// The human-readable part of the chain's segwit addresses, such as `bc`, or `None`
    /// for a chain without segregated witness, which reads every segwit address as
    /// another network's.
    pub segwit_hrp: Option<&'static str>,
    pub base58: Base58Versions,
}

/// `kind` is the single-key flavour the output matches, if any.
struct Parsed {
    canonical: String,
    script: ScriptBuf,
    kind: Option<ScriptKind>,
}

impl AddressCodec for UtxoCodec {
    fn chain(&self) -> ChainId {
        self.chain
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        Ok(Address::assume_canonical(self.chain, self.parse(value)?.canonical))
    }

    fn script_pubkey(&self, value: &str) -> Result<Vec<u8>, AddressError> {
        Ok(self.parse(value)?.script.into_bytes())
    }

    fn script_kind(&self, value: &str) -> Option<ScriptKind> {
        self.parse(value).ok()?.kind
    }

    /// Every witness version renders, so a preview never hides a recipient it can
    /// spell.
    fn address_of(&self, script_pubkey: &[u8]) -> Option<String> {
        let script = Script::from_bytes(script_pubkey);
        if let Some(version) = script.witness_version() {
            let program = script_pubkey.get(2..)?;
            return segwit::encode(self.hrp()?, Fe32::from(version), program).ok();
        }
        Legacy::of(script).map(|legacy| self.base58.encode(legacy))
    }
}

impl UtxoCodec {
    fn hrp(&self) -> Option<Hrp> {
        self.segwit_hrp.map(Hrp::parse_unchecked)
    }

    /// Bech32 is tried first because it names the human-readable part, so an address
    /// for another network is reported as such rather than as a broken base58
    /// checksum.
    fn parse(&self, value: &str) -> Result<Parsed, AddressError> {
        let chain = self.chain;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(AddressError::Empty { chain });
        }
        if let Some(hrp) = self.segwit_hrp
            && let Some(error) = segwit_error(chain, hrp, trimmed)
        {
            return Err(error);
        }
        if let Ok((parsed_hrp, version, program)) = segwit::decode(trimmed) {
            return self.parse_segwit(trimmed, parsed_hrp, version, &program);
        }
        if trimmed.len() > MAX_LEGACY_CHARS {
            return Err(AddressError::WrongLength { chain });
        }
        self.parse_base58(trimmed)
    }

    /// Relies on the decoder having already applied every segwit rule.
    ///
    /// Those rules include bech32 for version zero and bech32m above it, one case
    /// throughout, a version of at most 16, and a program of 2 to 40 bytes that is 20 or
    /// 32 at version zero.
    fn parse_segwit(
        &self,
        value: &str,
        parsed_hrp: Hrp,
        version: Fe32,
        program: &[u8],
    ) -> Result<Parsed, AddressError> {
        let chain = self.chain;
        if self.hrp() != Some(parsed_hrp) {
            return Err(AddressError::WrongNetwork { chain });
        }
        let kind = match (version.to_u8(), program.len()) {
            (0, 20) => Some(ScriptKind::P2wpkh),
            (1, 32) => Some(ScriptKind::P2tr),
            _ => None,
        };
        let witness_program = WitnessVersion::try_from(version)
            .ok()
            .and_then(|witness_version| WitnessProgram::new(witness_version, program).ok())
            .ok_or(AddressError::WrongLength { chain })?;
        Ok(Parsed {
            canonical: value.to_ascii_lowercase(),
            script: ScriptBuf::new_witness_program(&witness_program),
            kind,
        })
    }

    /// A P2SH address reads as wrapped segwit on a chain with segwit, because the hash
    /// hides the redeem script, and as no single-key flavour on a chain without it.
    fn parse_base58(&self, value: &str) -> Result<Parsed, AddressError> {
        let legacy = self.base58.decode(self.chain, value)?;
        let kind = match legacy {
            Legacy::PubkeyHash(_) => Some(ScriptKind::P2pkh),
            Legacy::ScriptHash(_) => self.segwit_hrp.map(|_| ScriptKind::P2shP2wpkh),
        };
        Ok(Parsed { canonical: value.to_owned(), script: legacy.script_pubkey(), kind })
    }
}

/// Why `value`, spelled as a segwit address of `chain`, does not decode as one. `None`
/// when it decodes, or when it is not spelled as one and may be base58.
///
/// A string the segwit decoder recognizes as bech32 is judged by that decoder alone, and
/// so is one that begins with the chain's own `hrp` and separator, which no base58
/// address on a chain with segwit does. A base58 reading of a mistyped segwit address
/// would describe base58.
fn segwit_error(chain: ChainId, hrp: &str, value: &str) -> Option<AddressError> {
    let error = segwit::decode(value).err()?;
    Some(match error.0 {
        SegwitHrpstringError::Unchecked(_) => {
            let (prefix, rest) = value.split_at_checked(hrp.len())?;
            if !prefix.eq_ignore_ascii_case(hrp) || !rest.starts_with('1') {
                return None;
            }
            // The decoder stops before the checksum at a character outside the alphabet
            // or one that changes case, or when nothing follows the separator.
            let mut is_first_letter_upper = None;
            let broken = value.char_indices().find_map(|(position, character)| {
                let is_upper = character.is_ascii_uppercase();
                let is_case_change = character.is_ascii_alphabetic()
                    && *is_first_letter_upper.get_or_insert(is_upper) != is_upper;
                let is_outside_alphabet =
                    position > hrp.len() && Fe32::from_char(character).is_err();
                (is_case_change || is_outside_alphabet).then_some(position)
            });
            match broken {
                Some(position) => AddressError::Malformed { chain, position },
                None => AddressError::WrongLength { chain },
            }
        }
        SegwitHrpstringError::TooLong(_) => AddressError::WrongLength { chain },
        SegwitHrpstringError::NoData
        | SegwitHrpstringError::InvalidWitnessVersion(_)
        | SegwitHrpstringError::Padding(_)
        | SegwitHrpstringError::WitnessLength(_)
        | SegwitHrpstringError::Checksum(_)
        | _ => AddressError::Checksum { chain },
    })
}

/// The output script `pubkey` pays in the `script` flavour.
///
/// P2TR is the BIP-86 key-path-only output. The internal key is tweaked by
/// `tagged_hash("TapTweak", ser_x(P))` with no script tree, as an `m/86'` path
/// requires.
pub(crate) fn key_script(script: UtxoScript, pubkey: &CompressedPublicKey) -> ScriptBuf {
    match script {
        UtxoScript::P2tr => ScriptBuf::new_p2tr(verification_context(), (*pubkey).into(), None),
        UtxoScript::P2wpkh => ScriptBuf::new_p2wpkh(&pubkey.wpubkey_hash()),
        UtxoScript::P2shP2wpkh => {
            ScriptBuf::new_p2sh(&ScriptBuf::new_p2wpkh(&pubkey.wpubkey_hash()).script_hash())
        }
        UtxoScript::P2pkh => ScriptBuf::new_p2pkh(&pubkey.pubkey_hash()),
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::base58;

    use super::*;
    use crate::test_util::BITCOIN;

    #[test]
    fn length_errors_do_not_confuse_text_and_payload_sizes() {
        for value in [base58::encode_check(&[0; 22]), "2".repeat(51)] {
            let error = BITCOIN.codec.validate(&value).unwrap_err();
            assert_eq!(error, AddressError::WrongLength { chain: ChainId::Bitcoin });
            assert_eq!(
                error.to_string(),
                "The Bitcoin address has an invalid length. Check the address."
            );
        }
    }
}
