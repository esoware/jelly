use bitcoin::hashes::{Hash as _, hash160};
use bs58::Alphabet;
use jelly_core::{Address, AddressCodec, AddressError, ChainId};

use crate::config::CHAIN;

pub(crate) const CODEC: &dyn AddressCodec = &XrpCodec;
const INVALID: AddressError = AddressError::Invalid { chain: CHAIN };

#[derive(Debug)]
struct XrpCodec;

impl AddressCodec for XrpCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        let XrpDestination { account, tag } = xrp_destination(value)?;
        let canonical = match tag {
            Some(tag) => x_address(&account, tag),
            None => xrp_address(&account),
        };
        Ok(Address::assume_canonical(CHAIN, canonical))
    }
}

/// Where an XRP address, X-address, or `xrpl:` request pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct XrpDestination {
    pub(crate) account: [u8; 20],
    pub(crate) tag: Option<u32>,
}

pub(crate) fn xrp_address(account: &[u8; 20]) -> String {
    let mut bytes = [0; 21];
    let [_, body @ ..] = &mut bytes;
    *body = *account;
    bs58::encode(bytes).with_alphabet(Alphabet::RIPPLE).with_check().into_string()
}

/// The mainnet X-address of `account` with `tag`, whose flag byte marks the tag present
/// and whose last four bytes are reserved as zero.
fn x_address(account: &[u8; 20], tag: u32) -> String {
    let mut bytes = Vec::with_capacity(31);
    bytes.extend_from_slice(&[0x05, 0x44]);
    bytes.extend_from_slice(account);
    bytes.push(1);
    bytes.extend_from_slice(&tag.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bs58::encode(bytes).with_alphabet(Alphabet::RIPPLE).with_check().into_string()
}

/// The classic address of a SEC1 compressed public key.
pub(crate) fn pubkey_address(pubkey: &[u8; 33]) -> String {
    xrp_address(&hash160::Hash::hash(pubkey).to_byte_array())
}

/// Parses a classic address, X-address, or payment URI, keeping any tag it carries. A
/// URI's tag must agree with the X-address it names.
pub(crate) fn xrp_destination(value: &str) -> Result<XrpDestination, AddressError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    let (target, uri_tag) = match value.split_once(':') {
        Some((scheme, rest)) => payment_request(scheme, rest)?,
        None => (value, None),
    };
    let XrpDestination { account, tag: embedded_tag } = decode_address(target)?;
    if let (Some(embedded), Some(explicit)) = (embedded_tag, uri_tag)
        && embedded != explicit
    {
        return Err(INVALID);
    }
    Ok(XrpDestination { account, tag: embedded_tag.or(uri_tag) })
}

/// The address an `xrpl:` or `ripple:` request pays, and the destination tag it names.
fn payment_request<'a>(
    scheme: &str,
    rest: &'a str,
) -> Result<(&'a str, Option<u32>), AddressError> {
    if !scheme.eq_ignore_ascii_case("xrpl") && !scheme.eq_ignore_ascii_case("ripple") {
        return Err(AddressError::WrongNetwork { chain: CHAIN });
    }
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut tag = None;
    for parameter in query.split('&').filter(|parameter| !parameter.is_empty()) {
        let (name, text) = parameter.split_once('=').ok_or(INVALID)?;
        match name {
            "dt" | "destination_tag" => {
                if tag.is_some()
                    || text.is_empty()
                    || !text.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(INVALID);
                }
                tag = Some(text.parse::<u32>().map_err(|_| INVALID)?);
            }
            "amount" | "label" | "message" => {}
            _ => return Err(AddressError::UnsupportedRequest { chain: CHAIN }),
        }
    }
    Ok((target, tag))
}

