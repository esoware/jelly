use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, CustomFee, FeeLevel,
    KeyError, KeyKind, KeyMaterial, ScriptKind, Transport, payment_uri,
};
use zeroize::Zeroizing;

use crate::address::CODEC;
use crate::keys::MoneroKeys;
use crate::params::{CHAIN, FEE_RATE_UNIT};

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    coin_name: "Monero",
    slip44: 128,
    key_kind: KeyKind::MoneroSpend,
    default_path: "m/44'/128'/{index}'",
    script_kinds: &[],
    default_script: None,
    settled_confirmations: 10,
    poll_secs: 2,
    fee_rate_unit: Some(FEE_RATE_UNIT),
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::Rate),
    // About $2.01 at the October 2026 price of $591. A payment spending one output costs
    // about 0.00003 XMR at the unimportant tier, 0.00012 at normal and 0.0005 at elevated.
    high_fee: Amount::from_base_units(3_400_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "monero",
    explorer_tx_url: Some("https://xmrchain.net/tx/{txid}"),
    explorer_address_url: None,
    transports: &[Transport::MoneroRpc],
    primary_transport: Transport::MoneroRpc,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: true,
    needs_account_key: true,
};

pub const ENTRY: &dyn ChainEntry = &MoneroEntry;

#[derive(Debug)]
struct MoneroEntry;

impl ChainEntry for MoneroEntry {
    fn spec(&self) -> &'static ChainSpec {
        &SPEC
    }

    fn codec(&self) -> &'static dyn AddressCodec {
        CODEC
    }

    fn default_endpoints(&self) -> &'static str {
        include_str!("../endpoints.json")
    }

    /// A `tx_payment_id` is how the recipient matches the payment, and Jelly cannot
    /// send one.
    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        let Some(uri) = payment_uri(SPEC.uri_scheme, value) else {
            return CODEC.validate(value.trim());
        };
        if uri.target.is_empty() || uri.params().any(|(name, _)| name == "tx_payment_id") {
            return Err(AddressError::UnsupportedRequest { chain: CHAIN });
        }
        CODEC.validate(uri.target)
    }

    /// The primary address of the account whose spend scalar `key` holds.
    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        if script.is_some() {
            return Err(KeyError::ScriptNotApplicable { chain: CHAIN });
        }
        MoneroKeys::from_key_material(key)?.address(0, 0)
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Ok(MoneroKeys::from_key_material(key)?.spend_secret_hex())
    }

    fn subaddress(
        &self,
        account: &KeyMaterial,
        major: u32,
        minor: u32,
    ) -> Result<String, KeyError> {
        MoneroKeys::from_key_material(account)?.address(major, minor)
    }

    fn subaddress_key(
        &self,
        account: &KeyMaterial,
        major: u32,
        minor: u32,
    ) -> Result<KeyMaterial, KeyError> {
        Ok(MoneroKeys::from_key_material(account)?.key_material_at(major, minor))
    }

    /// Monero's own 25-word seed, which carries only the spend key.
    fn account_seed_words(&self, account: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        MoneroKeys::from_key_material(account)?.to_seed_words()
    }
}
