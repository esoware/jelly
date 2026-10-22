use aes::Aes128;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit as _, StreamCipher as _};
use jelly_core::{KeyError, SecretText};
use serde::Deserialize;
use sha2::Sha256;
use sha3::{Digest as _, Keccak256};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::keccak::keccak_address_bytes;
use crate::secp256k1::Secp256k1Key;
use crate::secret_hex::hex_body;

/// Caps the memory a hostile file can demand. geth's defaults need 256 MiB, so a 1 GiB
/// ceiling accepts every real document.
const MAX_SCRYPT_MEMORY_BYTES: u64 = 1 << 30;

/// Caps the time a hostile file can demand, counted as `128 * n * r * p`, the bytes the
/// mixing passes touch.
///
/// `p` repeats the whole `n * r` pass, so the ceiling covers the product. Bounding memory
/// alone allows `n = 2, r = p = 65536`, hours of work that the scrypt crate's own check
/// cannot refuse because `r * p` overflows before it is compared.
const MAX_SCRYPT_WORK_BYTES: u64 = 1 << 30;

/// The same cap for PBKDF2, whose work is its round count once per 32-byte output block.
const MAX_PBKDF2_ROUNDS: u64 = 10_000_000;

const PBKDF2_BLOCK_BYTES: usize = 32;

/// Real documents ask for 32 bytes, so the ceiling only bounds the buffer and the PBKDF2
/// blocks a hostile file can demand.
const MAX_DERIVED_KEY_BYTES: usize = 1024;

type Aes128Ctr = Ctr128BE<Aes128>;

#[derive(Deserialize)]
struct Document {
    version: u32,
    #[serde(alias = "Crypto")]
    crypto: Crypto,
    address: Option<String>,
}

#[derive(Deserialize)]
struct Crypto {
    cipher: String,
    cipherparams: CipherParams,
    ciphertext: String,
    kdf: String,
    kdfparams: serde_json::Value,
    mac: String,
}

#[derive(Deserialize)]
struct CipherParams {
    iv: String,
}

#[derive(Deserialize)]
struct ScryptParams {
    dklen: usize,
    n: u64,
    r: u32,
    p: u32,
    salt: String,
}

#[derive(Deserialize)]
struct Pbkdf2Params {
    dklen: usize,
    c: u32,
    prf: Option<String>,
    salt: String,
}

/// A structural sniff, so format detection can tell a Web3 Secret Storage v3
/// document from other key text without validating it.
pub(crate) fn is_keystore_v3(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with('{') && (trimmed.contains("\"crypto\"") || trimmed.contains("\"Crypto\""))
}

/// Decrypts a Web3 Secret Storage v3 document into the secp256k1 key it holds.
///
/// A MAC mismatch is [`KeyError::WrongPassword`] whether the password was wrong or the
/// file was altered, because the MAC covers the derived key and the ciphertext together.
pub(crate) fn decrypt_keystore_v3(
    json: &str,
    password: &SecretText,
) -> Result<Secp256k1Key, KeyError> {
    let document: Document = serde_json::from_str(json).map_err(|_| KeyError::KeystoreMalformed)?;
    if document.version != 3 {
        return Err(KeyError::KeystoreVersion { version: document.version });
    }
    let crypto = &document.crypto;
    if !crypto.cipher.eq_ignore_ascii_case("aes-128-ctr") {
        return Err(KeyError::KeystoreUnsupported);
    }

    let iv: [u8; 16] = unhex(&crypto.cipherparams.iv)?
        .as_slice()
        .try_into()
        .map_err(|_| KeyError::KeystoreParams)?;
    let ciphertext = unhex(&crypto.ciphertext)?;
    let mac = unhex(&crypto.mac)?;

    let derived = derive_key(&crypto.kdf, &crypto.kdfparams, password)?;
    let mac_key = derived.get(16..32).ok_or(KeyError::KeystoreParams)?;
    let mut hasher = Keccak256::new();
    hasher.update(mac_key);
    hasher.update(&ciphertext);
    let computed = hasher.finalize();
    if computed.as_slice().ct_eq(mac.as_slice()).unwrap_u8() != 1 {
        return Err(KeyError::WrongPassword);
    }

    let aes_key = Zeroizing::new(*derived.first_chunk::<16>().ok_or(KeyError::KeystoreParams)?);
    let mut plain = Zeroizing::new(ciphertext);
    Aes128Ctr::new((&*aes_key).into(), (&iv).into()).apply_keystream(&mut plain);

    if plain.len() != 32 {
        return Err(KeyError::InvalidKeyLength { got: plain.len() });
    }
    let key = Secp256k1Key::from_secret_bytes(&plain)?;

    // The address field is advisory. When present and well formed, it checks that the
    // document holds the key it claims.
    if let Some(claimed) = document.address.as_deref().map(hex_body)
        && claimed.len() == 40
    {
        let address = hex::encode(keccak_address_bytes(&key.public_uncompressed())?);
        if !claimed.eq_ignore_ascii_case(&address) {
            return Err(KeyError::KeystoreAddressMismatch);
        }
    }
    Ok(key)
}

