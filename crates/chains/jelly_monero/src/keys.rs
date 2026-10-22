use std::fmt;

use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use jelly_core::{KeyError, KeyKind, KeyMaterial};
use jelly_keypair::keccak256;
use monero_address::{AddressType, MoneroAddress, Network};
use monero_ed25519::CompressedPoint;
use monero_seed::{Language, Seed};
use zeroize::Zeroizing;

use crate::params::CHAIN;

/// The C string `SubAddr` with its trailing NUL. wallet2 hashes `sizeof(prefix)` bytes,
/// so the NUL is part of the domain separator, and leaving it out produces subaddresses
/// no other wallet agrees with.
const SUBADDRESS_PREFIX: &[u8; 8] = b"SubAddr\0";

/// Monero is `CryptoNote`, not BIP-32. An account is two ed25519 scalar pairs, spend
/// `(ks, Ks)` and view `(kv, Kv)`, and its addresses are subaddresses rather than
/// further derivations.
///
/// Both secrets are reduced scalars wiped on drop, and the public keys are their
/// basepoint multiples. The public surface is bytes only, so no curve library type
/// crosses this crate's boundary.
#[cfg_attr(feature = "test-util", derive(Clone))]
pub struct MoneroKeys {
    spend: Zeroizing<Scalar>,
    view: Zeroizing<Scalar>,
    public_spend: EdwardsPoint,
    public_view: EdwardsPoint,
}

impl MoneroKeys {
    /// Reduces any 32 bytes to the spend scalar, then derives the view key from
    /// it. This is the SLIP-10 bridge from the seed phrase.
    pub fn from_spend_bytes(bytes: &[u8; 32]) -> Self {
        Self::from_spend_scalar(Scalar::from_bytes_mod_order(*bytes))
    }

    /// The account whose spend scalar `material` holds.
    pub(crate) fn from_key_material(material: &KeyMaterial) -> Result<Self, KeyError> {
        if material.kind() != KeyKind::MoneroSpend {
            return Err(KeyError::WrongKeyKind { chain: CHAIN });
        }
        material.with_bytes(|bytes| {
            let spend = <&[u8; 32]>::try_from(bytes)
                .map_err(|_| KeyError::InvalidKeyLength { got: bytes.len() })?;
            Ok(Self::from_spend_bytes(spend))
        })
    }

    /// wallet2's 25-word English seed, which carries the spend key only. The view key is
    /// recomputed from it on restore.
    pub(crate) fn to_seed_words(&self) -> Result<Zeroizing<String>, KeyError> {
        Seed::from_entropy(Language::English, Zeroizing::new(*self.spend.as_bytes()))
            .map(|seed| seed.to_string())
            .ok_or(KeyError::internal("a monero spend key is not a reduced scalar"))
    }

    pub(crate) fn with_spend_bytes<R>(&self, read: impl FnOnce(&[u8; 32]) -> R) -> R {
        read(self.spend.as_bytes())
    }

    /// Secret but shareable. The view key reveals every incoming transfer, spends
    /// nothing, and is what a watch-only scanner needs.
    pub fn with_view_bytes<R>(&self, read: impl FnOnce(&[u8; 32]) -> R) -> R {
        read(self.view.as_bytes())
    }

