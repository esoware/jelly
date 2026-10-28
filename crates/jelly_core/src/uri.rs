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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_splits_into_its_target_and_query() {
        let uri = payment_uri("bitcoin", " BITCOIN:bc1q?amount=1&label=x ").expect("a uri");
        assert_eq!(uri.target, "bc1q");
        assert_eq!(uri.query, "amount=1&label=x");
        assert_eq!(uri.param("label"), Some("x"));
        assert_eq!(uri.param("memo"), None);
        assert_eq!(uri.params().count(), 2);
    }

    #[test]
    fn a_double_slash_and_a_missing_query_are_both_fine() {
        let uri = payment_uri("ethereum", "ethereum://0xabc").expect("a uri");
        assert_eq!(uri, PaymentUri { target: "0xabc", query: "" });
        assert_eq!(uri.params().count(), 0);
    }

    #[test]
    fn another_scheme_or_no_scheme_is_not_a_uri() {
        assert_eq!(payment_uri("bitcoin", "litecoin:ltc1q"), None);
        assert_eq!(payment_uri("bitcoin", "bc1q"), None);
    }

    #[test]
    fn parameter_names_are_percent_decoded_and_values_are_not() {
        let uri = payment_uri("solana", "solana:abc?%6Demo=hi%20there").expect("a uri");
        assert_eq!(uri.param("memo"), Some("hi%20there"));
    }
}
