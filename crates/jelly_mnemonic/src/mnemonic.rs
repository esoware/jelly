use std::fmt;

use bip39::Language;
use jelly_core::{KeyError, PhraseWords, phrase_words, random_fill};
use jelly_keypair::Seed;
use sha2::Sha512;
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;

/// Fixed by BIP-39 for interoperability, so it must never be raised. The phrase's
/// entropy is what protects the wallet.
const SEED_ROUNDS: u32 = 2048;

/// The BIP-39 salt prefix. The passphrase is appended to it.
const SEED_SALT_PREFIX: &str = "mnemonic";

/// A checksum-validated BIP-39 phrase on the English word list.
///
/// Holds the library's word indices, which its `zeroize` feature wipes on drop. The
/// entropy the vault stores and the phrase are produced on demand into buffers that also
/// wipe on drop.
pub struct Mnemonic(bip39::Mnemonic);

impl Mnemonic {
    pub fn generate(words: PhraseWords) -> Result<Self, KeyError> {
        let entropy_bytes = match words {
            PhraseWords::Twelve => 16,
            PhraseWords::TwentyFour => 32,
        };
        let mut entropy = Zeroizing::new(vec![0u8; entropy_bytes]);
        random_fill(&mut entropy)?;
        Self::from_entropy(&entropy)
    }

    /// Rebuilds a phrase from the entropy the vault stores.
    pub fn from_entropy(entropy: &[u8]) -> Result<Self, KeyError> {
        bip39::Mnemonic::from_entropy(entropy).map(Self).map_err(key_error)
    }

    pub fn parse(phrase: &str) -> Result<Self, KeyError> {
        bip39::Mnemonic::parse_in_normalized(Language::English, &normalize_phrase(phrase))
            .map(Self)
            .map_err(key_error)
    }

    pub fn word_count(&self) -> usize {
        self.0.word_count()
    }

    /// The phrase, single-space separated, in a buffer that wipes on drop.
    pub fn phrase(&self) -> Zeroizing<String> {
        join_words(|| self.0.words().map(str::chars))
    }

    pub fn with_entropy<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        let entropy = Zeroizing::new(self.0.to_entropy_array());
        let (bytes, length) = &*entropy;
        read(bytes.get(..*length).unwrap_or_default())
    }

    /// PBKDF2-HMAC-SHA512 for 2048 rounds over the space-joined phrase, salted with
    /// `mnemonic` and the NFKD-normalized passphrase.
    ///
    /// An empty passphrase means no passphrase. Every passphrase yields a valid seed, so
    /// a wrong one produces a different wallet rather than an error.
    pub fn to_seed(&self, passphrase: &str) -> Seed {
        let phrase = self.phrase();
        let passphrase_bytes: usize = passphrase.nfkd().map(char::len_utf8).sum();
        let mut salt = Zeroizing::new(String::with_capacity(
            SEED_SALT_PREFIX.len().saturating_add(passphrase_bytes),
        ));
        salt.push_str(SEED_SALT_PREFIX);
        salt.extend(passphrase.nfkd());
        let mut seed = Box::new([0u8; Seed::LEN]);
        pbkdf2::pbkdf2_hmac::<Sha512>(
            phrase.as_bytes(),
            salt.as_bytes(),
            SEED_ROUNDS,
            seed.as_mut_slice(),
        );
        Seed::from_boxed(seed)
    }
}

impl fmt::Debug for Mnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mnemonic({} words, <redacted>)", self.word_count())
    }
}

const fn key_error(error: bip39::Error) -> KeyError {
    match error {
        bip39::Error::BadWordCount(got) => KeyError::InvalidWordCount { got },
        bip39::Error::UnknownWord(index) => KeyError::UnknownWord { index },
        bip39::Error::BadEntropyBitCount(bits) => {
            KeyError::InvalidEntropyLength { got: bits.div_euclid(8) }
        }
        bip39::Error::InvalidChecksum => KeyError::PhraseChecksum,
        // Only language detection reports this, and every parse here names English.
        bip39::Error::AmbiguousLanguages(_) => KeyError::internal("ambiguous phrase language"),
    }
}

/// Collapses whitespace, folds case, and normalizes to NFKD, because the library only
/// splits on whitespace and matches words exactly.
///
/// The specification only requires NFKD. The rest is input hygiene, so a phrase pasted
/// with odd spacing, capitals, or list numbers is the same phrase.
fn normalize_phrase(phrase: &str) -> Zeroizing<String> {
    join_words(|| phrase_words(phrase).map(|word| word.chars().flat_map(char::to_lowercase).nfkd()))
}

/// Joins the words with single spaces. The output is measured before it is written, so
/// the buffer never reallocates and leaves an unwiped copy behind.
fn join_words<W>(words: impl Fn() -> W) -> Zeroizing<String>
where
    W: Iterator<Item: Iterator<Item = char>>,
{
    let separators = words().count().saturating_sub(1);
    let word_bytes: usize = words().flatten().map(char::len_utf8).sum();
    let mut joined = Zeroizing::new(String::with_capacity(word_bytes.saturating_add(separators)));
    for (i, word) in words().enumerate() {
        if i > 0 {
            joined.push(' ');
        }
        joined.extend(word);
    }
    joined
}

#[cfg(test)]
mod tests {
    use jelly_keypair::test_util::TEST_PHRASE;

    use super::*;

    #[test]
    fn normalizing_word_by_word_matches_normalizing_the_joined_phrase() {
        for phrase in [
            TEST_PHRASE,
            "  \u{0130}stanbul\tCAF\u{00c9}\u{3000}\u{fb01}le ",
            "a\u{00a8}b \u{0301}c \u{ff21}",
            "",
        ] {
            let lowered: Vec<String> = phrase
                .split_whitespace()
                .map(|word| word.chars().flat_map(char::to_lowercase).collect())
                .collect();
            let whole: String = lowered.join(" ").nfkd().collect();
            assert_eq!(normalize_phrase(phrase).as_str(), whole, "{phrase:?}");
        }
    }

    #[test]
    fn the_library_mnemonic_wipes_on_drop() {
        fn wipes_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        wipes_on_drop::<bip39::Mnemonic>();
    }
}
