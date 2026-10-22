use alloy::primitives::{Address, B256, Bytes, U256, address};
use alloy::sol;
use alloy::sol_types::{SolCall as _, SolEvent as _};

use crate::error::EvmError;

sol! {
    interface IERC20 {
        function balanceOf(address owner) external view returns (uint256);
        function transfer(address to, uint256 value) external returns (bool);
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the generated constructor takes each of the event's eight fields"
)]
mod bor {
    alloy::sol! {
        /// Bor's system token contract at `0x1010`, which logs every movement of
        /// the native coin, including one a contract makes, and names itself as
        /// `token`.
        interface IBorNativeToken {
            event LogTransfer(
                address indexed token,
                address indexed from,
                address indexed to,
                uint256 amount,
                uint256 input1,
                uint256 input2,
                uint256 output1,
                uint256 output2
            );
        }
    }
}

sol! {
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }
        struct Call3Result {
            bool success;
            bytes returnData;
        }
        function aggregate3(Call3[] calldata calls) external payable returns (Call3Result[] memory returnData);
        function getEthBalance(address addr) external view returns (uint256 balance);
    }
}

sol! {
    /// The OP Stack's fee oracle, which prices the L1 data fee its network charges
    /// apart from gas.
    interface IGasPriceOracle {
        function getL1FeeUpperBound(uint256 unsignedTxSize) external view returns (uint256);
    }
}

pub(crate) const TRANSFER_TOPIC: B256 = IERC20::Transfer::SIGNATURE_HASH;

pub(crate) const NATIVE_TRANSFER_TOPIC: B256 = bor::IBorNativeToken::LogTransfer::SIGNATURE_HASH;

pub(crate) const BOR_NATIVE_TOKEN: Address = address!("0x0000000000000000000000000000000000001010");

/// Multicall3, deployed at this address on every network this build reaches.
pub(crate) const MULTICALL3: Address = address!("0xcA11bde05977b3631167028862bE2a173976CA11");

/// The `GasPriceOracle` predeploy, at this address on every OP Stack network.
pub(crate) const GAS_PRICE_ORACLE: Address = address!("0x420000000000000000000000000000000000000F");

/// One movement a log reports, whether a token contract or the native token
/// logged it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transfer {
    pub(crate) from: Address,
    pub(crate) to: Address,
    pub(crate) value: U256,
}

pub(crate) fn balance_of_calldata(owner: Address) -> Vec<u8> {
    IERC20::balanceOfCall { owner }.abi_encode()
}

/// The only state-changing call this build generates. It never generates an
/// approval, because an allowance that outlives the send is a standing risk.
/// Calldata the user supplies is signed as given.
pub(crate) fn transfer_calldata(to: Address, value: U256) -> Vec<u8> {
    IERC20::transferCall { to, value }.abi_encode()
}

pub(crate) fn eth_balance_calldata(owner: Address) -> Vec<u8> {
    IMulticall3::getEthBalanceCall { addr: owner }.abi_encode()
}

pub(crate) fn aggregate3_calldata(calls: Vec<IMulticall3::Call3>) -> Vec<u8> {
    IMulticall3::aggregate3Call { calls }.abi_encode()
}

/// `allowFailure` is always set, so one reverting contract does not lose the
/// whole batch. A failed entry reads as an unknown balance, not a zero one.
pub(crate) fn call3(target: Address, call_data: Vec<u8>) -> IMulticall3::Call3 {
    IMulticall3::Call3 { target, allowFailure: true, callData: Bytes::from(call_data) }
}

pub(crate) fn decode_balance(returned: &[u8]) -> Result<U256, EvmError> {
    IERC20::balanceOfCall::abi_decode_returns(returned)
        .map_err(|source| EvmError::AbiDecode { what: "balanceOf", source })
}

/// The oracle adds the signature's bytes itself, so it takes the size of the
/// unsigned transaction.
pub(crate) fn l1_fee_upper_bound_calldata(unsigned_tx_size: usize) -> Vec<u8> {
    IGasPriceOracle::getL1FeeUpperBoundCall { unsignedTxSize: U256::from(unsigned_tx_size) }
        .abi_encode()
}

pub(crate) fn decode_l1_fee(returned: &[u8]) -> Result<U256, EvmError> {
    IGasPriceOracle::getL1FeeUpperBoundCall::abi_decode_returns(returned)
        .map_err(|source| EvmError::AbiDecode { what: "getL1FeeUpperBound", source })
}

pub(crate) fn decode_aggregate3(
    returned: &[u8],
) -> Result<Vec<IMulticall3::Call3Result>, EvmError> {
    IMulticall3::aggregate3Call::abi_decode_returns(returned)
        .map_err(|source| EvmError::AbiDecode { what: "aggregate3", source })
}

/// `None` for a malformed log or an unrelated event that shares topic zero.
pub(crate) fn decode_transfer(topics: &[B256], data: &[u8]) -> Option<Transfer> {
    let IERC20::Transfer { from, to, value } =
        IERC20::Transfer::decode_raw_log(topics.iter().copied(), data).ok()?;
    Some(Transfer { from, to, value })
}

/// `None` for a malformed log. The four balance words after the amount are not
/// read.
pub(crate) fn decode_native_transfer(topics: &[B256], data: &[u8]) -> Option<Transfer> {
    let log =
        bor::IBorNativeToken::LogTransfer::decode_raw_log(topics.iter().copied(), data).ok()?;
    Some(Transfer { from: log.from, to: log.to, value: log.amount })
}

