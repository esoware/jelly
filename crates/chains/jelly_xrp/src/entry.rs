use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, CustomFee, FeeLevel,
    ImportedKey, KeyError, KeyKind, KeyMaterial, ScriptKind, SecretText, Transport,
};
use jelly_keypair::{
    HEX_KEY_FORMAT, Secp256k1Key, decode_secret_hex, encode_secret_hex, is_secret_hex,
};
use zeroize::Zeroizing;

use crate::address::{CODEC, pubkey_address};
use crate::config::CHAIN;

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    coin_chain: CHAIN,
    slip44: 144,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/144'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    settled_confirmations: 1,
    poll_secs: 4,
    fee_rate_unit: None,
    fee_levels: &[FeeLevel::Normal],
    custom_fee: Some(CustomFee::Total),
    // About $2. A payment's fee is capped at 0.1 XRP, so only an account deletion, which pays
    // the owner reserve, could reach it.
    high_fee: Amount::from_base_units(1_500_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "xrpl",
    explorer_tx_url: Some("https://livenet.xrpl.org/transactions/{txid}"),
    explorer_address_url: Some("https://livenet.xrpl.org/accounts/{address}"),
    transports: &[Transport::HttpJsonrpc],
    primary_transport: Transport::HttpJsonrpc,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: true,
    has_destination_tags: true,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const ENTRY: &dyn ChainEntry = &XrpEntry;

#[derive(Debug)]
struct XrpEntry;

impl ChainEntry for XrpEntry {
    fn spec(&self) -> &'static ChainSpec {
        &SPEC
    }

    fn codec(&self) -> &'static dyn AddressCodec {
        CODEC
    }

    fn default_endpoints(&self) -> &'static str {
        include_str!("../endpoints.json")
    }

    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        // The codec reads an `xrpl:` request itself, because the destination tag it can
        // carry decides who the payment reaches.
        CODEC.validate(value)
    }

    fn address(&self, key: &KeyMaterial, _script: Option<ScriptKind>) -> Result<String, KeyError> {
        Ok(pubkey_address(&Secp256k1Key::from_key_material(key)?.public_compressed()))
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        is_secret_hex(input).then_some(HEX_KEY_FORMAT)
    }

    fn import_key(
        &self,
        input: &str,
        _password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        let secret = decode_secret_hex(input).ok_or(KeyError::UnknownFormat)?;
        let material = Secp256k1Key::from_secret_bytes(secret.as_slice())?.key_material();
        Ok(ImportedKey { material, named_script: None })
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Ok(Secp256k1Key::from_key_material(key)?.with_secret_bytes(encode_secret_hex))
    }
}
