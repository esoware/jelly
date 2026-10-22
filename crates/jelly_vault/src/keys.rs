//! The key hierarchy of one vault file.
//!
//! Two factors go in, the keychain master key and the Argon2id output over the
//! password. HKDF-SHA-256 combines them with the per-file extract salt from the
//! header and a domain-separated `info` per output.
//!
//! ```text
//! header mac key = HKDF(salt, ikm = master,                   info = HMAC_INFO || lp(wallet_id))
//! aead key       = HKDF(salt, ikm = master || argon2_output,  info = AEAD_INFO || lp(wallet_id))
//! commitment     = HKDF(salt, ikm = master || argon2_output,  info = COMMIT_INFO || lp(wallet_id))
//!
//! lp(x)          = u64le(len(x)) || x
//! ```
//!
//! The header MAC key is deliberately password-independent, so the file's integrity is
//! provable before the password is known. That is what separates a tampered file from a
//! wrong password.
//!
//! The commitment is written to the header in clear. XChaCha20-Poly1305 is not
//! key-committing, so a ciphertext can in principle be crafted to open under more than
//! one key. Comparing a password-derived commitment before the AEAD closes that class
//! and makes an AEAD failure mean corruption rather than a wrong password. It reveals
//! nothing an attacker holding the file and the master key could not already test by
//! trial decryption, and nothing at all to an attacker holding only the file.
//!
//! Every `info` string below, like the domains in `vault_file.rs`, is on-disk contract.
//! Editing one makes every existing vault undecryptable.

use std::fmt;
use std::mem;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{AeadInOut as _, KeyInit as _},
};
use hkdf::{Hkdf, HkdfExtract};
use hmac::{Hmac, Mac as _};
use secrecy::SecretBox;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::error::VaultError;
use crate::keychain::MasterKey;

/// Width of every derived key: the Argon2id output, the AEAD key, and the header MAC key.
pub(crate) const KEY_LEN: usize = 32;
pub(crate) const NONCE_LEN: usize = 24;
/// Poly1305 tag length, appended to the ciphertext by `seal`.
pub(crate) const TAG_LEN: usize = 16;
pub(crate) const HKDF_SALT_LEN: usize = 32;
pub(crate) const MAC_LEN: usize = 32;
pub(crate) const COMMITMENT_LEN: usize = 32;

const AEAD_INFO: &[u8] = b"jelly-vault/v3/aead";
const HMAC_INFO: &[u8] = b"jelly-vault/v3/hmac";
const COMMIT_INFO: &[u8] = b"jelly-vault/v3/commit";

/// What a password unlocks, the AEAD key and the commitment proving it is the key this
/// file was sealed under.
pub(crate) struct PasswordKeys {
    pub(crate) aead_key: Zeroizing<[u8; KEY_LEN]>,
    pub(crate) commitment: [u8; COMMITMENT_LEN],
}

impl fmt::Debug for PasswordKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordKeys(<redacted>)")
    }
}

/// The password-independent key the header tag is computed under, from the master key
/// alone. Boxed, because an open session holds it at one address for its lifetime.
pub(crate) fn header_mac_key(
    master: &MasterKey,
    hkdf_salt: &[u8; HKDF_SALT_LEN],
    wallet_id: &str,
) -> Result<SecretBox<[u8; KEY_LEN]>, VaultError> {
    let hkdf = master.with_bytes(|bytes| Hkdf::<Sha256>::new(Some(hkdf_salt), bytes));
    let key = expand(&hkdf, HMAC_INFO, wallet_id)?;
    Ok(SecretBox::init_with_mut(|slot: &mut [u8; KEY_LEN]| *slot = *key))
}

/// Needs both factors. Neither the keychain entry nor the password is sufficient alone.
pub(crate) fn password_keys(
    master: &MasterKey,
    password_hash: &[u8; KEY_LEN],
    hkdf_salt: &[u8; HKDF_SALT_LEN],
    wallet_id: &str,
) -> Result<PasswordKeys, VaultError> {
    let mut extract = HkdfExtract::<Sha256>::new(Some(hkdf_salt));
    master.with_bytes(|bytes| extract.input_ikm(bytes));
    extract.input_ikm(password_hash);
    let (_, hkdf) = extract.finalize();
    Ok(PasswordKeys {
        aead_key: expand(&hkdf, AEAD_INFO, wallet_id)?,
        commitment: *expand(&hkdf, COMMIT_INFO, wallet_id)?,
    })
}

