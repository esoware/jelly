//! The password factor, Argon2id v1.3 per RFC 9106.

use std::fmt;

use argon2::{Algorithm, Argon2, Version};
use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroizing;

use crate::error::VaultError;
use crate::keys::KEY_LEN;

/// Argon2 v1.3, which the header writes as 19.
pub(crate) const ARGON2_VERSION: Version = Version::V0x13;

/// RFC 9106 recommends a 16-byte Argon2id salt.
pub(crate) const KDF_SALT_LEN: usize = 16;

/// Never logged or serialized.
///
/// Any password is accepted, even an empty one. Strength is the owner's call, and the
/// keychain master key is a second factor either way.
pub struct VaultPassword(SecretString);

impl VaultPassword {
    pub const fn new(value: SecretString) -> Self {
        Self(value)
    }
}

impl fmt::Debug for VaultPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultPassword(<redacted>)")
    }
}

/// Stored in the vault header, so a file written under an older baseline still opens after
/// the baseline is raised. The header tag covers them, so an edit can't downgrade them.
///
/// The KDF and the header parser both check [`Argon2Params::MINIMUM`] first, so a weaker
/// set can be built but never used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

impl Argon2Params {
    /// The shipped baseline, m=262144 KiB, t=3, p=4. That's RFC 9106 section 4's second
    /// recommended option with four times the memory.
    pub const RECOMMENDED: Self = Self { m_cost_kib: 262_144, t_cost: 3, p_cost: 4 };

    /// The floor a file's parameters must meet before the KDF runs over it. That's the
    /// OWASP Password Storage minimum, m=19456 KiB, t=2, p=1.
    pub const MINIMUM: Self = Self { m_cost_kib: 19_456, t_cost: 2, p_cost: 1 };

