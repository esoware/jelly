use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, ImportedKey, KeyError,
    KeyKind, KeyMaterial, ScriptKind, SecretText, Transport, payment_uri,
};
use jelly_keypair::{Secp256k1Key, export_web3_key, import_web3_key, recognize_web3_key};
use zeroize::Zeroizing;

use crate::address::{self, CODEC};
use crate::config::CHAIN;

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    coin_chain: CHAIN,
    slip44: 195,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/195'/0'/0/{index}",
    script_kinds: &[],
    default_script: None,
    // A block is solidified once 19 of the 27 super representatives have
    // confirmed it, about 57 seconds. Past that TRON has no reorg risk in
    // practice.
    settled_confirmations: 19,
    poll_secs: 12,
    fee_rate_unit: None,
    fee_levels: &[],
    custom_fee: None,
    // About $5. That's above the 6.5 to 13.4 TRX a USDT transfer burns without staked
    // energy, so an ordinary token send doesn't warn.
    high_fee: Amount::from_base_units(15_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "tron",
    explorer_tx_url: Some("https://tronscan.org/#/transaction/{txid}"),
    explorer_address_url: Some("https://tronscan.org/#/address/{address}"),
    transports: &[Transport::TronHttp],
    primary_transport: Transport::TronHttp,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const ENTRY: &dyn ChainEntry = &TronEntry;

#[derive(Debug)]
struct TronEntry;

impl ChainEntry for TronEntry {
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
        let target = match payment_uri(SPEC.uri_scheme, value) {
            Some(uri) => {
                if uri.params().any(|(name, _)| name != "amount") {
                    return Err(AddressError::UnsupportedRequest { chain: CHAIN });
                }
                uri.target
            }
            None => value.trim(),
        };
        CODEC.validate(target)
    }

    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        if script.is_some() {
            return Err(KeyError::ScriptNotApplicable { chain: CHAIN });
        }
        if key.kind() != KeyKind::Secp256k1 {
            return Err(KeyError::WrongKeyKind { chain: CHAIN });
        }
        address::from_pubkey(&Secp256k1Key::from_key_material(key)?.public_uncompressed())
    }

    /// TRON keys are Ethereum keys, so this accepts the same two forms an Ethereum
    /// wallet exports.
    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        recognize_web3_key(input)
    }

    fn import_key(
        &self,
        input: &str,
        password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        import_web3_key(input, password)
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        export_web3_key(key, CHAIN)
    }
}
