use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, ImportedKey, KeyError,
    KeyKind, KeyMaterial, ScriptKind, SecretText, Transport, phrase_words,
};
use jelly_keypair::{
    Ed25519Key, HEX_KEY_FORMAT, decode_secret_hex, encode_secret_hex, is_secret_hex,
};
use zeroize::Zeroizing;

use crate::address::CODEC;
use crate::config::CHAIN;
use crate::seed::{decode_ton_seed, is_ton_seed_shape};
use crate::wallet::{Version, wallet_address};

const TON_SEED: &str = "TON seed";

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    slip44: 607,
    key_kind: KeyKind::Ed25519Seed,
    default_path: "m/44'/607'/{index}'",
    script_kinds: &[ScriptKind::TonV5r1, ScriptKind::TonV4r2, ScriptKind::TonV3r2],
    default_script: Some(ScriptKind::TonV5r1),
    settled_confirmations: 1,
    poll_secs: 4,
    fee_rate_unit: None,
    fee_levels: &[],
    custom_fee: None,
    // Under $1 at 2026 prices. A payment costs a few thousandths of a Gram, and a jetton
    // transfer adds the gas it attaches, 0.018 for USDT and 0.05 for XAUT, most of which
    // comes back. Update it when the price moves a lot.
    high_fee: Amount::from_base_units(100_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "ton",
    explorer_tx_url: Some("https://tonviewer.com/transaction/{txid}"),
    explorer_address_url: Some("https://tonviewer.com/{address}"),
    transports: &[Transport::HttpJsonrpc],
    primary_transport: Transport::HttpJsonrpc,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: true,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const ENTRY: &dyn ChainEntry = &TonEntry;

#[derive(Debug)]
struct TonEntry;

impl ChainEntry for TonEntry {
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
        // The codec reads a `ton://transfer/` request itself, because the comment it can
        // carry is often what the recipient credits the payment by.
        CODEC.validate(value)
    }

    /// The adapter reads the comment out of the request, so a send keeps the request
    /// rather than the bare address.
    fn payment_target(&self, value: &str) -> Result<String, AddressError> {
        CODEC.validate(value)?;
        Ok(value.trim().to_owned())
    }

    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        let script = script.ok_or(KeyError::internal("script kind unresolved"))?;
        let pubkey = Ed25519Key::from_key_material(key)?.public_bytes();
        wallet_address(&pubkey, Version::try_from(script)?)
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        if is_ton_seed_shape(input) {
            Some(TON_SEED)
        } else {
            is_secret_hex(input).then_some(HEX_KEY_FORMAT)
        }
    }

    /// A phrase is read as a TON mnemonic, with `password` as the mnemonic's password.
    fn import_key(
        &self,
        input: &str,
        password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        let seed = if phrase_words(input).nth(1).is_some() {
            decode_ton_seed(input, password)?
        } else {
            decode_secret_hex(input).ok_or(KeyError::UnknownFormat)?
        };
        Ok(ImportedKey {
            material: Ed25519Key::from_seed_bytes(&seed).key_material(),
            named_script: None,
        })
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Ok(Ed25519Key::from_key_material(key)?.with_secret_bytes(encode_secret_hex))
    }
}
