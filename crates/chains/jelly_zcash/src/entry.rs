use jelly_core::{
    Address, AddressCodec, AddressError, Amount, ChainEntry, ChainSpec, CustomFee, FeeLevel,
    ImportedKey, KeyError, KeyKind, KeyMaterial, ScriptKind, SecretText, Transport, payment_uri,
};
use jelly_keypair::{
    HEX_KEY_FORMAT, Secp256k1Key, decode_secret_hex, is_secret_hex, wif_decode, wif_encode,
    wif_version,
};
use zeroize::Zeroizing;

use crate::address::{CODEC, pubkey_address};
use crate::config::CHAIN;

/// zcashd kept Bitcoin's WIF version byte.
const WIF_VERSION: u8 = 0x80;

const WIF_FORMAT: &str = "Bitcoin WIF key";

pub const SPEC: ChainSpec = ChainSpec {
    chain: CHAIN,
    coin_name: "Zcash",
    coin_chain: CHAIN,
    slip44: 133,
    key_kind: KeyKind::Secp256k1,
    default_path: "m/44'/133'/0'/0/{index}",
    script_kinds: &[ScriptKind::P2pkh],
    default_script: Some(ScriptKind::P2pkh),
    // ZIP 315 has a wallet wait ten blocks, about twelve minutes, before it trusts
    // funds that someone else sent.
    settled_confirmations: 10,
    poll_secs: 30,
    fee_rate_unit: None,
    fee_levels: &[FeeLevel::Normal],
    custom_fee: Some(CustomFee::Total),
    // About $2. A payment with up to two inputs and two outputs pays the ZIP 317 minimum of
    // 0.0001 ZEC, and each input past that adds 0.00005.
    high_fee: Amount::from_base_units(160_000),
    can_pin_nonce: false,
    can_carry_data: false,
    uri_scheme: "zcash",
    explorer_tx_url: Some("https://blockbook.zec.zelcore.io/tx/{txid}"),
    explorer_address_url: Some("https://blockbook.zec.zelcore.io/address/{address}"),
    transports: &[Transport::Blockbook],
    primary_transport: Transport::Blockbook,
    can_bump_fee: false,
    can_cancel: false,
    can_delete_account: false,
    has_destination_tags: false,
    has_comments: false,
    has_subaddresses: false,
    needs_account_key: false,
};

pub const ENTRY: &dyn ChainEntry = &ZcashEntry;

#[derive(Debug)]
struct ZcashEntry;

impl ChainEntry for ZcashEntry {
    fn spec(&self) -> &'static ChainSpec {
        &SPEC
    }

    fn codec(&self) -> &'static dyn AddressCodec {
        CODEC
    }

    fn default_endpoints(&self) -> &'static str {
        include_str!("../endpoints.json")
    }

    /// ZIP 321 lets a URI leave the address out and name its payee in a single `address`
    /// parameter instead. Any parameter beyond the payee, amount, label, and message,
    /// such as a memo, is one Jelly can't send, so the URI is refused.
    fn validate_address(&self, value: &str) -> Result<Address, AddressError> {
        let Some(uri) = payment_uri(SPEC.uri_scheme, value) else {
            return CODEC.validate(value.trim());
        };
        let unsupported = AddressError::UnsupportedRequest { chain: CHAIN };
        if uri
            .params()
            .any(|(name, _)| !matches!(name.as_ref(), "address" | "amount" | "label" | "message"))
        {
            return Err(unsupported);
        }
        let mut addresses =
            uri.params().filter_map(|(name, address)| (name == "address").then_some(address));
        let target = match (uri.target.is_empty(), addresses.next(), addresses.next()) {
            (true, Some(address), None) => address.trim(),
            (false, None, None) => uri.target,
            _ => return Err(unsupported),
        };
        if target.is_empty() {
            return Err(unsupported);
        }
        CODEC.validate(target)
    }

    fn address(&self, key: &KeyMaterial, script: Option<ScriptKind>) -> Result<String, KeyError> {
        match script {
            Some(ScriptKind::P2pkh) => {
                pubkey_address(&Secp256k1Key::from_key_material(key)?.public_compressed())
            }
            Some(
                script @ (ScriptKind::P2tr
                | ScriptKind::P2wpkh
                | ScriptKind::P2shP2wpkh
                | ScriptKind::TonV5r1
                | ScriptKind::TonV4r2
                | ScriptKind::TonV3r2),
            ) => Err(KeyError::ScriptUnsupported { chain: CHAIN, script }),
            None => Err(KeyError::internal("script kind unresolved")),
        }
    }

    fn recognize_key(&self, input: &str) -> Option<&'static str> {
        if is_secret_hex(input) {
            return Some(HEX_KEY_FORMAT);
        }
        (wif_version(input) == Some(WIF_VERSION)).then_some(WIF_FORMAT)
    }

    fn import_key(
        &self,
        input: &str,
        _password: Option<&SecretText>,
    ) -> Result<ImportedKey, KeyError> {
        let key = if let Some(secret) = decode_secret_hex(input) {
            Secp256k1Key::from_secret_bytes(secret.as_slice())?
        } else {
            let wif = wif_decode(input)?;
            if wif.version != WIF_VERSION {
                return Err(KeyError::UnknownFormat);
            }
            // Addresses here come from the compressed public key, but an uncompressed WIF's
            // funds sit at the uncompressed key's address.
            if !wif.is_compressed {
                return Err(KeyError::UncompressedKey);
            }
            wif.key
        };
        Ok(ImportedKey { material: key.key_material(), named_script: None })
    }

    fn export_key(&self, key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
        Ok(Secp256k1Key::from_key_material(key)?
            .with_secret_bytes(|secret| wif_encode(secret, WIF_VERSION)))
    }
}
