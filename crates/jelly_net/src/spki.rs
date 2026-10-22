use sha2::{Digest as _, Sha256};

const SEQUENCE: u8 = 0x30;
const BIT_STRING: u8 = 0x03;

/// SHA-256 over the DER encoding of a certificate's `SubjectPublicKeyInfo`,
/// header included.
///
/// Every pinning implementation hashes this element, including HPKP, `OkHttp`, and
/// `curl --pinnedpubkey`.
pub(crate) fn spki_sha256(certificate: &[u8]) -> Option<[u8; 32]> {
    subject_public_key_info(certificate).map(|spki| Sha256::digest(spki).into())
}

/// The DER `SubjectPublicKeyInfo` element of a certificate, header included.
///
/// webpki's parser refuses v1 and some self-signed certificates that public Electrum
/// servers present, so the key is found by this walk instead. A version 1 certificate has
/// no version tag, and the walk reaches its key all the same.
///
/// ```text
/// Certificate     ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
/// TBSCertificate  ::= SEQUENCE { [0] version OPTIONAL, serialNumber, signature,
///                                issuer, validity, subject, subjectPublicKeyInfo, ... }
/// ```
pub(crate) fn subject_public_key_info(certificate: &[u8]) -> Option<&[u8]> {
    // The outermost SEQUENCE's first child is the `TBSCertificate`.
    let mut rest = content(content(certificate)?)?;
    // The optional explicit [0] version tag, then five fields before the key.
    if rest.first() == Some(&0xA0) {
        rest = skip(rest)?;
    }
    for _ in 0..5u8 {
        rest = skip(rest)?;
    }
    element(rest)
}

/// The content of a `SubjectPublicKeyInfo`'s algorithm identifier and its key bits, the
/// two halves a signature algorithm reads.
///
/// ```text
/// SubjectPublicKeyInfo ::= SEQUENCE { algorithm AlgorithmIdentifier,
///                                     subjectPublicKey BIT STRING }
/// ```
pub(crate) fn public_key_parts(spki: &[u8]) -> Option<(&[u8], &[u8])> {
    if spki.first() != Some(&SEQUENCE) {
        return None;
    }
    let fields = content(spki)?;
    if fields.first() != Some(&SEQUENCE) {
        return None;
    }
    let algorithm = content(fields)?;
    let bits = skip(fields)?;
    if bits.first() != Some(&BIT_STRING) || !skip(bits)?.is_empty() {
        return None;
    }
    // A key is a whole number of bytes, so its leading unused-bit count is zero.
    let (&unused_bits, key) = content(bits)?.split_first()?;
    (unused_bits == 0).then_some((algorithm, key))
}

/// The full element at the head of `input`, tag and length bytes included.
fn element(input: &[u8]) -> Option<&[u8]> {
    let (header, length) = header(input)?;
    input.get(..header.checked_add(length)?)
}

/// The value bytes of the element at the head of `input`.
fn content(input: &[u8]) -> Option<&[u8]> {
    let (header, length) = header(input)?;
    input.get(header..header.checked_add(length)?)
}

/// `input` with its leading element removed.
fn skip(input: &[u8]) -> Option<&[u8]> {
    let (header, length) = header(input)?;
    input.get(header.checked_add(length)?..)
}

/// The header length in bytes and the content length it declares. Definite
/// form only, at most four length bytes, which covers any real certificate.
fn header(input: &[u8]) -> Option<(usize, usize)> {
    let first_length_byte = *input.get(1)?;
    if first_length_byte < 0x80 {
        return Some((2, usize::from(first_length_byte)));
    }
    let count = usize::from(first_length_byte.checked_sub(0x80)?);
    if count == 0 || count > 4 {
        return None;
    }
    let bytes = input.get(2..2usize.checked_add(count)?)?;
    let mut length = 0usize;
    for byte in bytes {
        length = length.checked_mul(256)?.checked_add(usize::from(*byte))?;
    }
    Some((2usize.checked_add(count)?, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal DER certificate whose every field but the key is a two-byte NULL.
    ///
    /// The shape is `SEQUENCE { SEQUENCE { serial, signature, issuer, validity, subject,
    /// spki }, alg, sig }`, so the walk is the only thing under test.
    fn certificate(spki: &[u8], has_version_tag: bool) -> Vec<u8> {
        let mut tbs = Vec::new();
        if has_version_tag {
            tbs.extend_from_slice(&[0xA0, 0x03, 0x02, 0x01, 0x02]);
        }
        for _ in 0..5 {
            tbs.extend_from_slice(&[0x05, 0x00]);
        }
        tbs.extend_from_slice(spki);
        let mut inner = vec![0x30, u8::try_from(tbs.len()).expect("short")];
        inner.extend_from_slice(&tbs);
        inner.extend_from_slice(&[0x05, 0x00, 0x05, 0x00]);
        let mut outer = vec![0x30, u8::try_from(inner.len()).expect("short")];
        outer.extend_from_slice(&inner);
        outer
    }

    #[test]
    fn the_walk_reaches_the_key_with_or_without_a_version_tag() {
        let spki = [0x30, 0x03, 0x02, 0x01, 0x07];
        let expected: [u8; 32] = Sha256::digest(spki).into();
        for has_version_tag in [false, true] {
            assert_eq!(spki_sha256(&certificate(&spki, has_version_tag)), Some(expected));
        }
    }

    #[test]
    fn the_digest_covers_the_element_and_not_just_its_content() {
        let spki = [0x30, 0x03, 0x02, 0x01, 0x07];
        let content_only: [u8; 32] = Sha256::digest([0x02, 0x01, 0x07]).into();
        assert_ne!(spki_sha256(&certificate(&spki, false)), Some(content_only));
    }

    #[test]
    fn a_key_splits_into_its_algorithm_and_its_bits() {
        let spki = [0x30, 0x08, 0x30, 0x02, 0x06, 0x00, 0x03, 0x02, 0x00, 0x07];
        assert_eq!(public_key_parts(&spki), Some(([0x06, 0x00].as_slice(), [0x07].as_slice())));

        for malformed in [
            // Unused bits in the key.
            vec![0x30, 0x08, 0x30, 0x02, 0x06, 0x00, 0x03, 0x02, 0x01, 0x07],
            // An octet string where the bit string belongs.
            vec![0x30, 0x08, 0x30, 0x02, 0x06, 0x00, 0x04, 0x02, 0x00, 0x07],
            // A field after the key.
            vec![0x30, 0x0a, 0x30, 0x02, 0x06, 0x00, 0x03, 0x02, 0x00, 0x07, 0x05, 0x00],
            // No algorithm identifier.
            vec![0x30, 0x04, 0x03, 0x02, 0x00, 0x07],
        ] {
            assert_eq!(public_key_parts(&malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn malformed_der_is_rejected_rather_than_guessed_at() {
        for input in [
            vec![],
            vec![0x30],
            vec![0x30, 0x82, 0x01],
            vec![0x30, 0x02, 0x30, 0x7f],
            // A length claiming five bytes of length, which no certificate uses.
            vec![0x30, 0x85, 0x01, 0x02, 0x03, 0x04, 0x05],
        ] {
            assert_eq!(spki_sha256(&input), None, "{input:?}");
        }
    }
}
