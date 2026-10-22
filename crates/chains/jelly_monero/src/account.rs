use std::fmt;

use monero_address::SubaddressIndex;
use monero_wallet::ed25519::{Commitment, CompressedPoint, Point, Scalar};
use monero_wallet::{ViewPair, WalletOutput};
use zeroize::Zeroizing;

use crate::error::MoneroError;
use crate::keys::{MoneroKeys, subaddress_secret};
use crate::subaddress::Subaddress;

const VIEW_NOT_REDUCED: &str = "the view key is not a reduced ed25519 scalar";
const SPEND_NOT_REDUCED: &str = "the spend key is not a reduced ed25519 scalar";

/// Monero account keys retained only while unlocked. Local scanning needs the view scalar.
/// Spend detection and signing also need the spend scalar.
///
/// Construction validates reduced scalars and a torsion-free public spend key.
#[cfg_attr(feature = "test-util", derive(Clone))]
pub struct MoneroAccount {
    /// The key hierarchy, which alone derives this account's addresses and the
    /// offset a subaddress's spend key carries.
    keys: MoneroKeys,
    view_pair: ViewPair,
    spend: Zeroizing<Scalar>,
}

impl fmt::Debug for MoneroAccount {
    /// Only public metadata and redacted secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoneroAccount")
            .field("public_spend", &hex::encode(self.public_spend()))
            .field("spend_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl TryFrom<MoneroKeys> for MoneroAccount {
    type Error = MoneroError;

    fn try_from(keys: MoneroKeys) -> Result<Self, MoneroError> {
        let (view_pair, spend) = keys.with_view_bytes(|view| {
            keys.with_spend_bytes(|spend| wallet_keys(keys.public_spend(), view, spend))
        })?;
        Ok(Self { keys, view_pair, spend })
    }
}

/// The scanning pair and the spend scalar in monero-wallet's own types.
fn wallet_keys(
    public_spend: [u8; 32],
    view: &[u8; 32],
    spend: &[u8; 32],
) -> Result<(ViewPair, Zeroizing<Scalar>), MoneroError> {
    let point = CompressedPoint::from(public_spend)
        .decompress()
        .ok_or(MoneroError::Key { what: "the public spend key is not a curve point" })?;
    let view_scalar = Zeroizing::new(read_scalar(view, VIEW_NOT_REDUCED)?);
    let view_pair = ViewPair::new(point, view_scalar)
        .map_err(|_| MoneroError::Key { what: "the public spend key is torsioned" })?;
    let spend = Zeroizing::new(read_scalar(spend, SPEND_NOT_REDUCED)?);
    Ok((view_pair, spend))
}

impl MoneroAccount {
    /// The account identity. The persisted scan blob is bound to it, so a blob
    /// from another account is refused rather than mis-attributed.
    pub fn public_spend(&self) -> [u8; 32] {
        self.view_pair.spend().compress().to_bytes()
    }

    /// The scanning pair. It is cloned into each [`monero_wallet::Scanner`],
    /// which takes ownership.
    pub(crate) const fn view_pair(&self) -> &ViewPair {
        &self.view_pair
    }

    /// `(major, minor)` as a mainnet address, as [`MoneroKeys::address`] derives it.
    pub fn address(&self, major: u32, minor: u32) -> Result<String, MoneroError> {
        self.keys.address(major, minor).map_err(|_| MoneroError::Internal {
            context: "an address of this account does not encode",
        })
    }

    /// The account's signing scalar, recovered from `key`, which is the spend key
    /// behind the address at `subaddress`. That is the account's own spend key at
    /// the primary address and `ks + m` anywhere else, so the offset comes off
    /// before the result has to be the private key behind this account's public
    /// spend key. Nothing but the key the caller supplied is ever signed with.
    ///
    /// `k G` is computed without a public scalar-basepoint multiplication, since a
    /// Pedersen commitment to zero with mask `k` is `k G + 0 H`.
    #[expect(clippy::arithmetic_side_effects, reason = "ed25519 scalar arithmetic is total")]
    pub(crate) fn signing_key(
        &self,
        key: &Zeroizing<[u8; 32]>,
        subaddress: Subaddress,
    ) -> Result<Zeroizing<Scalar>, MoneroError> {
        let held = Zeroizing::new(read_scalar(key, SPEND_NOT_REDUCED)?);
        let scalar = match subaddress.index() {
            None => held,
            Some(index) => {
                let offset = self.subaddress_offset(index)?;
                Zeroizing::new(Scalar::from(Scalar::into(*held) - Scalar::into(*offset)))
            }
        };
        let public = Commitment::new(*scalar, 0).commit().compress().to_bytes();
        if public != self.public_spend() {
            return Err(MoneroError::KeyNotOwner);
        }
        Ok(scalar)
    }

