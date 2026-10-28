use jelly_core::{Address, AddressCodec, AddressError, ChainId};
use tonlib_core::types::TonAddress;
use url::form_urlencoded;

use crate::config::{CHAIN, MAX_COMMENT_BYTES};

pub(crate) const CODEC: &dyn AddressCodec = &TonCodec;

#[derive(Debug)]
struct TonCodec;

impl AddressCodec for TonCodec {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    /// A raw address keeps its raw spelling, because it carries no bounce flag and the
    /// recipient's state decides one at send time.
    fn validate(&self, value: &str) -> Result<Address, AddressError> {
        let destination = ton_destination(value)?;
        Ok(Address::assume_canonical(CHAIN, destination.spelling()))
    }
}

/// Where a TON address or `ton://transfer/` request pays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TonDestination {
    pub(crate) address: TonAddress,
    /// The flag a user-friendly address carries, or `None` for a raw `0:...` address.
    pub(crate) is_bounceable: Option<bool>,
    pub(crate) comment: Option<String>,
}

impl TonDestination {
    pub(crate) fn spelling(&self) -> String {
        match self.is_bounceable {
            Some(is_bounceable) => self.address.to_base64_url_flags(!is_bounceable, false),
            None => self.address.to_hex(),
        }
    }
}

pub(crate) fn ton_destination(value: &str) -> Result<TonDestination, AddressError> {
    let invalid = AddressError::Invalid { chain: CHAIN };
    let unsupported = AddressError::UnsupportedRequest { chain: CHAIN };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty { chain: CHAIN });
    }
    let mut comment = None;
    let target = if let Some(rest) = trimmed.strip_prefix("ton://transfer/") {
        let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "text" if comment.is_none() && value.len() <= MAX_COMMENT_BYTES => {
                    comment = Some(value.into_owned());
                }
                "amount"
                    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {}
                _ => return Err(unsupported),
            }
        }
        target
    } else {
        trimmed
    };
    let (address, is_bounceable) = if let Some((workchain, _)) = target.split_once(':') {
        if !matches!(workchain, "0" | "-1") {
            return Err(AddressError::WrongNetwork { chain: CHAIN });
        }
        (TonAddress::from_hex_str(target).map_err(|_| invalid)?, None)
    } else {
        let (address, is_non_bounceable, is_testnet) = TonAddress::from_base64_url_flags(target)
            .or_else(|_| TonAddress::from_base64_std_flags(target))
            .map_err(|_| invalid)?;
        if is_testnet {
            return Err(AddressError::WrongNetwork { chain: CHAIN });
        }
        (address, Some(!is_non_bounceable))
    };
    if !matches!(address.workchain, 0 | -1) {
        return Err(AddressError::WrongNetwork { chain: CHAIN });
    }
    Ok(TonDestination { address, is_bounceable, comment })
}

/// The user-friendly mainnet spelling of `address`, flagged non-bounceable.
pub(crate) fn non_bounceable(address: &TonAddress) -> String {
    address.to_base64_url_flags(true, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::{Version, ton_wallet, wallet_address};

    #[test]
    fn friendly_flags_and_raw_addresses_preserve_the_account() {
        let address = ton_wallet(&[1; 32], Version::V5r1).unwrap().address;
        for is_non_bounceable in [false, true] {
            let friendly = address.to_base64_url_flags(is_non_bounceable, false);
            assert_eq!(
                ton_destination(&friendly).unwrap(),
                TonDestination {
                    address: address.clone(),
                    is_bounceable: Some(!is_non_bounceable),
                    comment: None
                }
            );
            assert_eq!(
                ton_destination(&address.to_base64_url_flags(is_non_bounceable, true)),
                Err(AddressError::WrongNetwork { chain: CHAIN })
            );
        }
        let raw = ton_destination(&address.to_hex()).unwrap();
        assert_eq!((raw.address.clone(), raw.is_bounceable), (address.clone(), None));
        assert_eq!(raw.spelling(), address.to_hex());
        assert_eq!(CODEC.validate(&address.to_hex()).unwrap().value(), address.to_hex());
    }

    #[test]
    fn a_payment_request_keeps_its_comment_and_refuses_fields_it_cannot_honor() {
        let address = wallet_address(&[1; 32], Version::V5r1).unwrap();
        let request = format!("ton://transfer/{address}?amount=123&text=order%207");
        assert_eq!(ton_destination(&request).unwrap().comment.as_deref(), Some("order 7"));
        for query in ["bin=AAAA", "init=AAAA", "text=one&text=two", "amount=-1"] {
            assert_eq!(
                ton_destination(&format!("ton://transfer/{address}?{query}")),
                Err(AddressError::UnsupportedRequest { chain: CHAIN }),
                "{query}"
            );
        }
    }
}
