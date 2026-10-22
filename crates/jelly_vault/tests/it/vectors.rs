//! Published vectors for every primitive the vault format rests on, run against the
//! crates and features `jelly_vault` links.
//!
//! These prove the primitives. `tests/it/golden.rs` proves this crate's use of them.

use std::error::Error;
use std::fs;
use std::ops::Deref;
use std::path::Path;

use argon2::{Algorithm, Argon2, AssociatedData, ParamsBuilder, Version};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{AeadInOut as _, KeyInit as _},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::Sha256;

/// How a vector file spells a byte string.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Spelling {
    Hex(String),
    Ascii(String),
    Repeat { byte: u8, count: usize },
    Seq { start: u8, count: usize },
}

/// A byte string, read from its spelling as the vector file loads.
#[derive(Debug, Deserialize)]
#[serde(try_from = "Spelling")]
struct Bytes(Vec<u8>);

impl TryFrom<Spelling> for Bytes {
    type Error = String;

    fn try_from(spelling: Spelling) -> Result<Self, String> {
        let bytes = match spelling {
            Spelling::Hex(text) => hex::decode(text).map_err(|error| error.to_string())?,
            Spelling::Ascii(text) => text.into_bytes(),
            Spelling::Repeat { byte, count } => vec![byte; count],
            Spelling::Seq { start, count } => (0..count)
                .map(|offset| {
                    u8::try_from(offset)
                        .ok()
                        .and_then(|offset| start.checked_add(offset))
                        .ok_or_else(|| String::from("a sequence runs past a byte"))
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(Self(bytes))
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
struct File<C> {
    cases: Vec<C>,
}

fn load<T: DeserializeOwned>(name: &str) -> Result<T, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors").join(name);
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?)
}

mod hmac_sha256 {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        key: Bytes,
        data: Bytes,
        tag: String,
        truncate: Option<usize>,
    }

    /// The header tag is HMAC-SHA-256.
    #[test]
    fn matches_the_rfc_4231_vectors() {
        let file: File<Case> = load("hmac.json").unwrap();
        assert_eq!(file.cases.len(), 7);
        for case in &file.cases {
            let mut mac =
                Hmac::<Sha256>::new_from_slice(&case.key).expect("hmac takes any key length");
            mac.update(&case.data);
            let tag = mac.finalize().into_bytes();
            let got = match case.truncate {
                Some(length) => tag.get(..length).expect("truncation fits").to_vec(),
                None => tag.to_vec(),
            };
            assert_eq!(hex::encode(got), case.tag, "{}", case.name);
        }
    }
}

mod hkdf_sha256 {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        ikm: Bytes,
        salt: Bytes,
        info: Bytes,
        length: usize,
        prk: String,
        okm: String,
    }

    /// HKDF-SHA-256 combines the two factors into the session keys.
    #[test]
    fn matches_the_rfc_5869_vectors() {
        let file: File<Case> = load("hkdf.json").unwrap();
        assert_eq!(file.cases.len(), 3);
        for case in &file.cases {
            let salt = case.salt.to_vec();
            let (prk, hkdf) = Hkdf::<Sha256>::extract(Some(&salt), &case.ikm);
            assert_eq!(hex::encode(prk), case.prk, "{} prk", case.name);

            let mut okm = vec![0u8; case.length];
            hkdf.expand(&case.info, &mut okm).expect("expand");
            assert_eq!(hex::encode(&okm), case.okm, "{} okm", case.name);
        }
    }
}

mod xchacha20poly1305 {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        key: Bytes,
        nonce: Bytes,
        aad: Bytes,
        plaintext: Bytes,
        ciphertext: String,
        tag: String,
    }

    /// The payload AEAD, including the `HChaCha20` subkey derivation that makes
    /// the 192-bit nonce possible.
    #[test]
    fn matches_the_draft_irtf_cfrg_xchacha_03_vector() {
        let file: File<Case> = load("xchacha.json").unwrap();
        assert_eq!(file.cases.len(), 1);
        for case in &file.cases {
            let key: [u8; 32] = case.key.to_vec().try_into().expect("32-byte key");
            let nonce: [u8; 24] = case.nonce.to_vec().try_into().expect("24-byte nonce");
            let aad = case.aad.to_vec();
            let plaintext = case.plaintext.to_vec();

            let cipher = XChaCha20Poly1305::new(&Key::from(key));
            let mut buffer = plaintext.clone();
            cipher.encrypt_in_place(&XNonce::from(nonce), &aad, &mut buffer).expect("seal");
            assert_eq!(
                hex::encode(&buffer),
                format!("{}{}", case.ciphertext, case.tag),
                "{}",
                case.name
            );

            cipher.decrypt_in_place(&XNonce::from(nonce), &aad, &mut buffer).expect("open");
            assert_eq!(buffer, plaintext, "{}", case.name);
        }
    }
}

mod argon2id {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Case {
        name: String,
        m_cost_kib: u32,
        t_cost: u32,
        p_cost: u32,
        password: Bytes,
        salt: Bytes,
        secret: Bytes,
        associated_data: Bytes,
        tag: String,
    }

    /// The RFC 9106 section 5.3 Argon2id vector.
    ///
    /// The secret-key and associated-data inputs are the RFC's own. The vault's KDF
    /// exposes neither, so this checks only the primitive. `src/kdf.rs` and the golden
    /// file pin the wiring.
    #[test]
    fn matches_the_rfc_9106_vector() {
        let file: File<Case> = load("argon2.json").unwrap();
        assert_eq!(file.cases.len(), 1);
        for case in &file.cases {
            let associated_data = case.associated_data.to_vec();
            let params = ParamsBuilder::new()
                .m_cost(case.m_cost_kib)
                .t_cost(case.t_cost)
                .p_cost(case.p_cost)
                .data(AssociatedData::new(&associated_data).expect("associated data"))
                .build()
                .expect("params");
            let secret = case.secret.to_vec();
            let argon2 =
                Argon2::new_with_secret(&secret, Algorithm::Argon2id, Version::V0x13, params)
                    .expect("context");
            let mut tag = [0u8; 32];
            argon2.hash_password_into(&case.password, &case.salt, &mut tag).expect("hash");
            assert_eq!(hex::encode(tag), case.tag, "{}", case.name);
        }
    }
}