    /// What a subaddress's spend key adds to the account's, as [`subaddress_secret`]
    /// derives it.
    fn subaddress_offset(&self, index: SubaddressIndex) -> Result<Zeroizing<Scalar>, MoneroError> {
        let offset = self
            .keys
            .with_view_bytes(|view| subaddress_secret(view, index.account(), index.address()));
        Scalar::read(&mut offset.as_bytes().as_slice()).map(Zeroizing::new).map_err(|_| {
            MoneroError::Internal { context: "a subaddress offset is not a reduced scalar" }
        })
    }

    /// Computes an owned output's key image as `(ks + key_offset) Hp(P)`.
    ///
    /// The hidden `monero-ed25519` conversions are used because the crate has no
    /// public arbitrary-point multiplication. Its version is pinned, and vectors
    /// check the result against the library's signing path.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "ed25519 scalar and point arithmetic is total"
    )]
    pub fn key_image(&self, output: &WalletOutput) -> [u8; 32] {
        let input_key =
            Zeroizing::new(Scalar::into(*self.spend) + Scalar::into(output.key_offset()));
        let hash_point = Point::into(Point::biased_hash(output.key().compress().to_bytes()));
        Point::from(*input_key * hash_point).compress().to_bytes()
    }
}

/// Reads a canonically encoded ed25519 scalar.
///
/// Every key [`MoneroKeys`] produces is already reduced, so a failure means the caller
/// supplied something that is not a Monero key. `what` is always a literal, so no
/// fragment of a key can reach the message.
fn read_scalar(bytes: &[u8; 32], what: &'static str) -> Result<Scalar, MoneroError> {
    Scalar::read(&mut bytes.as_slice()).map_err(|_| MoneroError::Key { what })
}

#[cfg(test)]
mod tests {
    use monero_address::Network;
    use serde::Deserialize;

    use super::*;
    use crate::fixtures::{SPEND_KEY, account};

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AddressVectors {
        spend_key: String,
        primary: String,
        rows: Vec<Row>,
    }

    #[derive(Deserialize)]
    struct Row {
        major: u32,
        minor: u32,
        address: String,
    }

    #[test]
    fn the_account_agrees_with_the_key_hierarchy_it_was_built_from() {
        let keys = MoneroKeys::from_spend_bytes(&SPEND_KEY);
        let account = account();
        assert_eq!(account.public_spend(), keys.public_spend());
        assert_eq!(account.view_pair().view().compress().to_bytes(), keys.public_view());
    }

    #[test]
    fn zero_zero_is_the_primary_address_and_everything_else_is_a_subaddress() {
        let account = account();
        let address = |major, minor| account.address(major, minor).expect("an address");
        assert!(address(0, 0).starts_with('4'));
        assert!(address(0, 1).starts_with('8'));
        assert!(address(1, 0).starts_with('8'));
        assert_ne!(address(1, 0), address(0, 1));
    }

