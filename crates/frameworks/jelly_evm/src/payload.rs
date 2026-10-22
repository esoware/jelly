use alloy::consensus::{SignableTransaction as _, TxEip1559};
use alloy::eips::eip2930::AccessList;
use alloy::primitives::{Address, B256, Bytes, TxKind, U256};
use jelly_core::{Amount, AssetId, TxPayload};
use serde::{Deserialize, Serialize};

use crate::abi;
use crate::error::EvmError;
use crate::fee::FeePair;
use crate::params::EvmParams;
use crate::token::TokenMeta;

/// An unsigned transaction and the intent it cannot state about itself.
///
/// Every consensus field lives in one [`TxEip1559`], so the preview, the
/// signature hash and the broadcast bytes come from the same values and cannot
/// disagree.
///
/// The wire form is JSON so a bug report can read it. Quantities are hex strings,
/// alloy's own representation, which round-trips exactly.
///
/// ```text
/// | member        | meaning                                                    |
/// | ------------- | ---------------------------------------------------------- |
/// | tx            | the EIP-1559 transaction, hex quantities throughout        |
/// | intent.from   | the sender, and signing refuses a key that does not own it |
/// | intent.asset  | which asset the transfer moves                             |
/// | intent.to     | the recipient the user named, not the contract             |
/// | intent.amount | the transfer in the asset's base units, decimal            |
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvmPayload {
    tx: TxEip1559,
    intent: Intent,
}

/// A token transfer's `to` is the contract and its `value` is zero, so this
/// records who was paid and how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Intent {
    pub(crate) from: Address,
    pub(crate) asset: AssetId,
    pub(crate) to: Address,
    pub(crate) amount: Amount,
}

impl EvmPayload {
    pub(crate) const fn new(tx: TxEip1559, intent: Intent) -> Self {
        Self { tx, intent }
    }

    pub(crate) fn encode(&self) -> Result<TxPayload, EvmError> {
        serde_json::to_vec(self)
            .map(TxPayload::from_bytes)
            .map_err(|source| EvmError::Encode { what: "transaction payload", source })
    }

    /// Validating here keeps every accessor infallible.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, EvmError> {
        serde_json::from_slice(bytes).map_err(|source| EvmError::Payload { source })
    }

    pub(crate) const fn tx(&self) -> &TxEip1559 {
        &self.tx
    }

    pub(crate) const fn chain_id(&self) -> u64 {
        self.tx.chain_id
    }

    pub(crate) const fn nonce(&self) -> u64 {
        self.tx.nonce
    }

    /// Refuses a payload whose tip is above its cap, which this build never
    /// writes.
    pub(crate) const fn fee(&self) -> Result<FeePair, EvmError> {
        FeePair::new(self.tx.max_fee_per_gas, self.tx.max_priority_fee_per_gas)
    }

    /// The address the transaction calls, which is the contract for a token
    /// transfer.
    pub(crate) const fn call_target(&self) -> Option<Address> {
        match self.tx.to {
            TxKind::Call(address) => Some(address),
            TxKind::Create => None,
        }
    }

    pub(crate) const fn value(&self) -> U256 {
        self.tx.value
    }

    pub(crate) fn input(&self) -> &[u8] {
        self.tx.input.as_ref()
    }

    /// Calldata the user supplied in place of the transfer the intent describes,
    /// or `None` when the input is that transfer.
    pub(crate) fn calldata(&self) -> Option<Bytes> {
        let transfer = match self.intent.asset {
            AssetId::Native(_) => Bytes::new(),
            AssetId::Token(_) => transfer_input(self.intent.to, self.intent.amount.base_units()),
        };
        (self.tx.input != transfer).then(|| self.tx.input.clone())
    }

    pub(crate) const fn from(&self) -> Address {
        self.intent.from
    }

    pub(crate) const fn intent(&self) -> Intent {
        self.intent
    }

    /// The EIP-1559 signing digest, `keccak256(0x02 || rlp(unsigned fields))`.
    pub(crate) fn signature_hash(&self) -> B256 {
        self.tx.signature_hash()
    }
}

/// Built in one place, so the call a gas estimate is made against and the
/// transaction that gets signed cannot differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) to: Address,
    pub(crate) value: u128,
    pub(crate) input: Bytes,
}

impl Shape {
    /// A token transfer calls the contract and attaches no coin. Calldata the
    /// user supplied replaces the transfer entirely and attaches no coin either.
    pub(crate) fn of(
        token: Option<TokenMeta>,
        to: Address,
        amount: u128,
        calldata: Option<&[u8]>,
    ) -> Self {
        match (token, calldata) {
            (Some(metadata), Some(data)) => {
                Self { to: metadata.address(), value: 0, input: Bytes::copy_from_slice(data) }
            }
            (Some(metadata), None) => {
                Self { to: metadata.address(), value: 0, input: transfer_input(to, amount) }
            }
            (None, Some(data)) => Self { to, value: 0, input: Bytes::copy_from_slice(data) },
            (None, None) => Self { to, value: amount, input: Bytes::new() },
        }
    }
}

fn transfer_input(to: Address, amount: u128) -> Bytes {
    Bytes::from(abi::transfer_calldata(to, U256::from(amount)))
}