fn derive_key(
    kdf: &str,
    params: &serde_json::Value,
    password: &SecretText,
) -> Result<Zeroizing<Vec<u8>>, KeyError> {
    if kdf.eq_ignore_ascii_case("scrypt") {
        ScryptParams::deserialize(params).map_err(|_| KeyError::KeystoreMalformed)?.derive(password)
    } else if kdf.eq_ignore_ascii_case("pbkdf2") {
        Pbkdf2Params::deserialize(params).map_err(|_| KeyError::KeystoreMalformed)?.derive(password)
    } else {
        Err(KeyError::KeystoreUnsupported)
    }
}

impl ScryptParams {
    fn derive(&self, password: &SecretText) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        check_derived_key_length(self.dklen)?;
        let salt = unhex(&self.salt)?;
        if !self.n.is_power_of_two() || self.n < 2 {
            return Err(KeyError::KeystoreParams);
        }
        if self.r == 0 || self.p == 0 {
            return Err(KeyError::KeystoreParams);
        }
        let memory_bytes = 128u64.saturating_mul(self.n).saturating_mul(u64::from(self.r));
        if memory_bytes > MAX_SCRYPT_MEMORY_BYTES {
            return Err(KeyError::KeystoreParams);
        }
        if memory_bytes.saturating_mul(u64::from(self.p)) > MAX_SCRYPT_WORK_BYTES {
            return Err(KeyError::KeystoreParams);
        }
        let log_n = u8::try_from(self.n.trailing_zeros()).map_err(|_| KeyError::KeystoreParams)?;
        let params =
            scrypt::Params::new(log_n, self.r, self.p).map_err(|_| KeyError::KeystoreParams)?;
        let mut derived = Zeroizing::new(vec![0u8; self.dklen]);
        password
            .with_str(|text| scrypt::scrypt(text.as_bytes(), &salt, &params, &mut derived))
            .map_err(|_| KeyError::KeystoreParams)?;
        Ok(derived)
    }
}

impl Pbkdf2Params {
    fn derive(&self, password: &SecretText) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        check_derived_key_length(self.dklen)?;
        let salt = unhex(&self.salt)?;
        match self.prf.as_deref() {
            None => {}
            Some(prf) if prf.eq_ignore_ascii_case("hmac-sha256") => {}
            Some(_) => return Err(KeyError::KeystoreUnsupported),
        }
        let blocks = u64::try_from(self.dklen.div_ceil(PBKDF2_BLOCK_BYTES))
            .map_err(|_| KeyError::KeystoreParams)?;
        if self.c == 0 || u64::from(self.c).saturating_mul(blocks) > MAX_PBKDF2_ROUNDS {
            return Err(KeyError::KeystoreParams);
        }
        let mut derived = Zeroizing::new(vec![0u8; self.dklen]);
        password.with_str(|text| {
            pbkdf2::pbkdf2_hmac::<Sha256>(text.as_bytes(), &salt, self.c, &mut derived);
        });
        Ok(derived)
    }
}

/// The MAC covers `dk[16..32]`, so a shorter derived key is unusable.
const fn check_derived_key_length(length: usize) -> Result<(), KeyError> {
    if length < 32 || length > MAX_DERIVED_KEY_BYTES {
        return Err(KeyError::KeystoreParams);
    }
    Ok(())
}

fn unhex(text: &str) -> Result<Vec<u8>, KeyError> {
    hex::decode(hex_body(text)).map_err(|_| KeyError::KeystoreMalformed)
}

#[cfg(test)]
mod tests {
    use jelly_core::test_util::{MARKER, assert_redacted};
    use serde::Deserialize;

    use super::*;

    const VECTORS_JSON: &str = include_str!("../tests/vectors/keystore_v3.json");

    #[derive(Deserialize)]
    struct Vectors {
        password: String,
        secret: String,
        address: String,
        pbkdf2: String,
        scrypt: String,
    }

    fn vectors() -> Vectors {
        serde_json::from_str(VECTORS_JSON).expect("the vector file parses")
    }

    fn secret_text(text: &str) -> SecretText {
        SecretText::new(text.to_owned())
    }

    #[test]
    fn both_published_kdf_vectors_decrypt_to_their_secret() {
        let suite = vectors();
        for document in [&suite.pbkdf2, &suite.scrypt] {
            let key =
                decrypt_keystore_v3(document, &secret_text(&suite.password)).expect("decrypt");
            assert_eq!(key.with_secret_bytes(|secret| hex::encode(secret)), suite.secret);
        }
    }