#[cfg(test)]
mod tests {
    use alloy::hex;

    use super::*;

    #[test]
    fn the_generated_selectors_are_the_published_ones() {
        assert_eq!(hex::encode(IERC20::balanceOfCall::SELECTOR), "70a08231");
        assert_eq!(hex::encode(IERC20::transferCall::SELECTOR), "a9059cbb");
        assert_eq!(hex::encode(IMulticall3::aggregate3Call::SELECTOR), "82ad56cb");
        assert_eq!(hex::encode(IMulticall3::getEthBalanceCall::SELECTOR), "4d2301cc");
        assert_eq!(
            hex::encode(TRANSFER_TOPIC),
            "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        );
        assert_eq!(
            hex::encode(NATIVE_TRANSFER_TOPIC),
            "e6497e3ee548a3372136af2fcb0696db31fc6cf20260707645068bd3fe97f3c4"
        );
    }

    #[test]
    fn a_transfer_calldata_is_a_selector_and_two_padded_words() {
        let to: Address = "0x5041ed759Dd4aFc3a72b8192C143F72f4724081A".parse().expect("address");
        let data = transfer_calldata(to, U256::from(1_000_000u64));
        assert_eq!(data.len(), 4 + 32 + 32);
        assert_eq!(
            hex::encode(&data),
            "a9059cbb\
             0000000000000000000000005041ed759dd4afc3a72b8192c143f72f4724081a\
             00000000000000000000000000000000000000000000000000000000000f4240"
        );
    }

    #[test]
    fn a_transfer_log_decodes_from_its_topics_and_data() {
        let from: Address = "0x5041ed759Dd4aFc3a72b8192C143F72f4724081A".parse().expect("address");
        let to: Address = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".parse().expect("address");
        let topics = [TRANSFER_TOPIC, from.into_word(), to.into_word()];
        let data = U256::from(2_500_000u64).to_be_bytes::<32>();
        let decoded = decode_transfer(&topics, &data).expect("a transfer");
        assert_eq!(decoded.from, from);
        assert_eq!(decoded.to, to);
        assert_eq!(decoded.value, U256::from(2_500_000u64));

        assert!(decode_transfer(&topics[..2], &data).is_none(), "a transfer has three topics");
        assert!(decode_transfer(&topics, &[]).is_none(), "a transfer carries a value word");
    }

    /// A 0.0347 POL payment as Polygon logged it in block 95,473,283. The four
    /// words after the amount are the sender's and the recipient's balances before
    /// and after.
    #[test]
    fn a_recorded_native_transfer_log_decodes_its_parties_and_amount() {
        let topics: Vec<B256> = [
            "0xe6497e3ee548a3372136af2fcb0696db31fc6cf20260707645068bd3fe97f3c4",
            "0x0000000000000000000000000000000000000000000000000000000000001010",
            "0x000000000000000000000000950fcaf1914105544c05645a912a2003151352a0",
            "0x000000000000000000000000377bc465c0d292d4547ddeef11eaa8ce695ae6fe",
        ]
        .iter()
        .map(|topic| topic.parse().expect("a topic"))
        .collect();
        let data = hex::decode(
            "000000000000000000000000000000000000000000000000007b358c80cc177c\
             000000000000000000000000000000000000000000000002acae04bced9af4b6\
             0000000000000000000000000000000000000000000000000000000000000000\
             000000000000000000000000000000000000000000000002ac32cf306ccedd3a\
             000000000000000000000000000000000000000000000000007b358c80cc177c",
        )
        .expect("hex");
        let decoded = decode_native_transfer(&topics, &data).expect("a native transfer");
        let address = |text: &str| text.parse::<Address>().expect("an address");
        assert_eq!(decoded.from, address("0x950fcaf1914105544c05645a912a2003151352a0"));
        assert_eq!(decoded.to, address("0x377bc465c0d292d4547ddeef11eaa8ce695ae6fe"));
        assert_eq!(decoded.value, U256::from(34_680_299_707_963_260u128));
        assert_eq!(topics.first(), Some(&NATIVE_TRANSFER_TOPIC));
        assert_eq!(topics.get(1), Some(&BOR_NATIVE_TOKEN.into_word()));

        assert!(decode_native_transfer(&topics[..3], &data).is_none(), "the recipient is missing");
        assert!(decode_native_transfer(&topics, &data[..32]).is_none(), "the balances are missing");
        assert!(decode_transfer(&topics, &data).is_none(), "it is not an erc-20 transfer");
    }

    #[test]
    fn the_oracle_is_asked_for_the_l1_fee_of_an_unsigned_transaction_by_its_size() {
        assert_eq!(
            hex::encode(l1_fee_upper_bound_calldata(120)),
            format!("f1c7a58b{:064x}", 120),
            "getL1FeeUpperBound(uint256)"
        );
        let answered =
            hex::decode("000000000000000000000000000000000000000000000000000000003a0752bb")
                .unwrap();
        assert_eq!(decode_l1_fee(&answered).unwrap(), U256::from(973_558_459u64));
        assert!(decode_l1_fee(&[]).is_err(), "an empty answer is no fee");
    }
}