pub(crate) fn transaction(
    params: &EvmParams,
    nonce: u64,
    gas_limit: u64,
    fee: FeePair,
    shape: &Shape,
) -> TxEip1559 {
    TxEip1559 {
        chain_id: params.chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas: fee.max_fee_per_gas(),
        max_priority_fee_per_gas: fee.max_priority_fee_per_gas(),
        to: TxKind::Call(shape.to),
        value: U256::from(shape.value),
        access_list: AccessList::default(),
        input: shape.input.clone(),
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, TokenId};

    use super::*;
    use crate::fixtures::{address, token_table};
    use crate::test_util::ETHEREUM;

    const COIN: u128 = 1_000_000_000_000_000_000;

    fn pair(cap: u128, tip: u128) -> FeePair {
        FeePair::new(cap, tip).expect("a pair")
    }

    fn payload() -> EvmPayload {
        let shape = Shape::of(None, address(2), COIN, None);
        EvmPayload::new(
            transaction(&ETHEREUM, 7, 21_000, pair(30_000_000_000, 1_000_000_000), &shape),
            Intent {
                from: address(1),
                asset: AssetId::Native(ChainId::Ethereum),
                to: address(2),
                amount: Amount::from_base_units(COIN),
            },
        )
    }

    #[test]
    fn the_wire_form_is_the_transaction_and_the_intent_and_nothing_else_decodes() {
        let original = payload();
        let expected = format!(
            "{{\"tx\":{},\"intent\":{}}}",
            serde_json::to_string(&original.tx).unwrap(),
            serde_json::to_string(&original.intent).unwrap()
        );
        let encoded = original.encode().unwrap();
        assert_eq!(encoded.as_bytes(), expected.as_bytes());
        let trailing = format!("{expected} {{}}");
        EvmPayload::decode(trailing.as_bytes()).unwrap_err();
        EvmPayload::decode(br#"{"tx":null}"#).unwrap_err();
    }

    #[test]
    fn a_payload_round_trips_through_its_wire_form_exactly() {
        let original = payload();
        let encoded = original.encode().expect("it encodes");
        let decoded = EvmPayload::decode(encoded.as_bytes()).expect("it decodes");
        assert_eq!(decoded, original);
        assert_eq!(decoded.signature_hash(), original.signature_hash());
    }

    #[test]
    fn a_token_payload_keeps_the_recipient_the_user_named_beside_the_contract() {
        let table = token_table();
        let token =
            table.of_asset(AssetId::Token(TokenId::UsdtErc20)).ok().flatten().expect("a token");
        let recipient = address(2);
        let shape = Shape::of(Some(token), recipient, 2_500_000, None);
        let payload = EvmPayload::new(
            transaction(&ETHEREUM, 12, 65_000, pair(40_000_000_000, 2_000_000_000), &shape),
            Intent {
                from: address(1),
                asset: AssetId::Token(TokenId::UsdtErc20),
                to: recipient,
                amount: Amount::from_base_units(2_500_000),
            },
        );
        assert_eq!(payload.call_target(), Some(token.address()), "the transaction calls the token");
        assert_eq!(payload.intent().to, recipient, "and the preview shows the person");
        assert_eq!(payload.value(), U256::ZERO, "a token transfer attaches no coin");
        assert_eq!(payload.input().len(), 4 + 32 + 32);

        let decoded =
            EvmPayload::decode(payload.encode().expect("encodes").as_bytes()).expect("decodes");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn calldata_replaces_the_transfer_and_never_rides_beside_it() {
        let data = [0xde, 0xad, 0xbe, 0xef];
        let raw = Shape::of(None, address(2), COIN, Some(&data));
        assert_eq!(raw.to, address(2));
        assert_eq!(raw.value, 0, "calldata replaces the transfer, so it carries no coin");
        assert_eq!(&raw.input[..], data);

        let table = token_table();
        let token =
            table.of_asset(AssetId::Token(TokenId::UsdtErc20)).ok().flatten().expect("a token");
        let over_token = Shape::of(Some(token), address(2), 5, Some(&data));
        assert_eq!(over_token.to, token.address(), "a token call still goes to the contract");
        assert_eq!(&over_token.input[..], data);
    }

    #[test]
    fn only_calldata_the_user_supplied_reads_back_as_calldata() {
        let table = token_table();
        let token =
            table.of_asset(AssetId::Token(TokenId::UsdtErc20)).ok().flatten().expect("a token");
        let native = AssetId::Native(ChainId::Ethereum);
        let usdt = AssetId::Token(TokenId::UsdtErc20);
        let data = Bytes::from_static(&[0xd0, 0xe3, 0x0d, 0xb0]);
        let built = |token: Option<TokenMeta>, asset: AssetId, calldata: Option<&[u8]>| {
            let shape = Shape::of(token, address(2), 5, calldata);
            let intent = Intent {
                from: address(1),
                asset,
                to: address(2),
                amount: Amount::from_base_units(5),
            };
            EvmPayload::new(transaction(&ETHEREUM, 7, 50_000, pair(30, 1), &shape), intent)
        };
        assert_eq!(built(None, native, None).calldata(), None, "a payment");
        assert_eq!(built(Some(token), usdt, None).calldata(), None, "a generated token transfer");
        assert_eq!(built(None, native, Some(&data)).calldata(), Some(data.clone()));
        assert_eq!(built(Some(token), usdt, Some(&data)).calldata(), Some(data));
    }

    #[test]
    fn a_payload_that_is_not_json_is_refused_rather_than_guessed_at() {
        assert!(matches!(EvmPayload::decode(b"not json"), Err(EvmError::Payload { .. })));
        assert!(matches!(EvmPayload::decode(b""), Err(EvmError::Payload { .. })));
    }

    #[test]
    fn a_payload_carries_the_chain_id_of_the_network_it_was_built_for() {
        let payload = payload();
        assert_eq!(payload.chain_id(), ETHEREUM.chain_id);
        assert_eq!(payload.fee().expect("a pair"), pair(30_000_000_000, 1_000_000_000));
        assert_eq!(payload.nonce(), 7);
    }
}
