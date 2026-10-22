use zeroize::Zeroizing;

/// What key recognition and a [`jelly_core::KeyError::WrongChain`] call a bare hex key.
pub const HEX_KEY_FORMAT: &str = "32-byte hex key";

/// Whether `input` has the shape of thirty-two bytes of hex, with or without a `0x`
/// prefix, read without decoding the secret.
pub fn is_secret_hex(input: &str) -> bool {
    let body = hex_body(input);
    body.len() == 64 && body.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Thirty-two bytes of hex, with or without a `0x` prefix, or `None` when the text is
/// anything else.
pub fn decode_secret_hex(input: &str) -> Option<Zeroizing<[u8; 32]>> {
    if !is_secret_hex(input) {
        return None;
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(hex_body(input), bytes.as_mut_slice()).ok()?;
    Some(bytes)
}

/// Lowercase hex without a prefix, in a buffer that wipes on drop.
pub fn encode_secret_hex(bytes: &[u8; 32]) -> Zeroizing<String> {
    Zeroizing::new(hex::encode(bytes))
}

/// The trimmed text without a `0x` or `0X` prefix.
pub(crate) fn hex_body(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")).unwrap_or(trimmed)
}
