//! The on-disk vault file: a JSON envelope around the encrypted payload.
//!
//! ```text
//! field                    | type    | meaning
//! -------------------------|---------|------------------------------------------
//! magic                    | string  | "jelly-vault"
//! format_version           | u32     | 3
//! wallet_id                | string  | opaque, also the keychain account
//! kdf.algorithm            | string  | "argon2id"
//! kdf.version              | u32     | 19 (0x13, Argon2 v1.3)
//! kdf.m_cost_kib           | u32     | argon2id memory cost
//! kdf.t_cost               | u32     | argon2id passes
//! kdf.p_cost               | u32     | argon2id lanes
//! kdf.salt                 | hex 16B | argon2id salt, fresh per rekey
//! hkdf.hash                | string  | "sha256"
//! hkdf.salt                | hex 32B | hkdf extract salt, fresh per rekey
//! aead.algorithm           | string  | "xchacha20poly1305"
//! aead.nonce               | hex 24B | fresh per write
//! commitment               | hex 32B | key commitment of the password factor
//! ciphertext               | hex     | XChaCha20-Poly1305(payload) || tag
//! header_tag               | hex 32B | HMAC-SHA-256(header mac key, canonical bytes)
//! ```
//!
//! The hex decoder length-checks every fixed-width field as it parses. Algorithm names
//! are checked even though only one value is ever written, so a later version can add
//! another cleanly.
//!
//! `header_tag` covers the parsed field values in a canonical, length-prefixed encoding,
//! not the JSON text, so reformatting the file doesn't break it but any real change does.
//! Covering the KDF parameters blocks a downgrade. The tag is checked under a key from
//! the master key alone, before the password commitment, which is what keeps `Corrupt`
//! and `WrongPassword` distinct.
//!
//! The tag can't prove freshness, so an older copy of the same wallet's file still
//! verifies. Rollback protection would need state outside the file, and the design
//! deliberately keeps none.

use serde::{Deserialize, Serialize};

use crate::error::VaultError;
use crate::kdf::{ARGON2_VERSION, Argon2Params, KDF_SALT_LEN};
use crate::keys::{
    COMMITMENT_LEN, HKDF_SALT_LEN, HeaderMac, KEY_LEN, MAC_LEN, NONCE_LEN, TAG_LEN, length_prefix,
};

/// Older versions aren't read. The field lets a file from another build be refused by
/// version instead of misparsed.
const FORMAT_VERSION: u32 = 3;

const MAGIC: &str = "jelly-vault";

const KDF_ALGORITHM: &str = "argon2id";
const AEAD_ALGORITHM: &str = "xchacha20poly1305";
const HKDF_HASH: &str = "sha256";

/// AEAD associated data domain. Part of the on-disk format.
const AAD_DOMAIN: &[u8] = b"jelly-vault/v3/aad";
/// Header MAC message domain. Part of the on-disk format.
const MAC_DOMAIN: &[u8] = b"jelly-vault/v3/header";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KdfBlock {
    algorithm: String,
    version: u32,
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
    #[serde(with = "hex")]
    pub(crate) salt: [u8; KDF_SALT_LEN],
}

impl KdfBlock {
    pub(crate) fn new(params: Argon2Params, salt: [u8; KDF_SALT_LEN]) -> Self {
        Self {
            algorithm: KDF_ALGORITHM.to_owned(),
            version: u32::from(ARGON2_VERSION),
            m_cost_kib: params.m_cost_kib(),
            t_cost: params.t_cost(),
            p_cost: params.p_cost(),
            salt,
        }
    }

