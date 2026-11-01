use std::fmt;

use jelly_core::{TxPreview, TxWarning};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use sha2::{Digest as _, Sha256};

/// Domain string of the preview digest. Every field after it is length-prefixed, so no
/// two previews frame to the same bytes.
const PREVIEW_DOMAIN: &[u8] = b"jelly-engine/v1/tx-preview";

/// The digest a submit is bound to.
///
/// The interface echoes the digest of the preview the user approved, and a submit that
/// carries a different one is refused, so what was shown is what gets signed. It isn't a
/// secret, so an ordinary comparison is fine.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PreviewDigest(#[cfg_attr(feature = "ipc", specta(type = String))] [u8; Self::LEN]);

impl PreviewDigest {
    pub const LEN: usize = 32;

    /// The digest of `preview` as the user sees it, with `fee_warning` after its own
    /// warnings.
    pub(crate) fn of(preview: &TxPreview, fee_warning: Option<&TxWarning>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(PREVIEW_DOMAIN);
        field(&mut hasher, preview.chain.as_str().as_bytes());
        field(&mut hasher, preview.asset.key().as_bytes());
        count(&mut hasher, preview.from.len());
        for source in &preview.from {
            field(&mut hasher, source.as_bytes());
        }
        field(&mut hasher, preview.to.as_bytes());
        field(&mut hasher, &preview.amount.base_units().to_be_bytes());
        field(&mut hasher, &preview.fee.base_units().to_be_bytes());
        count(&mut hasher, preview.details.len());
        for detail in &preview.details {
            field(&mut hasher, detail.label.as_bytes());
            field(&mut hasher, detail.value.as_bytes());
        }
        count(
            &mut hasher,
            preview.warnings.len().saturating_add(usize::from(fee_warning.is_some())),
        );
        for warning in preview.warnings.iter().chain(fee_warning) {
            field(&mut hasher, warning.kind.as_str().as_bytes());
            field(&mut hasher, warning.message.as_bytes());
        }
        // Last, and only when present. Everything before them is self-delimiting and the
        // two differ in width, so which ones are present is never ambiguous.
        if let Some(rent) = preview.rent {
            field(&mut hasher, &rent.base_units().to_be_bytes());
        }
        if let Some(tag) = preview.destination_tag {
            field(&mut hasher, &tag.to_be_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// The exact inverse of [`Display`](fmt::Display), two lowercase hex digits per
    /// byte. An uppercase or mixed-case echo was never handed out, so it's refused instead
    /// of normalized.
    fn parse(text: &str) -> Option<Self> {
        if !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut bytes = [0u8; Self::LEN];
        hex::decode_to_slice(text, &mut bytes).ok()?;
        Some(Self(bytes))
    }
}

impl fmt::Display for PreviewDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for PreviewDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Serialize for PreviewDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PreviewDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| D::Error::custom("expected 64 lowercase hex digits"))
    }
}

fn field(hasher: &mut Sha256, bytes: &[u8]) {
    count(hasher, bytes.len());
    hasher.update(bytes);
}

