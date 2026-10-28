//! The binary encoding of a vault's decrypted entries, the plaintext inside the AEAD.
//!
//! ```text
//! payload := version:u8 = 1, count:u32le, entry * count
//! entry   := id:[u8; 16],
//!            kind:     u8 len, utf-8 bytes   (BlobKind::as_str)
//!            key_kind: u8 len, utf-8 bytes   (KeyKind::as_str, empty when absent)
//!            secret:   u32le len, bytes
//! ```
//!
//! Entries are written in id order, so an unchanged vault re-encodes to the same bytes.
//!
//! [`decode`] reads back anything [`encode`] writes. The entry count and the secret length
//! are the only limits, and both sides use the same `u32`, because a decoder cap the
//! encoder didn't enforce would turn a large wallet into a vault that seals but never
//! opens again. Nothing else bounds the input: `decode` only runs on plaintext the AEAD
//! has authenticated, and every read is checked against the bytes present.
//!
//! It's binary and not JSON because JSON would render each secret as hex or base64 text,
//! leaving an unwipeable `String` copy on both the encode and the decode path. Here a
//! secret is copied exactly twice, into and out of a buffer that zeroizes.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use jelly_core::KeyKind;
use zeroize::Zeroizing;

use crate::entry::{BlobContent, BlobKind, EntryId, SecretBlob};
use crate::error::VaultError;
use crate::keys::TAG_LEN;

/// Encoding version of the plaintext, independent of the file format version
/// that wraps it.
const PAYLOAD_VERSION: u8 = 1;