/// The account of a classic address or mainnet X-address, with the tag an X-address
/// carries.
fn decode_address(target: &str) -> Result<XrpDestination, AddressError> {
    if target.len() > 60 {
        return Err(AddressError::WrongLength { chain: CHAIN });
    }
    let bytes = bs58::decode(target)
        .with_alphabet(Alphabet::RIPPLE)
        .with_check(None)
        .into_vec()
        .map_err(|_| AddressError::Checksum { chain: CHAIN })?;
    if let Ok([0, account @ ..]) = <[u8; 21]>::try_from(bytes.as_slice()) {
        return Ok(XrpDestination { account, tag: None });
    }
    if let Ok([0x05, 0x44, account @ .., flag, a, b, c, d, 0, 0, 0, 0]) =
        <[u8; 31]>::try_from(bytes.as_slice())
    {
        let tag = match (flag, u32::from_le_bytes([a, b, c, d])) {
            (0, 0) => None,
            (1, tag) => Some(tag),
            _ => return Err(INVALID),
        };
        return Ok(XrpDestination { account, tag });
    }
    if bytes.starts_with(&[0x04, 0x93]) {
        return Err(AddressError::WrongNetwork { chain: CHAIN });
    }
    Err(INVALID)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSIC: &str = "rPEPPER7kfTD9w2To4CQk6UCfuHM9c6GDY";

    #[test]
    fn classic_and_tagged_requests_round_trip_without_losing_zero() {
        let destination = xrp_destination(CLASSIC).unwrap();
        assert_eq!(destination.tag, None);
        assert_eq!(xrp_address(&destination.account), CLASSIC);
        for tag in [0, 1, u32::MAX] {
            let request = format!("xrpl:{CLASSIC}?dt={tag}");
            let canonical = CODEC.validate(&request).unwrap();
            assert!(canonical.value().starts_with('X'));
            assert_eq!(
                xrp_destination(canonical.value()).unwrap(),
                XrpDestination { account: destination.account, tag: Some(tag) }
            );
        }
    }

    #[test]
    fn malformed_tags_and_unsupported_requests_are_refused() {
        let invalid = AddressError::Invalid { chain: CHAIN };
        let unsupported = AddressError::UnsupportedRequest { chain: CHAIN };
        for (query, refusal) in [
            ("dt=-1", invalid),
            ("dt=4294967296", invalid),
            ("dt=", invalid),
            ("dt=+1", invalid),
            ("dt=1&dt=2", invalid),
            ("memo=deposit", unsupported),
            ("dt=1&destination_tag=2", invalid),
        ] {
            assert_eq!(
                xrp_destination(&format!("xrpl:{CLASSIC}?{query}")),
                Err(refusal),
                "{query}"
            );
        }
    }

    #[test]
    fn a_mistyped_classic_address_fails_its_checksum() {
        assert_eq!(
            xrp_destination("rPEPPER7kfTD9w2To4CQk6UCfuHM9c6GDX"),
            Err(AddressError::Checksum { chain: CHAIN })
        );
    }

    #[test]
    fn reserved_x_address_bytes_and_hidden_tags_are_refused() {
        let account = xrp_destination(CLASSIC).unwrap().account;
        for (flag, tag, reserved) in [(0, 1u32, 0), (1, 0, 1), (2, 0, 0)] {
            let mut bytes = vec![0x05, 0x44];
            bytes.extend_from_slice(&account);
            bytes.push(flag);
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&[reserved, 0, 0, 0]);
            let encoded =
                bs58::encode(bytes).with_alphabet(Alphabet::RIPPLE).with_check().into_string();
            assert_eq!(
                xrp_destination(&encoded),
                Err(AddressError::Invalid { chain: CHAIN }),
                "({flag}, {tag}, {reserved})"
            );
        }
    }

    #[test]
    fn a_testnet_x_address_and_another_scheme_are_on_the_wrong_network() {
        let mut bytes = vec![0x04, 0x93];
        bytes.extend_from_slice(&xrp_destination(CLASSIC).unwrap().account);
        bytes.extend_from_slice(&[0; 9]);
        let testnet =
            bs58::encode(bytes).with_alphabet(Alphabet::RIPPLE).with_check().into_string();
        let wrong_network = AddressError::WrongNetwork { chain: CHAIN };
        assert_eq!(xrp_destination(&testnet), Err(wrong_network));
        assert_eq!(xrp_destination(&format!("bitcoin:{CLASSIC}")), Err(wrong_network));
    }
}