/// Whether two commitments are the same, without leaking where they differ.
#[must_use]
pub(crate) fn commitment_matches(
    left: &[u8; COMMITMENT_LEN],
    right: &[u8; COMMITMENT_LEN],
) -> bool {
    left.ct_eq(right).into()
}

/// Eight little-endian bytes of `length`, an on-disk contract.
///
/// Every variable-length field in a MAC message, an HKDF `info`, or the AEAD associated
/// data carries this prefix, so two different field sets never serialize identically.
pub(crate) fn length_prefix(length: usize) -> Result<[u8; 8], VaultError> {
    u64::try_from(length).map(u64::to_le_bytes).map_err(|_| VaultError::internal("length prefix"))
}

/// The wallet id is length-prefixed because it is the only variable-length
/// component of the `info` string, so no two ids can produce the same bytes.
fn expand(
    hkdf: &Hkdf<Sha256>,
    info: &'static [u8],
    wallet_id: &str,
) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let length = length_prefix(wallet_id.len())?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hkdf.expand_multi_info(&[info, &length, wallet_id.as_bytes()], key.as_mut())
        .map_err(|_| VaultError::internal("hkdf expand"))?;
    Ok(key)
}

/// An HMAC-SHA-256 in progress over the canonical header encoding.
///
/// Fed field by field rather than over a materialized buffer, so the ciphertext
/// is never copied just to be authenticated.
pub(crate) struct HeaderMac(Hmac<Sha256>);

impl HeaderMac {
    pub(crate) fn new(key: &[u8; KEY_LEN]) -> Result<Self, VaultError> {
        Hmac::<Sha256>::new_from_slice(key).map(Self).map_err(|_| VaultError::internal("hmac key"))
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// One [`length_prefix`]ed field.
    pub(crate) fn update_field(&mut self, bytes: &[u8]) -> Result<(), VaultError> {
        self.0.update(&length_prefix(bytes.len())?);
        self.0.update(bytes);
        Ok(())
    }

    pub(crate) fn finish(self) -> [u8; MAC_LEN] {
        self.0.finalize().into_bytes().into()
    }

    /// Constant-time comparison, delegated to the MAC so the tag is never
    /// handed around.
    #[must_use]
    pub(crate) fn verify(self, tag: &[u8; MAC_LEN]) -> bool {
        self.0.verify_slice(tag).is_ok()
    }
}

impl fmt::Debug for HeaderMac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HeaderMac(<in progress>)")
    }
}

/// The AEAD of one unlocked session. Built once and held for the session.
/// `chacha20poly1305`'s `zeroize` feature wipes the held key on drop.
pub(crate) struct SessionCipher(XChaCha20Poly1305);

impl SessionCipher {
    pub(crate) fn new(key: &[u8; KEY_LEN]) -> Self {
        Self(XChaCha20Poly1305::new(key.into()))
    }

    /// Seals in place. A `plaintext` without spare capacity for [`TAG_LEN`] more bytes is
    /// refused, because appending the tag would reallocate and strand an unwipeable copy
    /// of the plaintext in freed heap.
    pub(crate) fn seal(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        mut plaintext: Zeroizing<Vec<u8>>,
    ) -> Result<Vec<u8>, VaultError> {
        if plaintext.capacity().saturating_sub(plaintext.len()) < TAG_LEN {
            return Err(VaultError::internal("aead seal capacity"));
        }
        self.0
            .encrypt_in_place(&XNonce::from(*nonce), aad, &mut *plaintext)
            .map_err(|_| VaultError::internal("aead seal"))?;
        Ok(mem::take(&mut *plaintext))
    }