    pub const fn new(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Self {
        Self { m_cost_kib, t_cost, p_cost }
    }

    pub const fn m_cost_kib(self) -> u32 {
        self.m_cost_kib
    }

    pub const fn t_cost(self) -> u32 {
        self.t_cost
    }

    pub const fn p_cost(self) -> u32 {
        self.p_cost
    }

    /// Names the first axis below the floor. Argon2's own range check would accept much
    /// weaker sets.
    pub(crate) const fn check_minimum(self) -> Result<(), VaultError> {
        let field = if self.m_cost_kib < Self::MINIMUM.m_cost_kib {
            "m_cost_kib"
        } else if self.t_cost < Self::MINIMUM.t_cost {
            "t_cost"
        } else if self.p_cost < Self::MINIMUM.p_cost {
            "p_cost"
        } else {
            return Ok(());
        };
        Err(VaultError::WeakKdfParams { field })
    }

    /// Raises each axis to at least `floor`'s.
    ///
    /// An unlock upgrades a file to this, not to the handle's parameters, so no axis is ever
    /// lowered. A handle asking for more passes over a file with more memory gets both.
    pub(crate) fn at_least(self, floor: Self) -> Self {
        Self {
            m_cost_kib: self.m_cost_kib.max(floor.m_cost_kib),
            t_cost: self.t_cost.max(floor.t_cost),
            p_cost: self.p_cost.max(floor.p_cost),
        }
    }
}

/// Argon2 reads the password in place, so it's never copied, and the output lands in a
/// wiping buffer.
///
/// RFC 9106's secret-key and associated-data inputs go unused. The master key comes in
/// later, at the HKDF step.
pub(crate) fn hash_password(
    password: &VaultPassword,
    salt: &[u8; KDF_SALT_LEN],
    params: Argon2Params,
) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    params.check_minimum()?;
    let argon2_params =
        argon2::Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(KEY_LEN))?;
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, ARGON2_VERSION, argon2_params).hash_password_into(
        password.0.expose_secret().as_bytes(),
        salt,
        key.as_mut(),
    )?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; KDF_SALT_LEN] = [2u8; KDF_SALT_LEN];
    const PASSWORD: &str = "correct horse battery staple";
    const MINIMUM_TAG: &str = "b19c99d822ad52c690c95fcc4726153532bdae7e00720d2b1e4bb52db2d59120";
    const RECOMMENDED_TAG: &str =
        "8884716d371c945e36d6b83ce6497917697e41f92ec7595f6e42988661e860b3";

    fn password() -> VaultPassword {
        VaultPassword::new(SecretString::from(PASSWORD))
    }

    /// Pins the algorithm, version, parameter, and output-length wiring. The RFC 9106
    /// vector in `tests/it/vectors.rs` pins the primitive itself.
    #[test]
    fn the_output_is_pinned_at_both_shipped_parameter_sets() {
        let got = hash_password(&password(), &SALT, Argon2Params::MINIMUM).unwrap();
        assert_eq!(hex::encode(*got), MINIMUM_TAG);
        let got = hash_password(&password(), &SALT, Argon2Params::RECOMMENDED).unwrap();
        assert_eq!(hex::encode(*got), RECOMMENDED_TAG);
    }

    #[test]
    fn the_wiring_is_argon2id_version_0x13_with_a_32_byte_tag() {
        let got = hash_password(&password(), &SALT, Argon2Params::MINIMUM).unwrap();
        let reference = |algorithm, version| {
            let minimum = Argon2Params::MINIMUM;
            let params = argon2::Params::new(
                minimum.m_cost_kib,
                minimum.t_cost,
                minimum.p_cost,
                Some(KEY_LEN),
            )
            .unwrap();
            let mut key = [0u8; KEY_LEN];
            Argon2::new(algorithm, version, params)
                .hash_password_into(PASSWORD.as_bytes(), &SALT, &mut key)
                .unwrap();
            key
        };
        assert_eq!(*got, reference(Algorithm::Argon2id, Version::V0x13));
        assert_ne!(*got, reference(Algorithm::Argon2i, Version::V0x13));
        assert_ne!(*got, reference(Algorithm::Argon2d, Version::V0x13));
        assert_ne!(*got, reference(Algorithm::Argon2id, Version::V0x10));
    }

    #[test]
    fn the_hash_is_deterministic_and_moves_with_the_salt_and_the_password() {
        let first =
            hash_password(&password(), &[0u8; KDF_SALT_LEN], Argon2Params::MINIMUM).unwrap();
        let repeat =
            hash_password(&password(), &[0u8; KDF_SALT_LEN], Argon2Params::MINIMUM).unwrap();
        let other_salt =
            hash_password(&password(), &[1u8; KDF_SALT_LEN], Argon2Params::MINIMUM).unwrap();
        let other_password = hash_password(
            &VaultPassword::new(SecretString::from("correct horse battery stapld")),
            &[0u8; KDF_SALT_LEN],
            Argon2Params::MINIMUM,
        )
        .unwrap();
        assert_eq!(*first, *repeat);
        assert_ne!(*first, *other_salt);
        assert_ne!(*first, *other_password);
    }

    #[test]
    fn the_floor_is_enforced_before_the_algorithm_range_check() {
        Argon2Params::RECOMMENDED.check_minimum().unwrap();
        Argon2Params::MINIMUM.check_minimum().unwrap();
        for (params, expected) in [
            (Argon2Params::new(1_024, 3, 4), "m_cost_kib"),
            (Argon2Params::new(262_144, 1, 4), "t_cost"),
            (Argon2Params::new(262_144, 3, 0), "p_cost"),
        ] {
            match params.check_minimum() {
                Err(VaultError::WeakKdfParams { field }) => assert_eq!(field, expected),
                other => panic!("expected weak parameters for {expected}, got {other:?}"),
            }
        }
        assert!(matches!(
            hash_password(&password(), &SALT, Argon2Params::new(8, 1, 1)),
            Err(VaultError::WeakKdfParams { .. })
        ));
    }

    /// Above the floor isn't the same as valid. Argon2 also requires m >= 8p, and that
    /// refusal is reported as Argon2's, not as a weak set.
    #[test]
    fn a_set_the_algorithm_itself_refuses_is_reported_by_the_algorithm() {
        let params = Argon2Params::new(19_456, 2, 3_000);
        params.check_minimum().unwrap();
        assert!(matches!(hash_password(&password(), &SALT, params), Err(VaultError::KdfParams(_))));
    }

    #[test]
    fn raising_to_a_floor_never_lowers_an_axis() {
        let file = Argon2Params::new(1_048_576, 2, 1);
        let handle = Argon2Params::new(262_144, 3, 4);
        assert_eq!(file.at_least(handle), Argon2Params::new(1_048_576, 3, 4));
        assert_eq!(handle.at_least(file), Argon2Params::new(1_048_576, 3, 4));
        assert_eq!(file.at_least(file), file);
        assert_eq!(
            Argon2Params::MINIMUM.at_least(Argon2Params::RECOMMENDED),
            Argon2Params::RECOMMENDED
        );
        assert_eq!(
            Argon2Params::RECOMMENDED.at_least(Argon2Params::MINIMUM),
            Argon2Params::RECOMMENDED
        );
    }

    #[test]
    fn the_shipped_baseline_and_floor_are_pinned() {
        let recommended = Argon2Params::RECOMMENDED;
        assert_eq!(
            (recommended.m_cost_kib(), recommended.t_cost(), recommended.p_cost()),
            (262_144, 3, 4)
        );
        let minimum = Argon2Params::MINIMUM;
        assert_eq!((minimum.m_cost_kib(), minimum.t_cost(), minimum.p_cost()), (19_456, 2, 1));
        assert!(recommended.m_cost_kib() >= 65_536, "RFC 9106 second recommended option");
        assert!(
            recommended.m_cost_kib() >= 8u32.saturating_mul(recommended.p_cost()),
            "argon2 requires m >= 8p"
        );
    }
}
