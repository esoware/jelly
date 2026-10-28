use jelly_core::{KeyError, KeyKind, KeyMaterial};
use jelly_keypair::{Ed25519Key, HEX_KEY_FORMAT, decode_secret_hex};
use zeroize::Zeroizing;

const SEED_BYTES: usize = 32;
const KEYPAIR_BYTES: usize = 64;

/// The private-key encodings Solana accepts on import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyFormat {
    /// Thirty-two bytes of hex, with or without the `0x` prefix.
    Hex32,
    /// Base58 of the 64-byte `secret || public` keypair, or of the bare 32-byte
    /// secret.
    KeypairBase58,
    /// The 64-byte keypair as hex, with or without the `0x` prefix.
    KeypairHex,
    /// The keypair or the bare secret as a JSON byte array, which is what
    /// solana-keygen writes.
    KeypairJson,
}

impl KeyFormat {
    /// What key recognition and a wrong-chain error call this format.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Hex32 => HEX_KEY_FORMAT,
            Self::KeypairBase58 | Self::KeypairHex => "Solana keypair",
            Self::KeypairJson => "Solana keypair file",
        }
    }
}

/// Key bytes as an encoding wrote them, before the public half is checked.
struct Decoded {
    format: KeyFormat,
    raw: Zeroizing<[u8; KEYPAIR_BYTES]>,
    count: usize,
}

impl Decoded {
    fn bytes(&self) -> &[u8] {
        self.raw.get(..self.count).unwrap_or_default()
    }

    /// Whether the bytes have the length of a bare secret or of a keypair.
    const fn has_key_length(&self) -> bool {
        matches!(self.count, SEED_BYTES | KEYPAIR_BYTES)
    }
}

/// The format `input` is written in, read from its shape and length. The public half
/// of a keypair isn't checked.
pub(crate) fn recognize(input: &str) -> Option<KeyFormat> {
    decode(input).filter(Decoded::has_key_length).map(|decoded| decoded.format)
}

pub(crate) fn import(input: &str) -> Result<KeyMaterial, KeyError> {
    let decoded = decode(input).ok_or(KeyError::UnknownFormat)?;
    let seed = keypair_seed(decoded.bytes())?;
    Ok(KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(seed.to_vec())))
}

/// Base58 of `secret || public`, which is what Phantom exports.
pub(crate) fn export(key: &KeyMaterial) -> Result<Zeroizing<String>, KeyError> {
    let keypair = Ed25519Key::from_key_material(key)?.to_keypair_bytes();
    Ok(Zeroizing::new(bs58::encode(keypair.as_slice()).into_string()))
}

/// Order matters. A JSON array is unambiguous, and hex is tried before base58 because
/// their alphabets overlap.
fn decode(input: &str) -> Option<Decoded> {
    let trimmed = input.trim();
    if trimmed.starts_with('[') {
        return json(trimmed);
    }
    if let Some(seed) = decode_secret_hex(trimmed) {
        let mut raw = Zeroizing::new([0u8; KEYPAIR_BYTES]);
        raw.get_mut(..SEED_BYTES)?.copy_from_slice(seed.as_slice());
        return Some(Decoded { format: KeyFormat::Hex32, raw, count: SEED_BYTES });
    }
    let body = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")).unwrap_or(trimmed);
    if body.len() == KEYPAIR_BYTES.saturating_mul(2)
        && body.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let mut raw = Zeroizing::new([0u8; KEYPAIR_BYTES]);
        hex::decode_to_slice(body, raw.as_mut_slice()).ok()?;
        return Some(Decoded { format: KeyFormat::KeypairHex, raw, count: KEYPAIR_BYTES });
    }
    let mut raw = Zeroizing::new([0u8; KEYPAIR_BYTES]);
    let count = bs58::decode(trimmed).onto(raw.as_mut_slice()).ok()?;
    Some(Decoded { format: KeyFormat::KeypairBase58, raw, count })
}

/// Parsed by hand, because `serde_json` would read the array into a `Vec<u8>` that
/// holds the key and is never wiped.
fn json(input: &str) -> Option<Decoded> {
    let inner = input.strip_prefix('[')?.strip_suffix(']')?;
    let mut raw = Zeroizing::new([0u8; KEYPAIR_BYTES]);
    let mut count: usize = 0;
    for part in inner.split(',') {
        let number = part.trim();
        if number.is_empty() {
            continue;
        }
        *raw.get_mut(count)? = number.parse::<u8>().ok()?;
        count = count.saturating_add(1);
    }
    Some(Decoded { format: KeyFormat::KeypairJson, raw, count })
}

