use jelly_core::{ImportedKey, KeyError, ScriptKind};
use jelly_keypair::{
    HEX_KEY_FORMAT, Secp256k1Key, decode_secret_hex, is_secret_hex, wif_decode, wif_encode,
    wif_version,
};
use zeroize::Zeroizing;

/// The script types Electrum writes before a WIF, with a colon after them.
const ELECTRUM_PREFIXES: [(&str, ScriptKind); 4] = [
    ("p2pkh", ScriptKind::P2pkh),
    ("p2wpkh", ScriptKind::P2wpkh),
    ("p2wpkh-p2sh", ScriptKind::P2shP2wpkh),
    ("p2tr", ScriptKind::P2tr),
];

/// How a chain writes a private key in wallet import format.
#[derive(Debug, Clone, Copy)]
pub struct WifFormat {
    pub version: u8,
    /// Whether Electrum 3.0 wrote a segwit script type into this version byte, one above
    /// it for P2WPKH and two above it for P2SH-P2WPKH. Electrum still reads those keys.
    pub has_script_offsets: bool,
    /// The format's display name, as an error names it, such as `Bitcoin WIF key`.
    pub name: &'static str,
}

impl WifFormat {
    /// The format `input` is written in, read from its shape and version byte without
    /// checking the secret it holds.
    pub(crate) fn recognize(self, input: &str) -> Option<&'static str> {
        let trimmed = input.trim();
        if is_secret_hex(trimmed) {
            return Some(HEX_KEY_FORMAT);
        }
        let (wif, prefix) = split_electrum_prefix(trimmed);
        let named = self.script_of(wif_version(wif)?).ok()?;
        (prefix.is_none() || named.is_none()).then_some(self.name)
    }

    /// Reads 32 bytes of hex, a WIF of this format, or a WIF behind one of Electrum's
    /// script type prefixes, such as `p2wpkh:`.
    pub(crate) fn import(self, input: &str) -> Result<ImportedKey, KeyError> {
        let trimmed = input.trim();
        if let Some(secret) = decode_secret_hex(trimmed) {
            let key = Secp256k1Key::from_secret_bytes(secret.as_slice())?;
            return Ok(ImportedKey { material: key.key_material(), named_script: None });
        }
        let (wif, prefix) = split_electrum_prefix(trimmed);
        let decoded = wif_decode(wif)?;
        let named = self.script_of(decoded.version)?;
        // Electrum reads the script type from the version byte only when there is no
        // prefix.
        if prefix.is_some() && named.is_some() {
            return Err(KeyError::UnknownFormat);
        }
        // A key's addresses all come from its compressed public key, and an
        // uncompressed WIF's funds sit at the uncompressed key's address.
        if !decoded.is_compressed {
            return Err(KeyError::UncompressedKey);
        }
        Ok(ImportedKey { material: decoded.key.key_material(), named_script: prefix.or(named) })
    }

    /// The key as a compressed WIF of this format.
    pub(crate) fn export(self, key: &Secp256k1Key) -> Zeroizing<String> {
        key.with_secret_bytes(|bytes| wif_encode(bytes, self.version))
    }

    /// The script type `version` carries, or [`KeyError::UnknownFormat`] when the byte
    /// isn't this format's.
    const fn script_of(self, version: u8) -> Result<Option<ScriptKind>, KeyError> {
        match version.checked_sub(self.version) {
            Some(0) => Ok(None),
            Some(1) if self.has_script_offsets => Ok(Some(ScriptKind::P2wpkh)),
            Some(2) if self.has_script_offsets => Ok(Some(ScriptKind::P2shP2wpkh)),
            Some(_) | None => Err(KeyError::UnknownFormat),
        }
    }
}

/// The WIF after an Electrum script type prefix, and the type it names. A colon after
/// anything else, such as the list number in `1: abandon`, isn't that form, and the
/// text comes back whole.
fn split_electrum_prefix(text: &str) -> (&str, Option<ScriptKind>) {
    let Some((prefix, wif)) = text.split_once(':') else {
        return (text, None);
    };
    ELECTRUM_PREFIXES
        .into_iter()
        .find_map(|(name, script)| prefix.trim().eq_ignore_ascii_case(name).then_some(script))
        .map_or((text, None), |script| (wif, Some(script)))
}