/// The wiping buffer is sized once for the plaintext plus the AEAD tag. It never grows,
/// so no reallocation can strand a copy of a secret in freed heap.
pub(crate) fn encode(
    entries: &BTreeMap<EntryId, SecretBlob>,
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let plaintext_length = encoded_len(entries)?;
    let capacity = add(plaintext_length, TAG_LEN)?;
    let mut encoded = Zeroizing::new(Vec::with_capacity(capacity));

    encoded.push(PAYLOAD_VERSION);
    let count =
        u32::try_from(entries.len()).map_err(|_| VaultError::internal("vault entry count"))?;
    encoded.extend_from_slice(&count.to_le_bytes());
    for (id, blob) in entries {
        encoded.extend_from_slice(id.as_bytes());
        push_label(&mut encoded, blob.kind().as_str())?;
        push_label(&mut encoded, blob.key_kind().map_or("", KeyKind::as_str))?;
        let length =
            u32::try_from(blob.len()).map_err(|_| VaultError::internal("vault secret length"))?;
        encoded.extend_from_slice(&length.to_le_bytes());
        blob.with_bytes(|bytes| encoded.extend_from_slice(bytes));
    }

    if encoded.len() == plaintext_length {
        Ok(encoded)
    } else {
        Err(VaultError::internal("vault payload length"))
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<EntryId, SecretBlob>, VaultError> {
    let mut reader = Reader::new(bytes);
    let version = reader.u8()?;
    if version != PAYLOAD_VERSION {
        return Err(VaultError::UnsupportedPayloadVersion {
            found: version,
            supported: PAYLOAD_VERSION,
        });
    }
    let count = reader.u32()?;
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let id = EntryId::from_bytes(reader.array()?);
        let kind = BlobKind::parse(reader.label()?)
            .ok_or_else(|| VaultError::corrupt("the payload names an unknown entry kind"))?;
        let key_kind = match reader.label()? {
            "" => None,
            text => Some(
                KeyKind::parse(text)
                    .ok_or_else(|| VaultError::corrupt("the payload names an unknown key kind"))?,
            ),
        };
        let content = BlobContent::from_parts(kind, key_kind).ok_or_else(|| {
            VaultError::corrupt("the payload pairs an entry kind with the wrong key kind")
        })?;
        let length = reader.u32()?;
        let secret = reader.take(usize::try_from(length).unwrap_or(usize::MAX))?;
        let Entry::Vacant(slot) = entries.entry(id) else {
            return Err(VaultError::corrupt("the payload holds a duplicate entry id"));
        };
        slot.insert(SecretBlob::new(content, Zeroizing::new(secret.to_vec())));
    }
    if reader.is_empty() {
        Ok(entries)
    } else {
        Err(VaultError::corrupt("the payload has trailing bytes"))
    }
}

fn encoded_len(entries: &BTreeMap<EntryId, SecretBlob>) -> Result<usize, VaultError> {
    let mut total = add(size_of::<u8>(), size_of::<u32>())?;
    for blob in entries.values() {
        total = add(total, EntryId::LEN)?;
        total = add(total, add(size_of::<u8>(), blob.kind().as_str().len())?)?;
        total = add(
            total,
            add(size_of::<u8>(), blob.key_kind().map_or(0, |kind| kind.as_str().len()))?,
        )?;
        total = add(total, add(size_of::<u32>(), blob.len())?)?;
    }
    Ok(total)
}

fn add(a: usize, b: usize) -> Result<usize, VaultError> {
    a.checked_add(b).ok_or_else(|| VaultError::internal("vault payload size"))
}

fn push_label(bytes: &mut Vec<u8>, text: &str) -> Result<(), VaultError> {
    let length =
        u8::try_from(text.len()).map_err(|_| VaultError::internal("vault payload label length"))?;
    bytes.push(length);
    bytes.extend_from_slice(text.as_bytes());
    Ok(())
}

/// A cursor that hands out sub-slices or fails, so the decoder never indexes.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], VaultError> {
        let (head, tail) = self
            .bytes
            .split_at_checked(count)
            .ok_or_else(|| VaultError::corrupt("the payload ends in the middle of an entry"))?;
        self.bytes = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], VaultError> {
        let (head, tail) = self
            .bytes
            .split_first_chunk::<N>()
            .ok_or_else(|| VaultError::corrupt("the payload ends in the middle of an entry"))?;
        self.bytes = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, VaultError> {
        self.array().map(u8::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, VaultError> {
        self.array().map(u32::from_le_bytes)
    }

    fn label(&mut self) -> Result<&'a str, VaultError> {
        let length = usize::from(self.u8()?);
        str::from_utf8(self.take(length)?)
            .map_err(|_| VaultError::corrupt("a payload label isn't utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(content: BlobContent, byte: u8, length: usize) -> SecretBlob {
        SecretBlob::new(content, Zeroizing::new(vec![byte; length]))
    }

    fn sample() -> BTreeMap<EntryId, SecretBlob> {
        BTreeMap::from([
            (EntryId::from_bytes([1u8; 16]), blob(BlobContent::SeedEntropy, 0x11, 32)),
            (
                EntryId::from_bytes([2u8; 16]),
                blob(BlobContent::PrivateKey(KeyKind::Ed25519Seed), 0x22, 32),
            ),
            (EntryId::from_bytes([3u8; 16]), blob(BlobContent::MoneroScanState, 0, 0)),
        ])
    }

    #[test]
    fn a_seed_a_private_key_and_an_empty_secret_round_trip() {
        let encoded = encode(&sample()).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(
            decoded[&EntryId::from_bytes([2u8; 16])].content(),
            BlobContent::PrivateKey(KeyKind::Ed25519Seed)
        );
        decoded[&EntryId::from_bytes([1u8; 16])].with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
        assert_eq!(decoded[&EntryId::from_bytes([3u8; 16])].len(), 0);
    }

    #[test]
    fn an_empty_payload_round_trips() {
        let encoded = encode(&BTreeMap::new()).unwrap();
        assert_eq!(encoded.len(), 5);
        assert!(decode(&encoded).unwrap().is_empty());
    }

    #[test]
    fn the_buffer_is_sized_once_with_room_for_the_tag() {
        let encoded = encode(&sample()).unwrap();
        assert_eq!(encoded.capacity(), encoded.len().saturating_add(TAG_LEN));
    }

    #[test]
    fn entries_are_encoded_in_id_order() {
        let encoded = encode(&sample()).unwrap();
        assert_eq!(encoded.get(5..21).unwrap(), &[1u8; 16]);
        // The first entry's id is followed by its kind label, an empty key kind label, the
        // secret length, and the 32-byte secret.
        let second_at = 21usize
            .saturating_add(1)
            .saturating_add(BlobKind::SeedEntropy.as_str().len())
            .saturating_add(1)
            .saturating_add(4)
            .saturating_add(32);
        assert_eq!(encoded.get(second_at..second_at.saturating_add(16)).unwrap(), &[2u8; 16]);
    }

    #[test]
    fn truncation_and_trailing_bytes_are_corruption() {
        let encoded = encode(&sample()).unwrap();
        for cut in [0usize, 1, 4, 6, 20, encoded.len().saturating_sub(1)] {
            assert!(matches!(decode(encoded.get(..cut).unwrap()), Err(VaultError::Corrupt { .. })));
        }
        let mut longer = encoded.to_vec();
        longer.push(0);
        assert!(matches!(decode(&longer), Err(VaultError::Corrupt { .. })));
    }

    #[test]
    fn an_unsupported_payload_version_is_distinct_from_a_corrupt_label() {
        let mut encoded = encode(&sample()).unwrap().to_vec();
        *encoded.first_mut().unwrap() = 9;
        assert!(matches!(
            decode(&encoded),
            Err(VaultError::UnsupportedPayloadVersion { found: 9, supported: PAYLOAD_VERSION })
        ));

        let mut encoded = encode(&sample()).unwrap().to_vec();
        // The first label's text follows the version, the count, the 16-byte id, and its
        // length byte.
        encoded[22] = b'X';
        assert!(matches!(decode(&encoded), Err(VaultError::Corrupt { .. })));
    }

    #[test]
    fn a_duplicate_id_is_corruption() {
        let entries = BTreeMap::from([(
            EntryId::from_bytes([1u8; 16]),
            blob(BlobContent::SeedEntropy, 0x11, 4),
        )]);
        let encoded = encode(&entries).unwrap();
        let mut doubled = encoded.to_vec();
        doubled[1] = 2;
        doubled.extend_from_slice(encoded.get(5..).unwrap());
        assert!(matches!(decode(&doubled), Err(VaultError::Corrupt { .. })));
    }

    /// A count with nothing behind it fails at the first missing entry, without
    /// allocating for the count first.
    #[test]
    fn an_absurd_count_is_refused_at_the_first_missing_byte() {
        let mut bytes = vec![PAYLOAD_VERSION];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode(&bytes), Err(VaultError::Corrupt { .. })));
    }

    #[test]
    fn a_payload_larger_than_any_wallet_round_trips() {
        let mut entries = BTreeMap::from([(
            EntryId::from_bytes([1u8; 16]),
            blob(BlobContent::MoneroScanState, 0x5a, 4 << 20),
        )]);
        for i in 0..1000u16 {
            let mut id = [0u8; 16];
            id.get_mut(..2).unwrap().copy_from_slice(&i.to_le_bytes());
            entries.insert(
                EntryId::from_bytes(id),
                blob(BlobContent::PrivateKey(KeyKind::Secp256k1), 0x11, 32),
            );
        }
        let encoded = encode(&entries).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), entries.len());
        decoded[&EntryId::from_bytes([1u8; 16])]
            .with_bytes(|bytes| assert_eq!(bytes.len(), 4 << 20));
    }

    #[test]
    fn a_private_key_without_a_key_kind_is_refused() {
        let entries = BTreeMap::from([(
            EntryId::from_bytes([7u8; 16]),
            blob(BlobContent::PrivateKey(KeyKind::Secp256k1), 5, 4),
        )]);
        let mut encoded = encode(&entries).unwrap().to_vec();
        // Empty the key kind label, which follows the 16-byte id and the length-prefixed
        // kind label.
        let key_kind_at = 22usize.saturating_add(BlobKind::PrivateKey.as_str().len());
        let tail = encoded.split_off(key_kind_at);
        let secret = tail[1usize.saturating_add(KeyKind::Secp256k1.as_str().len())..].to_vec();
        encoded.push(0);
        encoded.extend_from_slice(&secret);
        assert!(matches!(decode(&encoded), Err(VaultError::Corrupt { .. })));
    }
}
