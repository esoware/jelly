//! The two TRC-20 calls this build makes, in both spellings TRON needs.
//!
//! A contract call inside a transaction carries the whole calldata, selector
//! included. `triggerconstantcontract` instead takes the signature as a string
//! and the arguments as a bare hex blob, so both forms live here and share one
//! encoder.

use alloy_primitives::{Address, U256};
use alloy_sol_types::abi::AbiDecoderConfig;
use alloy_sol_types::{SolCall as _, sol};

use crate::address;
use crate::error::TronError;

sol! {
    /// TRC-20 is the ERC-20 ABI.
    interface ITRC20 {
        function balanceOf(address owner) external view returns (uint256);
        function transfer(address to, uint256 value) external returns (bool);
    }
}

pub(crate) const TRANSFER_SIGNATURE: &str = ITRC20::transferCall::SIGNATURE;
pub(crate) const BALANCE_OF_SIGNATURE: &str = ITRC20::balanceOfCall::SIGNATURE;

const SELECTOR_BYTES: usize = ITRC20::transferCall::SELECTOR.len();
const WORD_BYTES: usize = 32;
pub(crate) const TRANSFER_CALL_BYTES: usize =
    SELECTOR_BYTES.saturating_add(WORD_BYTES.saturating_mul(2));

/// The last pad byte in front of the recipient's twenty-byte tail.
const LEGACY_PREFIX_AT: usize =
    SELECTOR_BYTES.saturating_add(WORD_BYTES).saturating_sub(address::TAIL_BYTES).saturating_sub(1);

/// Exactly the canonical encoding, so trailing bytes and a dirty pad are refused.
const STRICT: AbiDecoderConfig = AbiDecoderConfig::new().strict(true);

fn transfer_call(to: &[u8; address::BODY_BYTES], amount: u128) -> ITRC20::transferCall {
    ITRC20::transferCall { to: Address::from(address::tail(to)), value: U256::from(amount) }
}

/// `transfer(to, amount)` calldata, selector included.
pub(crate) fn encode_transfer(to: &[u8; address::BODY_BYTES], amount: u128) -> Vec<u8> {
    transfer_call(to, amount).abi_encode()
}

/// `transfer(to, amount)` arguments only, hex, for `triggerconstantcontract`.
pub(crate) fn transfer_args(to: &[u8; address::BODY_BYTES], amount: u128) -> String {
    let mut args = Vec::new();
    transfer_call(to, amount).abi_encode_raw(&mut args);
    hex::encode(args)
}

/// `balanceOf(owner)` arguments only, hex.
pub(crate) fn balance_of_args(owner: &[u8; address::BODY_BYTES]) -> String {
    let mut args = Vec::new();
    ITRC20::balanceOfCall { owner: Address::from(address::tail(owner)) }.abi_encode_raw(&mut args);
    hex::encode(args)
}

/// Splits `transfer` calldata back into its recipient body and amount.
///
/// Two paddings occur on TRON. This build emits twelve zero bytes, and older
/// `TronWeb` left-pads the twenty-one byte body, which leaves `0x41` in the last
/// pad byte. The virtual machine reads the low twenty bytes either way, so both
/// are accepted and nothing else is.
///
/// An amount of 2^128 or more is refused rather than truncated, because a value
/// that wide means a broken contract.
pub(crate) fn decode_transfer(data: &[u8]) -> Result<([u8; address::BODY_BYTES], u128), TronError> {
    let mut calldata = data.to_vec();
    if let Some(pad) = calldata.get_mut(LEGACY_PREFIX_AT)
        && *pad == address::PREFIX
    {
        *pad = 0;
    }
    let call = ITRC20::transferCall::abi_decode_with_config(&calldata, STRICT)
        .map_err(|_| TronError::Foreign { detail: "calldata that is not a TRC-20 transfer" })?;
    let amount =
        u128::try_from(call.value).map_err(|_| TronError::Overflow { what: "the token amount" })?;
    Ok((address::from_tail(&call.to.into_array()), amount))
}