    /// The scanner finds outputs through its own view pair, so it must watch exactly the
    /// addresses the account hands out, wallet2's published ones included.
    #[test]
    fn the_scanner_watches_the_addresses_the_account_hands_out() {
        let vectors: AddressVectors =
            serde_json::from_str(include_str!("../tests/vectors/subaddresses.json"))
                .expect("the vector file parses");
        let spend: [u8; 32] =
            hex::decode(&vectors.spend_key).expect("hex").try_into().expect("a 32-byte spend key");
        let account =
            MoneroAccount::try_from(MoneroKeys::from_spend_bytes(&spend)).expect("usable keys");
        let primary = account.view_pair().legacy_address(Network::Mainnet).to_string();
        assert_eq!(primary, vectors.primary);
        assert_eq!(account.address(0, 0).expect("an address"), primary);
        for row in &vectors.rows {
            let index = SubaddressIndex::new(row.major, row.minor).expect("a subaddress index");
            let scanned = account.view_pair().subaddress(Network::Mainnet, index).to_string();
            assert_eq!(scanned, row.address, "({}, {})", row.major, row.minor);
            assert_eq!(account.address(row.major, row.minor).expect("an address"), scanned);
        }
    }

    #[test]
    fn a_public_spend_key_off_the_curve_or_an_unreduced_scalar_is_refused() {
        assert!(matches!(
            wallet_keys([0xff; 32], &[1u8; 32], &SPEND_KEY),
            Err(MoneroError::Key { .. })
        ));
        let keys = MoneroKeys::from_spend_bytes(&SPEND_KEY);
        assert!(matches!(
            wallet_keys(keys.public_spend(), &[0xff; 32], &SPEND_KEY),
            Err(MoneroError::Key { .. })
        ));
    }

    #[test]
    fn only_this_accounts_own_spend_key_signs_for_it() {
        let account = account();
        let scalar = account
            .signing_key(&Zeroizing::new(SPEND_KEY), Subaddress::PRIMARY)
            .expect("its own key");
        assert_eq!(*scalar, read_scalar(&SPEND_KEY, SPEND_NOT_REDUCED).expect("a reduced key"));
        let mut other = SPEND_KEY;
        other[0] ^= 1;
        assert!(matches!(
            account.signing_key(&Zeroizing::new(other), Subaddress::PRIMARY),
            Err(MoneroError::KeyNotOwner)
        ));
        assert!(matches!(
            account.signing_key(&Zeroizing::new([0xff; 32]), Subaddress::PRIMARY),
            Err(MoneroError::Key { .. })
        ));
    }

    #[test]
    fn the_key_behind_a_subaddress_signs_for_that_subaddress_and_no_other() {
        let account = account();
        let keys = MoneroKeys::from_spend_bytes(&SPEND_KEY);
        let own = read_scalar(&SPEND_KEY, SPEND_NOT_REDUCED).expect("a reduced key");
        for (major, minor) in [(0u32, 1u32), (1, 0), (2, 7)] {
            let held = keys.key_material_at(major, minor).with_bytes(|bytes| {
                Zeroizing::new(<[u8; 32]>::try_from(bytes).expect("a 32-byte key"))
            });
            let subaddress = Subaddress::new(major, minor);
            let scalar = account.signing_key(&held, subaddress).expect("the key behind it");
            assert_eq!(*scalar, own, "({major}, {minor}) recovers the account's spend key");
            assert!(
                matches!(
                    account.signing_key(&held, Subaddress::PRIMARY),
                    Err(MoneroError::KeyNotOwner)
                ),
                "({major}, {minor})'s key is not the primary address's"
            );
            assert!(
                matches!(
                    account.signing_key(&held, Subaddress::new(major, minor.saturating_add(1))),
                    Err(MoneroError::KeyNotOwner)
                ),
                "({major}, {minor})'s key is not its neighbour's"
            );
        }
        assert!(matches!(
            account.signing_key(&Zeroizing::new(SPEND_KEY), Subaddress::new(0, 1)),
            Err(MoneroError::KeyNotOwner)
        ));
    }

    #[test]
    fn a_debug_line_carries_the_identity_and_neither_secret() {
        let rendered = format!("{:?}", account());
        assert!(rendered.contains("redacted"), "{rendered}");
        assert!(rendered.contains(&hex::encode(account().public_spend())), "{rendered}");
        assert!(!rendered.contains(&hex::encode(SPEND_KEY)), "{rendered}");
        let view =
            MoneroKeys::from_spend_bytes(&SPEND_KEY).with_view_bytes(|bytes| hex::encode(bytes));
        assert!(!rendered.contains(&view), "{rendered}");
    }
}