    pub(crate) fn spend_secret_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.spend.as_bytes()))
    }

    pub fn public_spend(&self) -> [u8; 32] {
        self.public_spend.compress().to_bytes()
    }

    pub fn public_view(&self) -> [u8; 32] {
        self.public_view.compress().to_bytes()
    }

    #[cfg(test)]
    pub(crate) fn key_material(&self) -> KeyMaterial {
        self.key_material_at(0, 0)
    }

    /// `(0, 0)` is the primary address, not a subaddress, and uses the account keys
    /// directly, as every Monero wallet does.
    pub fn address(&self, major: u32, minor: u32) -> Result<String, KeyError> {
        let (spend, view) = self.address_keys(major, minor);
        let kind =
            if is_primary(major, minor) { AddressType::Legacy } else { AddressType::Subaddress };
        encode_address(kind, &spend, &view)
    }

    /// The public key pair behind the address at `(major, minor)`, `(Ks, Kv)` at the
    /// primary index and `(D, C)` at any other.
    #[expect(clippy::arithmetic_side_effects, reason = "curve group operations are total")]
    fn address_keys(&self, major: u32, minor: u32) -> ([u8; 32], [u8; 32]) {
        if is_primary(major, minor) {
            return (self.public_spend(), self.public_view());
        }
        let m = subaddress_secret(self.view.as_bytes(), major, minor);
        let d = self.public_spend + EdwardsPoint::mul_base(&m);
        let c = *self.view * d;
        (d.compress().to_bytes(), c.compress().to_bytes())
    }

    /// The scalar that spends outputs received at `(major, minor)`, `ks` at the primary
    /// index and `ks + m` at any other.
    #[expect(clippy::arithmetic_side_effects, reason = "curve group operations are total")]
    fn spend_key_at(&self, major: u32, minor: u32) -> Zeroizing<[u8; 32]> {
        if is_primary(major, minor) {
            return Zeroizing::new(self.spend.to_bytes());
        }
        let m = subaddress_secret(self.view.as_bytes(), major, minor);
        Zeroizing::new((*self.spend + *m).to_bytes())
    }

    pub fn key_material_at(&self, major: u32, minor: u32) -> KeyMaterial {
        let secret = self.spend_key_at(major, minor);
        KeyMaterial::new(KeyKind::MoneroSpend, Zeroizing::new(secret.to_vec()))
    }

    /// `kv = sc_reduce32(keccak256(ks))`, which is what every deterministic
    /// Monero wallet does. Keccak-256 here is the original padding, not SHA3.
    fn from_spend_scalar(spend: Scalar) -> Self {
        let view = Scalar::from_bytes_mod_order(keccak256(spend.as_bytes()));
        let public_spend = EdwardsPoint::mul_base(&spend);
        let public_view = EdwardsPoint::mul_base(&view);
        Self { spend: Zeroizing::new(spend), view: Zeroizing::new(view), public_spend, public_view }
    }
}

impl fmt::Debug for MoneroKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MoneroKeys(<redacted>)")
    }
}

/// `m = Hs("SubAddr\0" || kv || major_le || minor_le)`, what the spend key behind the
/// address at `(major, minor)` adds to the account's. Secret, because it offsets the
/// spend key.
///
/// The view key is copied straight into a preimage sized for all of it, so no bare copy
/// of it is left behind.
pub(crate) fn subaddress_secret(view: &[u8; 32], major: u32, minor: u32) -> Zeroizing<Scalar> {
    let mut data = Zeroizing::new([0u8; 48]);
    let (prefix, rest) = data.split_at_mut(SUBADDRESS_PREFIX.len());
    let (view_bytes, indices) = rest.split_at_mut(32);
    let (major_bytes, minor_bytes) = indices.split_at_mut(4);
    prefix.copy_from_slice(SUBADDRESS_PREFIX);
    view_bytes.copy_from_slice(view);
    major_bytes.copy_from_slice(&major.to_le_bytes());
    minor_bytes.copy_from_slice(&minor.to_le_bytes());
    Zeroizing::new(Scalar::from_bytes_mod_order(keccak256(data.as_slice())))
}

const fn is_primary(major: u32, minor: u32) -> bool {
    major == 0 && minor == 0
}