    pub(crate) const fn params(&self) -> Argon2Params {
        Argon2Params::new(self.m_cost_kib, self.t_cost, self.p_cost)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HkdfBlock {
    hash: String,
    #[serde(with = "hex")]
    pub(crate) salt: [u8; HKDF_SALT_LEN],
}

impl HkdfBlock {
    pub(crate) fn new(salt: [u8; HKDF_SALT_LEN]) -> Self {
        Self { hash: HKDF_HASH.to_owned(), salt }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AeadBlock {
    algorithm: String,
    #[serde(with = "hex")]
    pub(crate) nonce: [u8; NONCE_LEN],
}

impl AeadBlock {
    pub(crate) fn new(nonce: [u8; NONCE_LEN]) -> Self {
        Self { algorithm: AEAD_ALGORITHM.to_owned(), nonce }
    }
}

/// Holds no secrets. `ciphertext` is encrypted and everything else is metadata.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VaultFile {
    magic: String,
    format_version: u32,
    pub(crate) wallet_id: String,
    pub(crate) kdf: KdfBlock,
    pub(crate) hkdf: HkdfBlock,
    pub(crate) aead: AeadBlock,
    #[serde(with = "hex")]
    pub(crate) commitment: [u8; COMMITMENT_LEN],
    #[serde(with = "hex")]
    pub(crate) ciphertext: Vec<u8>,
    #[serde(with = "hex")]
    header_tag: [u8; MAC_LEN],
}

impl VaultFile {
    /// Builds the file and stamps its header tag.
    pub(crate) fn build(
        wallet_id: &str,
        kdf: KdfBlock,
        hkdf: HkdfBlock,
        aead: AeadBlock,
        commitment: [u8; COMMITMENT_LEN],
        ciphertext: Vec<u8>,
        mac_key: &[u8; KEY_LEN],
    ) -> Result<Self, VaultError> {
        let mut file = Self {
            magic: MAGIC.to_owned(),
            format_version: FORMAT_VERSION,
            wallet_id: wallet_id.to_owned(),
            kdf,
            hkdf,
            aead,
            commitment,
            ciphertext,
            header_tag: [0u8; MAC_LEN],
        };
        file.stamp(mac_key)?;
        Ok(file)
    }

    pub(crate) fn stamp(&mut self, mac_key: &[u8; KEY_LEN]) -> Result<(), VaultError> {
        self.header_tag = self.mac(mac_key)?.finish();
        Ok(())
    }

    /// Password-independent, and always called before the commitment check.
    pub(crate) fn verify_integrity(&self, mac_key: &[u8; KEY_LEN]) -> Result<(), VaultError> {
        if self.mac(mac_key)?.verify(&self.header_tag) {
            Ok(())
        } else {
            Err(VaultError::corrupt("the header integrity tag doesn't verify"))
        }
    }

    /// Covers every field except `header_tag` itself.
    fn mac(&self, mac_key: &[u8; KEY_LEN]) -> Result<HeaderMac, VaultError> {
        let mut mac = HeaderMac::new(mac_key)?;
        mac.update(MAC_DOMAIN);
        mac.update_field(self.magic.as_bytes())?;
        mac.update(&self.format_version.to_le_bytes());
        mac.update_field(self.wallet_id.as_bytes())?;
        mac.update_field(self.kdf.algorithm.as_bytes())?;
        mac.update(&self.kdf.version.to_le_bytes());
        mac.update(&self.kdf.m_cost_kib.to_le_bytes());
        mac.update(&self.kdf.t_cost.to_le_bytes());
        mac.update(&self.kdf.p_cost.to_le_bytes());
        mac.update_field(&self.kdf.salt)?;
        mac.update_field(self.hkdf.hash.as_bytes())?;
        mac.update_field(&self.hkdf.salt)?;
        mac.update_field(self.aead.algorithm.as_bytes())?;
        mac.update_field(&self.aead.nonce)?;
        mac.update_field(&self.commitment)?;
        mac.update_field(&self.ciphertext)?;
        Ok(mac)
    }

    /// Structural validation only. Touches no key.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, VaultError> {
        let file: Self = serde_json::from_slice(bytes).map_err(VaultError::Malformed)?;

        if file.magic != MAGIC {
            return Err(VaultError::corrupt("the file magic is wrong"));
        }
        if file.format_version != FORMAT_VERSION {
            return Err(VaultError::UnsupportedVersion {
                found: file.format_version,
                supported: FORMAT_VERSION,
            });
        }
        if file.kdf.algorithm != KDF_ALGORITHM || file.kdf.version != u32::from(ARGON2_VERSION) {
            return Err(VaultError::corrupt("the kdf isn't argon2id v1.3"));
        }
        if file.hkdf.hash != HKDF_HASH {
            return Err(VaultError::corrupt("the hkdf hash isn't sha256"));
        }
        if file.aead.algorithm != AEAD_ALGORITHM {
            return Err(VaultError::corrupt("the aead isn't xchacha20poly1305"));
        }
        if file.ciphertext.len() < TAG_LEN {
            return Err(VaultError::corrupt("the ciphertext is shorter than its tag"));
        }
        file.kdf.params().check_minimum()?;
        Ok(file)
    }

    /// Pretty-printed so the header stays readable to someone doing forensics
    /// on a backup.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, VaultError> {
        serde_json::to_vec_pretty(self).map_err(|_| VaultError::internal("header encoding"))
    }
}

/// Binds the format version and the length-prefixed wallet id into the AEAD, so a
/// ciphertext transplanted from another wallet's file is refused.
pub(crate) fn associated_data(wallet_id: &str) -> Result<Vec<u8>, VaultError> {
    let length = length_prefix(wallet_id.len())?;
    Ok(<[&[u8]]>::concat(&[
        AAD_DOMAIN,
        &FORMAT_VERSION.to_le_bytes(),
        &length,
        wallet_id.as_bytes(),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC_KEY: [u8; KEY_LEN] = [0x8a; KEY_LEN];
    const WALLET_ID: &str = "wallet-1";

    type Mutation = (&'static str, fn(&mut VaultFile));

    fn sample() -> VaultFile {
        VaultFile::build(
            WALLET_ID,
            KdfBlock::new(Argon2Params::MINIMUM, [7u8; KDF_SALT_LEN]),
            HkdfBlock::new([9u8; HKDF_SALT_LEN]),
            AeadBlock::new([3u8; NONCE_LEN]),
            [4u8; COMMITMENT_LEN],
            vec![0x5e; 64],
            &MAC_KEY,
        )
        .unwrap()
    }

    #[test]
    fn a_file_round_trips_through_json_and_verifies() {
        let file = sample();
        let parsed = VaultFile::parse(&file.to_bytes().unwrap()).unwrap();
        parsed.verify_integrity(&MAC_KEY).unwrap();
        assert_eq!(parsed.magic, MAGIC);
        assert_eq!(parsed.format_version, FORMAT_VERSION);
        assert_eq!(parsed.wallet_id, WALLET_ID);
        assert_eq!(parsed.kdf.params(), Argon2Params::MINIMUM);
        assert_eq!(parsed.kdf.salt, [7u8; KDF_SALT_LEN]);
        assert_eq!(parsed.hkdf.salt, [9u8; HKDF_SALT_LEN]);
        assert_eq!(parsed.aead.nonce, [3u8; NONCE_LEN]);
        assert_eq!(parsed.commitment, [4u8; COMMITMENT_LEN]);
    }

    #[test]
    fn the_tag_ignores_json_formatting() {
        let file = sample();
        let compact = serde_json::to_vec(&file).unwrap();
        assert_ne!(compact, file.to_bytes().unwrap());
        VaultFile::parse(&compact).unwrap().verify_integrity(&MAC_KEY).unwrap();
    }

    /// Re-stamping after each mutation is the positive control. It shows the failure came
    /// from the tag, not from the mutation itself.
    #[test]
    fn every_field_is_covered_by_the_tag() {
        let cases: [Mutation; 15] = [
            ("magic", |file| file.magic.push('x')),
            ("format_version", |file| file.format_version = 4),
            ("wallet_id", |file| file.wallet_id.push('x')),
            ("kdf.algorithm", |file| file.kdf.algorithm.push('x')),
            ("kdf.version", |file| file.kdf.version = 0x10),
            ("kdf.m_cost_kib", |file| file.kdf.m_cost_kib = file.kdf.m_cost_kib.saturating_add(1)),
            ("kdf.t_cost", |file| file.kdf.t_cost = file.kdf.t_cost.saturating_add(1)),
            ("kdf.p_cost", |file| file.kdf.p_cost = file.kdf.p_cost.saturating_add(1)),
            ("kdf.salt", |file| file.kdf.salt[0] ^= 0xff),
            ("hkdf.hash", |file| file.hkdf.hash.push('x')),
            ("hkdf.salt", |file| file.hkdf.salt[0] ^= 0xff),
            ("aead.algorithm", |file| file.aead.algorithm.push('x')),
            ("aead.nonce", |file| file.aead.nonce[0] ^= 0xff),
            ("commitment", |file| file.commitment[0] ^= 0xff),
            ("ciphertext", |file| file.ciphertext[0] ^= 0x01),
        ];
        for (name, mutate) in cases {
            let mut file = sample();
            mutate(&mut file);
            assert!(
                matches!(file.verify_integrity(&MAC_KEY), Err(VaultError::Corrupt { .. })),
                "mutating {name} must fail the integrity check"
            );
            file.stamp(&MAC_KEY).unwrap();
            file.verify_integrity(&MAC_KEY).unwrap();
        }
    }

    #[test]
    fn flipping_any_ciphertext_bit_is_caught() {
        let mut file = sample();
        for index in 0..file.ciphertext.len() {
            for bit in [0x01u8, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80] {
                file.ciphertext[index] ^= bit;
                assert!(
                    matches!(file.verify_integrity(&MAC_KEY), Err(VaultError::Corrupt { .. })),
                    "ciphertext byte {index} bit {bit:#04x} was not caught"
                );
                file.ciphertext[index] ^= bit;
            }
        }
        file.verify_integrity(&MAC_KEY).unwrap();
    }

    #[test]
    fn a_different_mac_key_never_verifies() {
        assert!(matches!(
            sample().verify_integrity(&[0x8b; KEY_LEN]),
            Err(VaultError::Corrupt { .. })
        ));
    }

    #[test]
    fn parse_rejects_structural_defects() {
        assert!(matches!(VaultFile::parse(b"not json"), Err(VaultError::Malformed(_))));

        let cases: [Mutation; 5] = [
            ("magic", |file| file.magic = String::from("something-else")),
            ("kdf algorithm", |file| file.kdf.algorithm = String::from("pbkdf2")),
            ("kdf version", |file| file.kdf.version = 0x10),
            ("hkdf hash", |file| file.hkdf.hash = String::from("sha512")),
            ("aead algorithm", |file| file.aead.algorithm = String::from("aes")),
        ];
        for (name, mutate) in cases {
            let mut file = sample();
            mutate(&mut file);
            assert!(
                matches!(
                    VaultFile::parse(&file.to_bytes().unwrap()),
                    Err(VaultError::Corrupt { .. })
                ),
                "{name} must be rejected"
            );
        }

        let mut file = sample();
        file.ciphertext.truncate(4);
        assert!(matches!(
            VaultFile::parse(&file.to_bytes().unwrap()),
            Err(VaultError::Corrupt { .. })
        ));

        let mut file = sample();
        file.format_version = 99;
        assert!(matches!(
            VaultFile::parse(&file.to_bytes().unwrap()),
            Err(VaultError::UnsupportedVersion { found: 99, supported: FORMAT_VERSION })
        ));
    }

    #[test]
    fn parse_rejects_hex_fields_of_the_wrong_width() {
        let bytes = sample().to_bytes().unwrap();
        for (pointer, length) in [
            ("/kdf/salt", KDF_SALT_LEN),
            ("/hkdf/salt", HKDF_SALT_LEN),
            ("/aead/nonce", NONCE_LEN),
            ("/commitment", COMMITMENT_LEN),
            ("/header_tag", MAC_LEN),
        ] {
            let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            *json.pointer_mut(pointer).unwrap() = serde_json::Value::from("aa".repeat(length - 1));
            assert!(
                matches!(
                    VaultFile::parse(&serde_json::to_vec(&json).unwrap()),
                    Err(VaultError::Malformed(_))
                ),
                "a short {pointer} must be refused"
            );
        }
    }

    #[test]
    fn parse_rejects_unknown_fields() {
        let mut json: serde_json::Value =
            serde_json::from_slice(&sample().to_bytes().unwrap()).unwrap();
        json.as_object_mut().unwrap().insert(String::from("extra"), serde_json::Value::Null);
        assert!(matches!(
            VaultFile::parse(&serde_json::to_vec(&json).unwrap()),
            Err(VaultError::Malformed(_))
        ));
    }

    #[test]
    fn parse_rejects_kdf_parameters_below_the_floor() {
        for (field, params) in [
            ("m_cost_kib", Argon2Params::new(1_024, 3, 4)),
            ("t_cost", Argon2Params::new(262_144, 1, 4)),
            ("p_cost", Argon2Params::new(262_144, 3, 0)),
        ] {
            let mut file = sample();
            file.kdf.m_cost_kib = params.m_cost_kib();
            file.kdf.t_cost = params.t_cost();
            file.kdf.p_cost = params.p_cost();
            match VaultFile::parse(&file.to_bytes().unwrap()) {
                Err(VaultError::WeakKdfParams { field: got }) => assert_eq!(got, field),
                other => panic!("expected weak parameters for {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_aad_opens_with_its_domain_and_binds_the_wallet_id() {
        assert_ne!(associated_data("ab").unwrap(), associated_data("ba").unwrap());
        assert_ne!(associated_data("abc").unwrap(), associated_data("ab").unwrap());
        assert!(associated_data("").unwrap().starts_with(AAD_DOMAIN));
    }

    /// Editing any of these bricks every existing vault.
    #[test]
    fn on_disk_constants_are_pinned() {
        assert_eq!(MAGIC, "jelly-vault");
        assert_eq!(FORMAT_VERSION, 3);
        assert_eq!(KDF_ALGORITHM, "argon2id");
        assert_eq!(AEAD_ALGORITHM, "xchacha20poly1305");
        assert_eq!(HKDF_HASH, "sha256");
        assert_eq!(AAD_DOMAIN, b"jelly-vault/v3/aad");
        assert_eq!(MAC_DOMAIN, b"jelly-vault/v3/header");
        assert_eq!(u32::from(ARGON2_VERSION), 19);
        assert_eq!((KDF_SALT_LEN, HKDF_SALT_LEN, NONCE_LEN), (16, 32, 24));
        assert_eq!((MAC_LEN, COMMITMENT_LEN, TAG_LEN), (32, 32, 16));
    }
}
