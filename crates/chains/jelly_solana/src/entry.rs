use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, CustomFee, FeeLevel,
    ImportedKey, KeyError, KeyKind, KeyMaterial, ScriptKind, SecretText, Transport, payment_uri,
};
use jelly_keypair::Ed25519Key;
use zeroize::Zeroizing;

use crate::address::{self, CODEC};
use crate::config::CHAIN;
use crate::keypair::{self, KeyFormat};

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    slip44: 501,
    key_kind: KeyKind::Ed25519Seed,
    default_path: "m/44'/501'/{index}'/0'",
    script_kinds: &[],
    default_script: None,
    settled_confirmations: 1,
    poll_secs: 8,
    fee_rate_unit: None,
    fee_levels: &FeeLevel::ALL,
    custom_fee: Some(CustomFee::PriorityFee),
    // About $2.02 at the October 2026 price of $101. A transfer costs about 0.0000052 SOL,
    // the 5000-lamport base fee and a priority fee of a few hundred lamports.
    high_fee: Amount::from_base_units(20_000_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "solana",
    explorer_tx_url: Some("https://solscan.io/tx/{txid}"),
    explorer_address_url: Some("https://solscan.io/account/{address}"),
    transports: &[Transport::HttpJsonrpc, Transport::WsJsonrpc],
    primary_transport: Transport::HttpJsonrpc,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const ENTRY: &dyn ChainEntry = &SolanaEntry;

#[derive(Debug)]
struct SolanaEntry;

impl ChainEntry for SolanaEntry {
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
                if uri
                    .params()
                    .any(|(name, _)| matches!(name.as_ref(), "reference" | "memo" | "spl-token"))
                {
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
        if key.kind() != KeyKind::Ed25519Seed {
            return Err(KeyError::WrongKeyKind { chain: CHAIN });
        }
        Ok(address::encode(&Ed25519Key::from_key_material(key)?.public_bytes()))
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        keypair::recognize(input).map(KeyFormat::name)
    }

    fn import_key(
        &self,
        input: &str,
        _password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        Ok(ImportedKey { material: keypair::import(input)?, named_script: None })
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        if key.kind() != KeyKind::Ed25519Seed {
            return Err(KeyError::WrongKeyKind { chain: CHAIN });
        }
        keypair::export(key)
    }
}