/// Renders a mainnet address from a pair of compressed public keys.
fn encode_address(
    kind: AddressType,
    public_spend: &[u8; 32],
    public_view: &[u8; 32],
) -> Result<String, KeyError> {
    let spend =
        CompressedPoint::from(*public_spend).decompress().ok_or(KeyError::InvalidPublicKey)?;
    let view =
        CompressedPoint::from(*public_view).decompress().ok_or(KeyError::InvalidPublicKey)?;
    Ok(MoneroAddress::new(Network::Mainnet, kind, spend, view).to_string())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;
    use serde::Deserialize;

    use super::*;
    use crate::entry::ENTRY;
    use crate::fixtures::hd_account;

    const VECTORS_JSON: &str = include_str!("../tests/vectors/monero_seed.json");

    const PROPERTY_SEED: u64 = 20_261_021;

    #[derive(Deserialize)]
    struct Vectors {
        accounts: Vec<Account>,
        seed_to_hex: SeedToHex,
    }

    #[derive(Deserialize)]
    struct SeedToHex {
        phrase: String,
        hex: String,
    }

    #[derive(Deserialize)]
    struct Account {
        name: String,
        phrase: String,
        spend_secret: String,
        view_secret: String,
        public_spend: Option<String>,
        public_view: Option<String>,
        address: Option<String>,
        subaddresses: Vec<Subaddress>,
    }

    #[derive(Deserialize)]
    struct Subaddress {
        major: u32,
        minor: u32,
        address: String,
        public_spend: Option<String>,
        public_view: Option<String>,
    }

    fn vectors() -> Vectors {
        serde_json::from_str(VECTORS_JSON).expect("the vector file parses")
    }

    fn first_account(suite: &Vectors) -> &Account {
        suite.accounts.first().expect("at least one account vector")
    }

    fn vector_keys(account: &Account) -> MoneroKeys {
        let spend: [u8; 32] = hex::decode(&account.spend_secret)
            .expect("hex")
            .try_into()
            .expect("a 32-byte spend key");
        MoneroKeys::from_spend_bytes(&spend)
    }

    fn decode(address: &str) -> MoneroAddress {
        MoneroAddress::from_str(Network::Mainnet, address).expect("a mainnet address")
    }

    fn hd_keys(index: u32) -> MoneroKeys {
        MoneroKeys::from_key_material(&hd_account(index)).expect("an account")
    }

    #[test]
    fn every_published_account_derives_its_seed_keys_and_primary_address() {
        for account in &vectors().accounts {
            let keys = vector_keys(account);
            assert_eq!(
                keys.to_seed_words().expect("seed words").as_str(),
                account.phrase,
                "account {}",
                account.name
            );
            assert_eq!(keys.spend_secret_hex().as_str(), account.spend_secret);
            assert_eq!(
                keys.with_view_bytes(|bytes| hex::encode(bytes)),
                account.view_secret,
                "account {}",
                account.name
            );
            if let Some(want) = &account.public_spend {
                assert_eq!(hex::encode(keys.public_spend()), *want);
            }
            if let Some(want) = &account.public_view {
                assert_eq!(hex::encode(keys.public_view()), *want);
            }
            if let Some(want) = &account.address {
                assert_eq!(keys.address(0, 0).expect("an address"), *want, "(0, 0) is the primary");
                assert_eq!(ENTRY.validate_address(want).expect("validate").value(), *want);
                assert_eq!(
                    ENTRY.address(&keys.key_material(), None).expect("an address"),
                    *want,
                    "the entry renders the primary address of the account"
                );
                assert_eq!(want.len(), 95);
                assert!(want.starts_with('4'));
            }
        }
    }

    #[test]
    fn every_published_subaddress_derives_its_address_and_public_keys() {
        for account in &vectors().accounts {
            let keys = vector_keys(account);
            let material = keys.key_material();
            for subaddress in &account.subaddresses {
                assert_eq!(
                    keys.address(subaddress.major, subaddress.minor).expect("a subaddress"),
                    subaddress.address
                );
                assert_eq!(
                    ENTRY
                        .subaddress(&material, subaddress.major, subaddress.minor)
                        .expect("a subaddress"),
                    subaddress.address
                );
                assert!(subaddress.address.starts_with('8'));
                assert_eq!(subaddress.address.len(), 95);
                let (spend, view) = keys.address_keys(subaddress.major, subaddress.minor);
                if let Some(want) = &subaddress.public_spend {
                    assert_eq!(hex::encode(spend), *want);
                }
                if let Some(want) = &subaddress.public_view {
                    assert_eq!(hex::encode(view), *want);
                }
                let decoded = decode(&subaddress.address);
                assert!(matches!(decoded.kind(), AddressType::Subaddress));
                assert!(decoded.is_subaddress());
                assert_eq!(decoded.spend().compress().to_bytes(), spend);
                assert_eq!(decoded.view().compress().to_bytes(), view);
            }
        }
    }

    #[test]
    fn a_subaddress_secret_matches_its_public_key() {
        let suite = vectors();
        let keys = vector_keys(first_account(&suite));
        for (major, minor) in [(0u32, 0u32), (0, 1), (1, 0), (7, 9)] {
            let secret = keys.spend_key_at(major, minor);
            let (public_spend, _view) = keys.address_keys(major, minor);
            let derived = MoneroKeys::from_spend_bytes(&secret);
            assert_eq!(derived.public_spend(), public_spend, "({major}, {minor})");
        }
        // The primary index is the account key itself, not a subaddress offset.
        keys.with_spend_bytes(|spend| {
            assert_eq!(keys.spend_key_at(0, 0).as_slice(), spend);
            assert_ne!(keys.spend_key_at(0, 1).as_slice(), spend);
        });
    }

    #[test]
    fn an_unreduced_scalar_is_reduced_on_import() {
        let keys = MoneroKeys::from_spend_bytes(&[0xffu8; 32]);
        assert_eq!(
            keys.spend_secret_hex().as_str(),
            hex::encode(Scalar::from_bytes_mod_order([0xff; 32]).to_bytes())
        );
    }

    #[test]
    fn key_material_follows_the_subaddress() {
        let keys = hd_keys(0);
        for (major, minor) in [(0u32, 0u32), (0, 3), (2, 5)] {
            let material = keys.key_material_at(major, minor);
            assert_eq!(material.kind(), KeyKind::MoneroSpend);
            material
                .with_bytes(|bytes| assert_eq!(bytes, keys.spend_key_at(major, minor).as_slice()));
            ENTRY
                .subaddress_key(&keys.key_material(), major, minor)
                .expect("a subaddress key")
                .with_bytes(|bytes| assert_eq!(bytes, keys.spend_key_at(major, minor).as_slice()));
        }
        keys.key_material()
            .with_bytes(|bytes| keys.with_spend_bytes(|spend| assert_eq!(bytes, spend.as_slice())));
    }

    #[test]
    fn hd_accounts_are_distinct_and_interoperable() {
        let account_0 = hd_keys(0);
        let account_1 = hd_keys(1);
        assert_ne!(
            account_0.address(0, 0).expect("an address"),
            account_1.address(0, 0).expect("an address")
        );
        for (index, account) in [(0, &account_0), (1, &account_1)] {
            let address = account.address(0, 0).expect("an address");
            assert_eq!(ENTRY.validate_address(&address).expect("validate").value(), address);
            // A derived spend key is already a reduced scalar, so the seed words written
            // for it carry exactly the key the wider Monero ecosystem restores.
            hd_account(index).with_bytes(|derived| {
                account.with_spend_bytes(|spend| assert_eq!(derived, spend.as_slice()));
            });
        }
        assert_eq!(
            ENTRY.address(&hd_account(1), None).expect("an address"),
            account_1.address(0, 0).expect("an address")
        );
        assert_eq!(
            ENTRY.account_seed_words(&hd_account(0)).expect("seed words").as_str(),
            account_0.to_seed_words().expect("seed words").as_str()
        );
    }

    #[test]
    fn key_material_of_another_kind_or_length_is_refused() {
        let other = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![7; 32]));
        assert!(matches!(
            MoneroKeys::from_key_material(&other),
            Err(KeyError::WrongKeyKind { chain: CHAIN })
        ));
        let short = KeyMaterial::new(KeyKind::MoneroSpend, Zeroizing::new(vec![7; 31]));
        assert!(matches!(
            MoneroKeys::from_key_material(&short),
            Err(KeyError::InvalidKeyLength { got: 31 })
        ));
    }

    #[test]
    fn a_debug_line_carries_neither_secret_of_the_key_hierarchy() {
        let keys = hd_keys(0);
        let debug = format!("{keys:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains(keys.spend_secret_hex().as_str()));
        assert!(!debug.contains(&keys.with_view_bytes(|bytes| hex::encode(bytes))));
    }

    #[test]
    fn a_published_seed_and_its_key_convert_both_ways() {
        let vector = vectors().seed_to_hex;
        let spend: [u8; 32] = hex::decode(&vector.hex).expect("hex").try_into().expect("a key");
        let keys = MoneroKeys::from_spend_bytes(&spend);
        assert_eq!(keys.to_seed_words().expect("seed words").as_str(), vector.phrase);
        let seed = Seed::from_string(Language::English, Zeroizing::new(vector.phrase))
            .expect("a valid seed");
        assert_eq!(*seed.entropy(), spend);
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            rng_seed: RngSeed::Fixed(PROPERTY_SEED),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn every_seed_restores_the_spend_key_it_was_written_for(raw in prop::array::uniform32(any::<u8>())) {
            let keys = MoneroKeys::from_spend_bytes(&raw);
            let words = keys.to_seed_words().expect("seed words");
            prop_assert_eq!(words.split(' ').count(), 25);
            let seed = Seed::from_string(Language::English, words).expect("a valid seed");
            keys.with_spend_bytes(|spend| assert_eq!(*seed.entropy(), *spend));
        }

        #[test]
        fn subaddresses_decode_to_their_own_keys(major in 0u32..4, minor in 0u32..64) {
            let keys = MoneroKeys::from_spend_bytes(&[9u8; 32]);
            let address = keys.address(major, minor).expect("an address");
            let decoded = decode(&address);
            let (spend, view) = keys.address_keys(major, minor);
            prop_assert_eq!(decoded.spend().compress().to_bytes(), spend);
            prop_assert_eq!(decoded.view().compress().to_bytes(), view);
            prop_assert_eq!(decoded.is_subaddress(), !(major == 0 && minor == 0));
        }
    }
}
