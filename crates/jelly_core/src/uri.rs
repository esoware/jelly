use std::borrow::Cow;

use percent_encoding::percent_decode_str;

/// The parts of a payment URI after its scheme.
///
/// BIP-21 for the UTXO chains, EIP-681 for the EVM networks, Solana Pay, ZIP-321 for
/// Zcash, and the `tron:` and `monero:` forms all put the address after the scheme and
/// the parameters after a `?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaymentUri<'a> {
    /// The text between the scheme and the `?`, usually the address, trimmed.
    pub target: &'a str,
    /// The raw query after the `?`, empty when there is none.
    pub query: &'a str,
}

impl PaymentUri<'_> {
    /// The query's parameters, with names percent-decoded so `%6Demo` reads as `memo`.
    ///
    /// Values stay as written. The only values anything reads are addresses, which have no
    /// character to encode, so an encoded one fails validation.
    pub fn params(&self) -> impl Iterator<Item = (Cow<'_, str>, &str)> {
        self.query.split('&').filter(|param| !param.is_empty()).map(|param| {
            let (name, value) = param.split_once('=').unwrap_or((param, ""));
            (percent_decode_str(name).decode_utf8_lossy(), value)
        })
    }

    /// The value of the first parameter named `name`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params().find_map(|(key, value)| (key == name).then_some(value))
    }
}

/// Splits `value` as a payment URI of `scheme`, or `None` when it isn't one. The scheme
/// match ignores case, and a `//` after it is dropped.
pub fn payment_uri<'a>(scheme: &str, value: &'a str) -> Option<PaymentUri<'a>> {
    let (found, rest) = value.trim().split_once(':')?;
    if !found.eq_ignore_ascii_case(scheme) {
        return None;
    }
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
    Some(PaymentUri { target: target.trim(), query })
}