/// Accepts the 64-byte keypair or a bare 32-byte secret.
///
/// The public half is never trusted. It's recomputed from the secret and the two must
/// agree, because a mismatched pair is an oracle that leaks the private scalar across
/// two signatures.
fn keypair_seed(raw: &[u8]) -> Result<Zeroizing<[u8; SEED_BYTES]>, KeyError> {
    let (secret, public) = raw.split_first_chunk::<SEED_BYTES>().ok_or(KeyError::UnknownFormat)?;
    let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
    seed.copy_from_slice(secret);
    match public.len() {
        0 => Ok(seed),
        SEED_BYTES if Ed25519Key::from_seed_bytes(&seed).public_bytes() == public => Ok(seed),
        SEED_BYTES => Err(KeyError::InvalidSecretKey),
        _ => Err(KeyError::UnknownFormat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; SEED_BYTES] = [7; SEED_BYTES];

    fn keypair_json(bytes: &[u8]) -> String {
        format!("[{}]", bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(","))
    }

    fn material(seed: &[u8]) -> KeyMaterial {
        KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(seed.to_vec()))
    }

    fn imported_seed(input: &str) -> Vec<u8> {
        import(input).expect("import").with_bytes(<[u8]>::to_vec)
    }

    #[test]
    fn every_encoding_of_one_key_imports_the_same_seed() {
        let keypair = Ed25519Key::from_seed_bytes(&SEED).to_keypair_bytes();
        let exported = export(&material(&SEED)).expect("export");
        let hex_keypair = hex::encode(keypair.as_slice());
        for (input, format) in [
            (exported.to_string(), KeyFormat::KeypairBase58),
            (bs58::encode(SEED).into_string(), KeyFormat::KeypairBase58),
            (hex_keypair.clone(), KeyFormat::KeypairHex),
            (format!("0x{}", hex_keypair.to_uppercase()), KeyFormat::KeypairHex),
            (keypair_json(keypair.as_slice()), KeyFormat::KeypairJson),
            (keypair_json(&SEED), KeyFormat::KeypairJson),
            (hex::encode(SEED), KeyFormat::Hex32),
            (format!(" 0x{} ", hex::encode(SEED)), KeyFormat::Hex32),
        ] {
            assert_eq!(recognize(&input), Some(format), "{input}");
            assert_eq!(imported_seed(&input), SEED, "{input}");
        }
        assert_eq!(bs58::decode(exported.as_str()).into_vec().expect("base58").len(), 64);
    }

    #[test]
    fn the_keypair_file_format_is_what_solana_keygen_writes() {
        let keypair = Ed25519Key::from_seed_bytes(&SEED).to_keypair_bytes();
        let json = keypair_json(keypair.as_slice());
        assert_eq!(json.trim_matches(['[', ']']).split(',').count(), 64);
        let spaced =
            format!("[ {} ,]", keypair.iter().map(u8::to_string).collect::<Vec<_>>().join(" , "));
        assert_eq!(imported_seed(&spaced), SEED);
    }

    #[test]
    fn a_keypair_file_with_too_many_bytes_is_refused() {
        for count in [65, 128, 4096] {
            let input = format!("[{}]", vec!["7"; count].join(","));
            assert!(matches!(import(&input), Err(KeyError::UnknownFormat)), "{count}");
            assert_eq!(recognize(&input), None, "{count}");
        }
    }

    #[test]
    fn a_keypair_with_the_wrong_public_half_is_rejected() {
        let mut bytes = Ed25519Key::from_seed_bytes(&SEED).to_keypair_bytes();
        if let Some(last) = bytes.last_mut() {
            *last ^= 0xff;
        }
        let json = keypair_json(bytes.as_slice());
        assert_eq!(recognize(&json), Some(KeyFormat::KeypairJson), "the shape alone is named");
        assert!(matches!(import(&json), Err(KeyError::InvalidSecretKey)));
        let base58 = bs58::encode(bytes.as_slice()).into_string();
        assert!(matches!(import(&base58), Err(KeyError::InvalidSecretKey)));
    }

    #[test]
    fn text_in_no_solana_encoding_is_not_a_key() {
        for input in [
            "",
            "   ",
            "0x1234",
            "[1,2,3]",
            "[1,2,x]",
            "1,2,3]",
            "not base58 !!!",
            "hello world there",
            "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
            r#"{"crypto":{}}"#,
        ] {
            assert_eq!(recognize(input), None, "{input:?}");
            assert!(matches!(import(input), Err(KeyError::UnknownFormat)), "{input:?}");
        }
    }

    #[test]
    fn a_key_of_another_kind_is_not_exported() {
        let secp = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(SEED.to_vec()));
        assert!(matches!(export(&secp), Err(KeyError::InvalidSecretKey)));
    }
}