    /// `None` when the Poly1305 tag does not verify.
    pub(crate) fn open(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Option<Zeroizing<Vec<u8>>> {
        let mut plaintext = Zeroizing::new(ciphertext.to_vec());
        self.0.decrypt_in_place(&XNonce::from(*nonce), aad, &mut *plaintext).ok()?;
        Some(plaintext)
    }
}

impl fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionCipher(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;

    use super::*;
    use crate::keychain::MASTER_KEY_LEN;

    const WALLET_ID: &str = "wallet-1";
    const SALT: [u8; HKDF_SALT_LEN] = [0x33; HKDF_SALT_LEN];

    fn master(byte: u8) -> MasterKey {
        MasterKey::from_bytes([byte; MASTER_KEY_LEN])
    }

    fn buffer(plaintext: &[u8]) -> Zeroizing<Vec<u8>> {
        let mut buffered =
            Zeroizing::new(Vec::with_capacity(plaintext.len().saturating_add(TAG_LEN)));
        buffered.extend_from_slice(plaintext);
        buffered
    }

    /// Pins the HKDF wiring the on-disk format depends on, which is the per-file extract
    /// salt, the input keying material layout, and an `info` of the domain constant plus
    /// the length-prefixed wallet id.
    #[test]
    fn the_documented_salt_ikm_and_info_are_what_is_used() {
        let master_bytes = [0x11u8; MASTER_KEY_LEN];
        let password_hash = [0x22u8; KEY_LEN];
        let keys =
            password_keys(&MasterKey::from_bytes(master_bytes), &password_hash, &SALT, WALLET_ID)
                .unwrap();
        let mac = header_mac_key(&MasterKey::from_bytes(master_bytes), &SALT, WALLET_ID).unwrap();

        let length = u64::try_from(WALLET_ID.len()).unwrap().to_le_bytes();
        let ikm = [master_bytes, password_hash].concat();

        let expect = |hkdf: &Hkdf<Sha256>, info: &[u8]| {
            let mut key = [0u8; KEY_LEN];
            hkdf.expand_multi_info(&[info, &length, WALLET_ID.as_bytes()], &mut key).unwrap();
            key
        };
        let both = Hkdf::<Sha256>::new(Some(&SALT), &ikm);
        let master_only = Hkdf::<Sha256>::new(Some(&SALT), &master_bytes);

        assert_eq!(*keys.aead_key, expect(&both, AEAD_INFO));
        assert_eq!(keys.commitment, expect(&both, COMMIT_INFO));
        assert_eq!(*mac.expose_secret(), expect(&master_only, HMAC_INFO));
    }

    #[test]
    fn the_three_outputs_are_independent() {
        let keys = password_keys(&master(1), &[2u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        let mac = header_mac_key(&master(1), &SALT, WALLET_ID).unwrap();
        assert_ne!(*keys.aead_key, keys.commitment);
        assert_ne!(*keys.aead_key, *mac.expose_secret());
        assert_ne!(keys.commitment, *mac.expose_secret());
    }

    #[test]
    fn the_aead_key_and_the_commitment_depend_on_the_password() {
        let first = password_keys(&master(1), &[2u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        let second = password_keys(&master(1), &[3u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        assert_ne!(*first.aead_key, *second.aead_key);
        assert_ne!(first.commitment, second.commitment);
    }

    #[test]
    fn the_master_key_the_salt_and_the_wallet_id_all_bind() {
        let base = password_keys(&master(1), &[2u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        let other_master = password_keys(&master(2), &[2u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        let other_salt =
            password_keys(&master(1), &[2u8; KEY_LEN], &[8u8; HKDF_SALT_LEN], WALLET_ID).unwrap();
        let other_wallet = password_keys(&master(1), &[2u8; KEY_LEN], &SALT, "wallet-2").unwrap();
        for other in [&other_master, &other_salt, &other_wallet] {
            assert_ne!(*base.aead_key, *other.aead_key);
            assert_ne!(base.commitment, other.commitment);
        }
        let base_mac = header_mac_key(&master(1), &SALT, WALLET_ID).unwrap();
        assert_ne!(
            base_mac.expose_secret(),
            header_mac_key(&master(2), &SALT, WALLET_ID).unwrap().expose_secret()
        );
        assert_ne!(
            base_mac.expose_secret(),
            header_mac_key(&master(1), &[8u8; HKDF_SALT_LEN], WALLET_ID).unwrap().expose_secret()
        );
        assert_ne!(
            base_mac.expose_secret(),
            header_mac_key(&master(1), &SALT, "wallet-2").unwrap().expose_secret()
        );
    }

    /// The header tag is exactly HMAC-SHA-256 over the fed bytes under a
    /// 32-byte key, with no framing of its own beyond the length prefixes the
    /// caller asks for.
    #[test]
    fn the_header_tag_is_plain_hmac_sha256() {
        let key = [0x5au8; KEY_LEN];
        let mut mac = HeaderMac::new(&key).unwrap();
        mac.update(b"domain");
        mac.update_field(b"field").unwrap();
        let got = mac.finish();

        let mut reference = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        reference.update(b"domain");
        reference.update(&5u64.to_le_bytes());
        reference.update(b"field");
        let want: [u8; MAC_LEN] = reference.finalize().into_bytes().into();
        assert_eq!(got, want);

        let mut mac = HeaderMac::new(&key).unwrap();
        mac.update(b"domain");
        mac.update_field(b"field").unwrap();
        assert!(mac.verify(&want));

        let mut mac = HeaderMac::new(&[0x5b; KEY_LEN]).unwrap();
        mac.update(b"domain");
        mac.update_field(b"field").unwrap();
        assert!(!mac.verify(&want));
    }

    #[test]
    fn adjacent_fields_cannot_be_confused() {
        let render = |left: &[u8], right: &[u8]| {
            let mut mac = HeaderMac::new(&[0u8; KEY_LEN]).unwrap();
            mac.update_field(left).unwrap();
            mac.update_field(right).unwrap();
            mac.finish()
        };
        assert_ne!(render(b"ab", b"c"), render(b"a", b"bc"));
    }

    #[test]
    fn the_aead_binds_its_key_its_nonce_and_its_aad() {
        let cipher = SessionCipher::new(&[7u8; KEY_LEN]);
        let nonce = [4u8; NONCE_LEN];
        let sealed = cipher.seal(&nonce, b"wallet-a", buffer(b"payload")).unwrap();
        assert_eq!(sealed.len(), b"payload".len().saturating_add(TAG_LEN));
        assert_eq!(cipher.open(&nonce, b"wallet-a", &sealed).unwrap().as_slice(), b"payload");
        assert!(cipher.open(&nonce, b"wallet-b", &sealed).is_none());
        assert!(cipher.open(&[5u8; NONCE_LEN], b"wallet-a", &sealed).is_none());
        assert!(SessionCipher::new(&[9u8; KEY_LEN]).open(&nonce, b"wallet-a", &sealed).is_none());
    }

    #[test]
    fn a_plaintext_without_room_for_the_tag_is_refused() {
        let cipher = SessionCipher::new(&[7u8; KEY_LEN]);
        assert!(matches!(
            cipher.seal(&[4u8; NONCE_LEN], b"wallet-a", Zeroizing::new(Vec::new())),
            Err(VaultError::Internal { .. })
        ));
    }

    #[test]
    fn commitments_compare_by_value() {
        assert!(commitment_matches(&[1u8; COMMITMENT_LEN], &[1u8; COMMITMENT_LEN]));
        let mut other = [1u8; COMMITMENT_LEN];
        *other.last_mut().unwrap() = 2;
        assert!(!commitment_matches(&[1u8; COMMITMENT_LEN], &other));
    }

    #[test]
    fn the_session_cipher_password_keys_and_header_mac_redact() {
        assert_eq!(
            format!("{:?}", SessionCipher::new(&[1u8; KEY_LEN])),
            "SessionCipher(<redacted>)"
        );
        let keys = password_keys(&master(1), &[2u8; KEY_LEN], &SALT, WALLET_ID).unwrap();
        let rendered = format!("{keys:?}");
        assert_eq!(rendered, "PasswordKeys(<redacted>)");
        assert!(!rendered.contains(&hex::encode(*keys.aead_key)));
        assert_eq!(
            format!("{:?}", HeaderMac::new(&[1u8; KEY_LEN]).unwrap()),
            "HeaderMac(<in progress>)"
        );
    }

    /// Editing any of these bricks every existing vault.
    #[test]
    fn the_hkdf_info_strings_are_pinned() {
        assert_eq!(AEAD_INFO, b"jelly-vault/v3/aead");
        assert_eq!(HMAC_INFO, b"jelly-vault/v3/hmac");
        assert_eq!(COMMIT_INFO, b"jelly-vault/v3/commit");
    }
}
