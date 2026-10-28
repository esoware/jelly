use bip39::Language;
use hmac::{Hmac, KeyInit as _, Mac as _};
use jelly_core::{KeyError, SecretText, phrase_words};
use sha2::Sha512;
use zeroize::Zeroizing;

/// A TON mnemonic is always this many words from BIP-39's English list.
const TON_SEED_WORDS: usize = 24;

/// ton-crypto's `PBKDF_ITERATIONS`, the rounds that stretch the entropy into the key seed.
const DEFAULT_SEED_ROUNDS: u32 = 100_000;
const DEFAULT_SEED_SALT: &[u8] = b"TON default seed";

/// `max(1, DEFAULT_SEED_ROUNDS / 256)`, the rounds ton-crypto spends telling a basic seed
/// from any other phrase.
const SEED_VERSION_ROUNDS: u32 = 390;
const SEED_VERSION_SALT: &[u8] = b"TON seed version";

/// The single round ton-crypto spends on the password-seed check.
const FAST_SEED_VERSION_ROUNDS: u32 = 1;
const FAST_SEED_VERSION_SALT: &[u8] = b"TON fast seed version";

/// Whether `input` is spelled as a TON mnemonic, without checking that it is one.
pub(crate) fn is_ton_seed_shape(input: &str) -> bool {
    let lowered = Zeroizing::new(input.to_ascii_lowercase());
    let words: Vec<&str> = phrase_words(&lowered).collect();
    words.len() == TON_SEED_WORDS && words.iter().all(|word| is_listed(word))
}

fn is_listed(word: &str) -> bool {
    Language::English.word_list().binary_search(&word).is_ok()
}

/// The ed25519 seed a 24-word TON mnemonic expands to, following ton-crypto's
/// `mnemonicToEntropy` and `mnemonicValidate`.
///
/// tonlib's `Mnemonic` implements the same scheme, but it never wipes the words, the
/// password, or the entropy, and its password check inverts ton-crypto's final one, so it
/// refuses a protected seed under its own password.
pub(crate) fn decode_ton_seed(
    input: &str,
    password: Option<&SecretText>,
) -> Result<Zeroizing<[u8; 32]>, KeyError> {
    let lowered = Zeroizing::new(input.to_ascii_lowercase());
    let words: Vec<&str> = phrase_words(&lowered).collect();
    for (index, word) in words.iter().enumerate() {
        if !is_listed(word) {
            return Err(KeyError::UnknownWord { index });
        }
    }
    if words.len() != TON_SEED_WORDS {
        return Err(KeyError::TonSeedWordCount { got: words.len() });
    }
    let phrase = Zeroizing::new(words.join(" "));
    let mac = Hmac::<Sha512>::new_from_slice(phrase.as_bytes())
        .map_err(|_| KeyError::internal("hmac key length"))?;
    let passless = Zeroizing::new(<[u8; 64]>::from(mac.clone().finalize().into_bytes()));
    let is_basic = seed_version_byte(&passless, SEED_VERSION_SALT, SEED_VERSION_ROUNDS) == 0;
    let is_password_seed =
        seed_version_byte(&passless, FAST_SEED_VERSION_SALT, FAST_SEED_VERSION_ROUNDS) == 1;
    let password = Zeroizing::new(
        password.map_or_else(String::new, |password| password.with_str(str::to_owned)),
    );
    let entropy = if password.is_empty() {
        if !is_basic {
            return Err(if is_password_seed {
                KeyError::PasswordRequired
            } else {
                KeyError::PhraseChecksum
            });
        }
        passless
    } else {
        if is_basic {
            return Err(KeyError::WrongPassword);
        }
        if !is_password_seed {
            return Err(KeyError::PhraseChecksum);
        }
        let mut mac = mac;
        mac.update(password.as_bytes());
        let entropy = Zeroizing::new(<[u8; 64]>::from(mac.finalize().into_bytes()));
        if seed_version_byte(&entropy, SEED_VERSION_SALT, SEED_VERSION_ROUNDS) != 0 {
            return Err(KeyError::WrongPassword);
        }
        entropy
    };
    let mut expanded = Zeroizing::new([0u8; 64]);
    pbkdf2::pbkdf2_hmac::<Sha512>(
        entropy.as_slice(),
        DEFAULT_SEED_SALT,
        DEFAULT_SEED_ROUNDS,
        expanded.as_mut_slice(),
    );
    Ok(Zeroizing::new(*expanded.first_chunk::<32>().ok_or(KeyError::internal("ton seed length"))?))
}