    #[test]
    fn a_wrong_password_and_tampering_are_the_same_answer() {
        let suite = vectors();
        assert!(matches!(
            decrypt_keystore_v3(&suite.pbkdf2, &secret_text("not the password")),
            Err(KeyError::WrongPassword)
        ));
        assert!(matches!(
            decrypt_keystore_v3(&suite.pbkdf2, &secret_text("")),
            Err(KeyError::WrongPassword)
        ));

        let flipped_ciphertext = suite.pbkdf2.replace(
            "5318b4d5bcd28de64ee5559e671353e16f075ecae9f99c7a79a38af5f869aa46",
            "5318b4d5bcd28de64ee5559e671353e16f075ecae9f99c7a79a38af5f869aa47",
        );
        let flipped_mac = suite.pbkdf2.replace(
            "517ead924a9d0dc3124507e3393d175ce3ff7c1e96529c6c555ce9e51205e9b2",
            "517ead924a9d0dc3124507e3393d175ce3ff7c1e96529c6c555ce9e51205e9b3",
        );
        for document in [&flipped_ciphertext, &flipped_mac] {
            assert_ne!(document, &suite.pbkdf2, "the replacement matched something");
            assert!(matches!(
                decrypt_keystore_v3(document, &secret_text(&suite.password)),
                Err(KeyError::WrongPassword)
            ));
        }
    }

    #[test]
    fn detection_reads_the_shape_alone() {
        let suite = vectors();
        assert!(is_keystore_v3(&suite.pbkdf2));
        assert!(is_keystore_v3(&suite.scrypt));
        assert!(is_keystore_v3(&format!("  \n{}", suite.pbkdf2)));
        assert!(!is_keystore_v3("[1,2,3]"));
        assert!(!is_keystore_v3(r#"{"version":3}"#));
    }

    #[test]
    fn the_address_field_must_match_the_key() {
        let suite = vectors();
        let matching = suite
            .pbkdf2
            .replace(r#""version":3"#, &format!(r#""address":"{}","version":3"#, suite.address));
        decrypt_keystore_v3(&matching, &secret_text(&suite.password)).unwrap();

        let wrong = suite.pbkdf2.replace(
            r#""version":3"#,
            r#""address":"0000000000000000000000000000000000000000","version":3"#,
        );
        assert!(matches!(
            decrypt_keystore_v3(&wrong, &secret_text(&suite.password)),
            Err(KeyError::KeystoreAddressMismatch)
        ));
    }

    #[test]
    fn malformed_documents_are_rejected() {
        let suite = vectors();
        let cases = [
            suite.pbkdf2.replace(r#""version":3"#, r#""version":1"#),
            suite.pbkdf2.replace("aes-128-ctr", "aes-256-cbc"),
            suite.pbkdf2.replace(r#""kdf":"pbkdf2""#, r#""kdf":"argon2id""#),
            suite.pbkdf2.replace("hmac-sha256", "hmac-sha512"),
            suite.pbkdf2.replace(r#""dklen":32"#, r#""dklen":16"#),
            suite.pbkdf2.replace("6087dab2f9fdbbfaddc31a909735c1e6", "zzzz"),
            String::from(r#"{"version":3}"#),
            String::from("{\"crypto\": "),
            suite.pbkdf2.replace(r#""c":262144"#, r#""c":4000000000"#),
            suite.scrypt.replace(r#""n":262144"#, r#""n":1099511627776"#),
            suite.scrypt.replace(r#""n":262144"#, r#""n":262143"#),
            // Little memory and an enormous repeat count, about a terabyte of work that the
            // memory ceiling alone lets through and the scrypt crate's own check overflows on.
            suite
                .scrypt
                .replace(r#""n":262144"#, r#""n":2"#)
                .replace(r#""r":1"#, r#""r":65536"#)
                .replace(r#""p":8"#, r#""p":65536"#),
            // A long derived key multiplies the rounds counted against the ceiling.
            suite
                .pbkdf2
                .replace(r#""c":262144"#, r#""c":9000000"#)
                .replace(r#""dklen":32"#, r#""dklen":1024"#),
        ];
        for (index, document) in cases.iter().enumerate() {
            let error = decrypt_keystore_v3(document, &secret_text(&suite.password))
                .expect_err("the case is rejected");
            assert!(
                matches!(
                    error,
                    KeyError::KeystoreMalformed
                        | KeyError::KeystoreVersion { .. }
                        | KeyError::KeystoreUnsupported
                        | KeyError::KeystoreParams
                ),
                "case {index}: {error:?}"
            );
        }
    }

    #[test]
    fn a_rejected_keystore_never_leaks_its_password_or_body() {
        let document = format!(
            "{{\"crypto\":{{\"cipher\":\"aes-128-ctr\",\"cipherparams\":{{\"iv\":\"{MARKER}\"}},\"ciphertext\":\"00\",\"kdf\":\"pbkdf2\",\"kdfparams\":{{}},\"mac\":\"00\"}},\"version\":3}}"
        );
        let error = decrypt_keystore_v3(&document, &secret_text(MARKER)).expect_err("rejected");
        assert_redacted("decrypt_keystore_v3", &document, &error);
        assert_redacted("decrypt_keystore_v3", MARKER, &error);
    }

    #[test]
    fn a_capitalised_crypto_key_is_accepted() {
        let suite = vectors();
        let capitalised = suite.pbkdf2.replace(r#""crypto":"#, r#""Crypto":"#);
        assert!(is_keystore_v3(&capitalised));
        let key =
            decrypt_keystore_v3(&capitalised, &secret_text(&suite.password)).expect("decrypt");
        assert_eq!(key.with_secret_bytes(|secret| hex::encode(secret)), suite.secret);
    }
}