fn count(hasher: &mut Sha256, value: usize) {
    hasher.update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use jelly_core::{Amount, TxDetail, TxWarningKind};
    use serde::de::value::{Error as ValueError, StrDeserializer};

    use super::*;
    use crate::fixtures::preview;

    /// A change to the framing has to fail here first.
    #[test]
    fn the_digest_is_frozen() {
        let digest = PreviewDigest::of(&preview(), None);
        assert_eq!(
            digest.to_string(),
            "b90d2746b3961cd04fe803bb3f99282e7082a223f67366aff29c1f8446055440"
        );
    }

    #[test]
    fn a_digest_round_trips_through_its_text() {
        let digest = PreviewDigest::of(&preview(), None);
        assert_eq!(PreviewDigest::parse(&digest.to_string()), Some(digest));
        assert_eq!(PreviewDigest::parse("not-a-digest"), None);
        assert_eq!(PreviewDigest::parse(""), None);
    }

    #[test]
    fn only_the_lowercase_spelling_is_a_digest() {
        let text = PreviewDigest::of(&preview(), None).to_string();
        assert!(PreviewDigest::parse(&text).is_some());
        assert_eq!(PreviewDigest::parse(&text.to_uppercase()), None);
        let mixed = text.replacen('a', "A", 1);
        assert_ne!(mixed, text, "the fixed digest has to contain an 'a' to mix");
        assert_eq!(PreviewDigest::parse(&mixed), None);
        // The same contract on the wire, whose error message already promises it.
        deserialize(&text).unwrap();
        deserialize(&text.to_uppercase()).unwrap_err();
    }

    /// Deserializes a digest the way the interface's echo arrives.
    fn deserialize(text: &str) -> Result<PreviewDigest, ValueError> {
        PreviewDigest::deserialize(StrDeserializer::<ValueError>::new(text))
    }

    #[test]
    fn every_field_of_a_preview_changes_its_digest() {
        let base = PreviewDigest::of(&preview(), None);

        let mut amount = preview();
        amount.amount = Amount::from_base_units(10_001);
        assert_ne!(PreviewDigest::of(&amount, None), base);

        let mut fee = preview();
        fee.fee = Amount::from_base_units(251);
        assert_ne!(PreviewDigest::of(&fee, None), base);

        let mut to = preview();
        to.to = String::from("bc1qelse");
        assert_ne!(PreviewDigest::of(&to, None), base);

        let mut source = preview();
        source.from.push(String::from("bc1qsecond"));
        assert_ne!(PreviewDigest::of(&source, None), base);

        let mut order = source.clone();
        order.from.reverse();
        assert_ne!(PreviewDigest::of(&order, None), PreviewDigest::of(&source, None));

        let mut detail = preview();
        detail.details.push(TxDetail::new("Fee rate", "1 sat/vB"));
        assert_ne!(PreviewDigest::of(&detail, None), base);

        let mut warning = preview();
        warning.warnings.push(TxWarning::new(TxWarningKind::DustOutput, "dust"));
        assert_ne!(PreviewDigest::of(&warning, None), base);

        let mut tagged = preview();
        tagged.destination_tag = Some(0);
        assert_ne!(PreviewDigest::of(&tagged, None), base);
        let mut retagged = preview();
        retagged.destination_tag = Some(1);
        assert_ne!(PreviewDigest::of(&retagged, None), PreviewDigest::of(&tagged, None));

        let mut rented = preview();
        rented.rent = Some(Amount::ZERO);
        assert_ne!(PreviewDigest::of(&rented, None), base);
        let mut rerented = preview();
        rerented.rent = Some(Amount::from_base_units(1));
        assert_ne!(PreviewDigest::of(&rerented, None), PreviewDigest::of(&rented, None));
        let mut both = tagged.clone();
        both.rent = Some(Amount::ZERO);
        assert_ne!(PreviewDigest::of(&both, None), PreviewDigest::of(&tagged, None));
        assert_ne!(PreviewDigest::of(&both, None), PreviewDigest::of(&rented, None));
    }

    /// Without length prefixing these previews would frame to the same bytes.
    #[test]
    fn adjacent_fields_cannot_be_confused() {
        let mut first = preview();
        first.from = vec![String::from("aa")];
        first.to = String::from("bb");
        let mut second = preview();
        second.from = vec![String::from("aabb")];
        second.to = String::new();
        assert_ne!(PreviewDigest::of(&first, None), PreviewDigest::of(&second, None));

        let mut split = preview();
        split.from = vec![String::from("aa"), String::from("bb")];
        let mut joined = preview();
        joined.from = vec![String::from("aabb")];
        assert_ne!(PreviewDigest::of(&split, None), PreviewDigest::of(&joined, None));
    }
}
