use bitcoin::CompressedPublicKey;
use bitcoin::hashes::Hash as _;
use jelly_core::{Address, AddressCodec, AddressError, ChainId, KeyError, ScriptKind};
use zcash_address::{ConversionError, ParseError, TryFromAddress, ZcashAddress};
use zcash_protocol::consensus::NetworkType;
use zcash_script::{
    script::{self, Code, Evaluable as _},
    solver,
};
use zcash_transparent::{
    address::{Script, TransparentAddress},
    bundle::TxOut,
};

use crate::config::CHAIN;
use crate::error::ZcashError;

pub(crate) const CODEC: &dyn AddressCodec = &ZcashCodec;

#[derive(Debug)]
struct ZcashCodec;

impl AddressCodec for ZcashCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        Ok(Address::assume_canonical(CHAIN, encode(parse(value)?)))
    }

    fn script_pubkey(&self, value: &str) -> Result<Vec<u8>, AddressError> {
        Ok(parse(value)?.script().to_bytes())
    }

    fn script_kind(&self, value: &str) -> Option<ScriptKind> {
        match parse(value).ok()? {
            TransparentAddress::PublicKeyHash(_) => Some(ScriptKind::P2pkh),
            TransparentAddress::ScriptHash(_) => None,
        }
    }

    fn address_of(&self, script_pubkey: &[u8]) -> Option<String> {
        let script = script::PubKey::parse(&Code(script_pubkey.to_vec())).ok()?;
        TransparentAddress::from_script_pubkey(&script).map(encode)
    }
}

/// A mainnet transparent destination. A TEX address is ZIP 320's spelling of a key
/// hash that accepts only transparent inputs, which every payment built here spends,
/// so it pays that key hash.
struct Destination(TransparentAddress);

impl TryFromAddress for Destination {
    type Error = ();

    fn try_from_transparent_p2pkh(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::PublicKeyHash(data)))
    }

    fn try_from_transparent_p2sh(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::ScriptHash(data)))
    }

    fn try_from_tex(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::PublicKeyHash(data)))
    }
}

/// The mainnet P2PKH address of `pubkey`, the only single-key form. An uncompressed key
/// is read as its compressed form, so a pay-to-pubkey output to either spelling belongs
/// to the same address.
pub(crate) fn pubkey_address(pubkey: &[u8]) -> Result<String, KeyError> {
    let key = CompressedPublicKey::from_slice(pubkey).map_err(|_| KeyError::InvalidPublicKey)?;
    Ok(encode(TransparentAddress::PublicKeyHash(key.pubkey_hash().to_byte_array())))
}

pub(crate) fn script(address: &str) -> Result<Script, ZcashError> {
    let transparent =
        parse(address).map_err(|_| ZcashError::Internal("a Zcash address has no script"))?;
    Ok(Script(Code(transparent.script().to_bytes())))
}

pub(crate) fn is_key_address(address: &str) -> bool {
    matches!(parse(address), Ok(TransparentAddress::PublicKeyHash(_)))
}

/// Blockbook lists a bare pay-to-pubkey output under the address of the key it pays, so
/// the address's history and unspent outputs can hold one beside its key-hash outputs.
pub(crate) fn is_pay_to_pubkey(output: &TxOut, address: &str) -> bool {
    matches!(
        output.script_kind(),
        Some(solver::ScriptKind::PubKey { data })
            if pubkey_address(data.as_slice()).is_ok_and(|owner| owner == address)
    )
}

fn encode(transparent: TransparentAddress) -> String {
    transparent.to_zcash_address(NetworkType::Main).encode()
}

fn parse(value: &str) -> Result<TransparentAddress, AddressError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    // Bech32 permits an all-uppercase spelling, which `zcash_address` reads only in
    // lowercase, and no Base58Check address of Zcash's is written in upper case alone.
    let lowered;
    let value = if value.bytes().any(|byte| byte.is_ascii_lowercase()) {
        value
    } else {
        lowered = value.to_ascii_lowercase();
        lowered.as_str()
    };
    let parsed = ZcashAddress::try_from_encoded(value).map_err(|error| match error {
        ParseError::InvalidEncoding => AddressError::WrongLength { chain: CHAIN },
        ParseError::NotZcash | ParseError::Unified(_) => AddressError::Invalid { chain: CHAIN },
    })?;
    parsed
        .convert_if_network::<Destination>(NetworkType::Main)
        .map(|destination| destination.0)
        .map_err(|error| match error {
            ConversionError::IncorrectNetwork { .. } => AddressError::WrongNetwork { chain: CHAIN },
            ConversionError::Unsupported(_) => AddressError::UnsupportedRequest { chain: CHAIN },
            ConversionError::User(()) => AddressError::Invalid { chain: CHAIN },
        })
}

#[cfg(test)]
mod tests {
    use zcash_protocol::value::Zatoshis;

    use super::*;
    use crate::fixtures::{key, key_address};

    fn pay_to_pubkey(pubkey: &[u8]) -> TxOut {
        let mut code = vec![u8::try_from(pubkey.len()).unwrap()];
        code.extend_from_slice(pubkey);
        code.push(0xac);
        TxOut::new(Zatoshis::from_u64(10_000).unwrap(), Script(Code(code)))
    }

    #[test]
    fn a_pay_to_pubkey_output_belongs_only_to_the_address_of_its_key() {
        let owner = key(7);
        let address = key_address(7);
        assert!(is_pay_to_pubkey(&pay_to_pubkey(&owner.public_compressed()), &address));
        assert!(is_pay_to_pubkey(&pay_to_pubkey(&owner.public_uncompressed()), &address));
        assert!(!is_pay_to_pubkey(&pay_to_pubkey(&key(9).public_compressed()), &address));
        let key_hash = TxOut::new(Zatoshis::from_u64(10_000).unwrap(), script(&address).unwrap());
        assert!(!is_pay_to_pubkey(&key_hash, &address));
    }
}