/// The word a `balanceOf` call returned, as base units.
///
/// A balance of 2^128 or more is none rather than truncated, because a balance
/// that wide means a broken contract.
pub(crate) fn decode_balance(word_hex: &str) -> Option<u128> {
    let trimmed = word_hex.trim();
    let raw = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed)).ok()?;
    let balance = ITRC20::balanceOfCall::abi_decode_returns_with_config(&raw, STRICT).ok()?;
    u128::try_from(balance).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::fixtures::{body, contracts};
    use crate::test_util::RECIPIENT;

    fn recipient() -> [u8; address::BODY_BYTES] {
        body(RECIPIENT)
    }

    #[test]
    fn the_signatures_and_selectors_are_the_published_ones() {
        let vectors = contracts();
        for (signature, selector) in [
            (TRANSFER_SIGNATURE, ITRC20::transferCall::SELECTOR),
            (BALANCE_OF_SIGNATURE, ITRC20::balanceOfCall::SELECTOR),
        ] {
            let pinned = vectors
                .get("selectors")
                .and_then(|table| table.get(signature))
                .and_then(Value::as_str);
            assert_eq!(pinned, Some(hex::encode(selector).as_str()), "{signature}");
        }
    }

    #[test]
    fn an_argument_word_is_the_tail_left_padded_with_zeros() {
        assert_eq!(
            balance_of_args(&recipient()),
            "000000000000000000000000e28b3cfd4e0e909077821478e9fcb86b84be786e"
        );
    }

    #[test]
    fn calldata_round_trips_through_both_paddings_this_build_may_meet() {
        let standard = encode_transfer(&recipient(), 1_500_000);
        assert_eq!(standard.len(), TRANSFER_CALL_BYTES);
        assert_eq!(decode_transfer(&standard).expect("standard"), (recipient(), 1_500_000));
        assert_eq!(
            transfer_args(&recipient(), 1_500_000),
            hex::encode(&standard[SELECTOR_BYTES..])
        );

        let mut legacy = standard.clone();
        legacy[15] = 0x41;
        assert_eq!(decode_transfer(&legacy).expect("the older padding"), (recipient(), 1_500_000));

        let mut broken = standard;
        broken[8] = 0x11;
        assert!(
            matches!(decode_transfer(&broken), Err(TronError::Foreign { .. })),
            "a word that is not an address is refused"
        );
    }

    #[test]
    fn a_word_wider_than_the_amount_type_is_refused_rather_than_truncated() {
        let mut wide = encode_transfer(&recipient(), 1);
        wide[SELECTOR_BYTES + WORD_BYTES] = 1;
        assert!(matches!(decode_transfer(&wide), Err(TronError::Overflow { .. })));

        let mut word = [0u8; WORD_BYTES];
        word[0] = 1;
        assert_eq!(decode_balance(&hex::encode(word)), None);
        assert_eq!(decode_balance(&hex::encode(&word[..31])), None);
        assert_eq!(decode_balance(&hex::encode([0u8; WORD_BYTES + 1])), None);
        let widest = U256::from(u128::MAX).to_be_bytes::<WORD_BYTES>();
        assert_eq!(decode_balance(&hex::encode(widest)), Some(u128::MAX));
        assert_eq!(
            decode_balance("0x0000000000000000000000000000000000000000000000000000000000000007"),
            Some(7)
        );
        assert_eq!(decode_balance("not hex"), None);
    }

    #[test]
    fn calldata_of_the_wrong_length_or_selector_is_refused() {
        let is_refused =
            |calldata: &[u8]| matches!(decode_transfer(calldata), Err(TronError::Foreign { .. }));
        assert!(is_refused(&[]));
        let standard = encode_transfer(&recipient(), 1);
        assert!(is_refused(&standard[..TRANSFER_CALL_BYTES - 1]));
        let mut long = standard.clone();
        long.push(0);
        assert!(is_refused(&long));
        let mut wrong = standard;
        wrong[0] = 0xff;
        assert!(is_refused(&wrong));
    }
}
