use jelly_core::{KeyError, ScriptKind};
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::{
    mnemonic::KeyPair, ton_wallet::TonWallet, wallet_version::WalletVersion,
};

use crate::address::non_bounceable;
use crate::config::CHAIN;

jelly_core::wire_enum! {
    /// The wallet contract versions one key can hold Gram in, the default first. A
    /// payment stores its version, so the strings are a stored format.
    pub(crate) enum Version {
        V5r1 => "v5r1",
        V4r2 => "v4r2",
        V3r2 => "v3r2",
    }
}

impl TryFrom<ScriptKind> for Version {
    type Error = KeyError;

    fn try_from(script: ScriptKind) -> Result<Self, KeyError> {
        match script {
            ScriptKind::TonV5r1 => Ok(Self::V5r1),
            ScriptKind::TonV4r2 => Ok(Self::V4r2),
            ScriptKind::TonV3r2 => Ok(Self::V3r2),
            ScriptKind::P2tr | ScriptKind::P2wpkh | ScriptKind::P2shP2wpkh | ScriptKind::P2pkh => {
                Err(KeyError::ScriptUnsupported { chain: CHAIN, script })
            }
        }
    }
}

impl Version {
    pub(crate) const fn wallet_version(self) -> WalletVersion {
        match self {
            Self::V5r1 => WalletVersion::V5R1,
            Self::V4r2 => WalletVersion::V4R2,
            Self::V3r2 => WalletVersion::V3R2,
        }
    }

    /// The version's name as the TON ecosystem writes it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::V5r1 => "V5R1",
            Self::V4r2 => "V4R2",
            Self::V3r2 => "V3R2",
        }
    }
}

/// The wallet of `version` that `pubkey` controls, under the version's default wallet
/// id.
///
/// The wallet carries no secret key, so it must never sign for itself. Signing goes
/// through [`jelly_keypair::Ed25519Key::sign`].
pub(crate) fn ton_wallet(pubkey: &[u8; 32], version: Version) -> Result<TonWallet, KeyError> {
    TonWallet::new(
        version.wallet_version(),
        KeyPair { public_key: pubkey.to_vec(), secret_key: Vec::new() },
    )
    .map_err(|_| KeyError::internal("ton wallet state init"))
}

/// A wallet's own address is written non-bounceable, because the wallet is not deployed
/// until its first send, and a bounceable transfer to it would return to the sender.
pub(crate) fn wallet_address(pubkey: &[u8; 32], version: Version) -> Result<String, KeyError> {
    Ok(non_bounceable(&ton_wallet(pubkey, version)?.address))
}

/// The version whose wallet for `pubkey` sits at `address`, if any does.
pub(crate) fn version_at(
    pubkey: &[u8; 32],
    address: &TonAddress,
) -> Result<Option<Version>, KeyError> {
    for version in Version::ALL {
        if ton_wallet(pubkey, version)?.address == *address {
            return Ok(Some(version));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_version_is_found_at_its_own_address_and_no_other() {
        let pubkey = [7; 32];
        for version in Version::ALL {
            let address = ton_wallet(&pubkey, version).unwrap().address;
            assert_eq!(version_at(&pubkey, &address).unwrap(), Some(version));
            assert_eq!(version_at(&[8; 32], &address).unwrap(), None);
        }
    }

    #[test]
    fn only_the_ton_script_kinds_name_a_version() {
        assert_eq!(
            [ScriptKind::TonV5r1, ScriptKind::TonV4r2, ScriptKind::TonV3r2]
                .map(|script| Version::try_from(script).unwrap()),
            Version::ALL
        );
        assert!(matches!(
            Version::try_from(ScriptKind::P2wpkh),
            Err(KeyError::ScriptUnsupported { script: ScriptKind::P2wpkh, .. })
        ));
    }
}