/// The first byte of `PBKDF2-HMAC-SHA512(entropy, salt, rounds)`, which ton-crypto reads
/// as the phrase's version.
fn seed_version_byte(entropy: &[u8; 64], salt: &[u8], rounds: u32) -> u8 {
    let mut version = Zeroizing::new([0u8; 64]);
    pbkdf2::pbkdf2_hmac::<Sha512>(entropy, salt, rounds, version.as_mut_slice());
    let [first, ..] = *version;
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::ENTRY;

    const WORDS: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";

    fn secret(text: &str) -> SecretText {
        SecretText::new(text.to_owned())
    }

    #[test]
    fn protected_native_seed_requires_the_original_password() {
        let words = "atom trumpet engine wisdom butter embark outer brand quiz gaze moment second daughter motion vault banner auction weasel fire kid use away cry cover";
        let password = secret("public-test-password");
        assert_eq!(
            hex::encode(decode_ton_seed(words, Some(&password)).unwrap().as_slice()),
            "dc03db4f1944a7522e2310b5f4ff366a5511e2dbd355274f829cc5c303d4cd35"
        );
        assert!(matches!(decode_ton_seed(words, None), Err(KeyError::PasswordRequired)));
        assert!(matches!(
            decode_ton_seed(words, Some(&secret("wrong"))),
            Err(KeyError::WrongPassword)
        ));
    }

    #[test]
    fn native_ton_seed_matches_the_published_ton_crypto_vector() {
        let imported = ENTRY.import_key(WORDS, None).unwrap();
        assert_eq!(
            ENTRY.export_key(&imported.material).unwrap().as_str(),
            "119dcf2840a3d56521d260b2f125eedc0d4f3795b9e627269a4b5a6dca8257bd"
        );
        assert_eq!(
            decode_ton_seed(WORDS, None).unwrap().as_slice(),
            imported.material.with_bytes(<[u8]>::to_vec)
        );
    }

    #[test]
    fn a_basic_seed_refuses_a_password_and_a_broken_phrase_is_refused() {
        assert!(matches!(
            decode_ton_seed(WORDS, Some(&secret("unexpected password"))),
            Err(KeyError::WrongPassword)
        ));
        assert!(matches!(
            decode_ton_seed("abandon abandon", None),
            Err(KeyError::TonSeedWordCount { got: 2 })
        ));
        assert!(matches!(
            decode_ton_seed(&WORDS.replace("game", "abandon"), None),
            Err(KeyError::PhraseChecksum)
        ));
    }

    #[test]
    fn a_phrase_that_is_no_ton_seed_fails_its_checksum_with_or_without_a_password() {
        let phrase = format!("{} art", ["abandon"; 23].join(" "));
        let password = secret("any password");
        for supplied in [None, Some(&password)] {
            assert!(matches!(decode_ton_seed(&phrase, supplied), Err(KeyError::PhraseChecksum)));
        }
    }

    #[test]
    fn the_shape_is_twenty_four_listed_words_in_any_case() {
        assert!(is_ton_seed_shape(WORDS));
        assert!(is_ton_seed_shape(&WORDS.to_uppercase()));
        assert!(!is_ton_seed_shape(&WORDS.replace("game", "zzzz")));
        assert!(!is_ton_seed_shape(&WORDS.replace(" game", "")));
        assert!(!is_ton_seed_shape(&"ab".repeat(32)));
    }
}
