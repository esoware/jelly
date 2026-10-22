//! Transactions are built and hashed here, never by a node, because
//! `/wallet/createtransaction` would let a public endpoint choose the bytes a
//! person signs.
//!
//! Protobuf serialisation is not canonical across implementations, so the signed
//! bytes are the transaction. The payload carries them verbatim, the signed form
//! wraps them as opaque bytes rather than re-encoding them, and the broadcast
//! prefers `/wallet/broadcasthex`.

use std::time::Duration;

use jelly_core::wire_enum;
use prost::Message;
use sha2::{Digest as _, Sha256};

use crate::address;
use crate::error::TronError;
use crate::proto::protocol;
use crate::shape::BlockRef;

/// This build writes `r || s || recovery_id` with a raw recovery id, which
/// java-tron accepts alongside the form offset by 27.
pub(crate) const SIGNATURE_BYTES: usize = 65;

/// java-tron reserves this many bytes per transaction for the `ret` field when it
/// charges bandwidth (`Constant.MAX_RESULT_SIZE_IN_TX`).
const RESULT_ALLOWANCE_BYTES: u64 = 64;

/// The protocol refuses an expiration more than twenty-four hours past the head
/// block (`Constant.MAXIMUM_TIME_UNTIL_EXPIRATION`).
pub(crate) const MAX_EXPIRATION: Duration = Duration::from_hours(24);

/// A shorter window is not worth signing, because the transaction has to reach a
/// super representative and blocks are three seconds apart.
pub(crate) const MIN_EXPIRATION: Duration = Duration::from_secs(15);

/// The largest transaction java-tron accepts (`Constant.TRANSACTION_MAX_BYTE_SIZE`).
const MAX_TRANSACTION_BYTES: usize = 512_000;

wire_enum! {
    /// The only two contract bodies this build writes.
    ///
    /// The spelling is the protobuf message name, which java-tron matches exactly
    /// inside a `type_url`. A test here pins this table against the generated
    /// enum, so a re-vendor that renumbers or renames either fails there rather
    /// than in a broadcast.
    pub(crate) enum Contract {
        Transfer => "TransferContract",
        TriggerSmartContract => "TriggerSmartContract",
    }
}

impl Contract {
    /// The `Transaction.Contract.ContractType` discriminant, which is wire
    /// format.
    const fn discriminant(self) -> i32 {
        match self {
            Self::Transfer => 1,
            Self::TriggerSmartContract => 31,
        }
    }

    /// `type.googleapis.com/protocol.<MessageName>`, the string java-tron
    /// matches character for character.
    pub(crate) fn type_url(self) -> String {
        format!("type.googleapis.com/protocol.{}", self.as_str())
    }

    const fn of_discriminant(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Transfer),
            31 => Some(Self::TriggerSmartContract),
            _ => None,
        }
    }
}

/// TAPOS reference and timing fields in their protocol types.
///
/// The reference uses the low 16 height bits and block-ID bytes 8..16. `with_fee_limit`
/// performs the checked conversion to the protocol's signed `int64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) ref_block_bytes: [u8; 2],
    pub(crate) ref_block_hash: [u8; 8],
    pub(crate) timestamp_millis: i64,
    pub(crate) expiration_millis: i64,
    pub(crate) fee_limit_sun: i64,
}

impl Header {
    /// Pins a transaction to the head block a node just reported.
    ///
    /// The timestamp is the head block's own rather than the signer's clock, and
    /// the expiration is measured from it. The network judges the expiration
    /// against head-block time, so this keeps it inside the window the protocol
    /// allows whatever the local clock says.
    ///
    /// The window is clamped to at least [`MIN_EXPIRATION`] and at most
    /// [`MAX_EXPIRATION`], so a configuration mistake cannot build a transaction
    /// the network refuses.
    pub(crate) fn from_block(tip: &BlockRef, window: Duration) -> Result<Self, TronError> {
        let window_millis = i64::try_from(window.clamp(MIN_EXPIRATION, MAX_EXPIRATION).as_millis())
            .map_err(|_| TronError::Overflow { what: "an expiration window" })?;
        let expiration_millis = tip
            .timestamp_millis
            .checked_add(window_millis)
            .ok_or(TronError::Overflow { what: "an expiration time" })?;
        let [.., high, low] = tip.number.to_be_bytes();
        let [_, _, _, _, _, _, _, _, b8, b9, b10, b11, b12, b13, b14, b15, ..] = tip.block_id;
        Ok(Self {
            ref_block_bytes: [high, low],
            ref_block_hash: [b8, b9, b10, b11, b12, b13, b14, b15],
            timestamp_millis: tip.timestamp_millis,
            expiration_millis,
            fee_limit_sun: 0,
        })
    }

    /// Stamps `fee_limit`, the ceiling on the TRX a contract call may burn for
    /// energy.
    ///
    /// The protocol carries it as an `int64` and refuses anything above
    /// `getMaxFeeLimit`, so the lower of the two bounds is checked here rather
    /// than discovered after the user has signed.
    pub(crate) fn with_fee_limit(
        mut self,
        fee_limit_sun: u64,
        maximum_sun: u64,
    ) -> Result<Self, TronError> {
        let ceiling_sun = maximum_sun.min(i64::MAX.unsigned_abs());
        self.fee_limit_sun = i64::try_from(fee_limit_sun)
            .ok()
            .filter(|_| fee_limit_sun <= ceiling_sun)
            .ok_or(TronError::FeeLimitAboveMaximum {
                requested_sun: fee_limit_sun,
                maximum_sun: ceiling_sun,
            })?;
        Ok(self)
    }

    /// How long this transaction stays valid, in whole seconds.
    pub(crate) const fn window_secs(&self) -> i64 {
        match self.expiration_millis.checked_sub(self.timestamp_millis) {
            Some(span) => span.saturating_div(1_000),
            None => 0,
        }
    }
}

pub(crate) fn transfer(
    header: &Header,
    from: &[u8; address::BODY_BYTES],
    to: &[u8; address::BODY_BYTES],
    amount_sun: u64,
) -> Result<Vec<u8>, TronError> {
    let body = protocol::TransferContract {
        owner_address: from.to_vec(),
        to_address: to.to_vec(),
        amount: i64::try_from(amount_sun).map_err(|_| TronError::AmountTooLarge)?,
    };
    Ok(encode(header, Contract::Transfer, &body))
}

/// A contract call, which this build uses only for `transfer(address,uint256)`.
pub(crate) fn trigger(
    header: &Header,
    from: &[u8; address::BODY_BYTES],
    contract: &[u8; address::BODY_BYTES],
    calldata: Vec<u8>,
) -> Vec<u8> {
    let body = protocol::TriggerSmartContract {
        owner_address: from.to_vec(),
        contract_address: contract.to_vec(),
        data: calldata,
        ..Default::default()
    };
    encode(header, Contract::TriggerSmartContract, &body)
}

/// The serialised `raw_data`, which is what gets hashed and signed.
fn encode(header: &Header, kind: Contract, body: &impl Message) -> Vec<u8> {
    let contract = protocol::transaction::Contract {
        r#type: kind.discriminant(),
        parameter: Some(prost_types::Any {
            type_url: kind.type_url(),
            value: body.encode_to_vec(),
        }),
        ..Default::default()
    };
    protocol::transaction::Raw {
        ref_block_bytes: header.ref_block_bytes.to_vec(),
        ref_block_hash: header.ref_block_hash.to_vec(),
        expiration: header.expiration_millis,
        contract: vec![contract],
        timestamp: header.timestamp_millis,
        fee_limit: header.fee_limit_sun,
        ..Default::default()
    }
    .encode_to_vec()
}

/// `txID = sha256(raw_data)`.
pub(crate) fn txid(raw_data: &[u8]) -> [u8; 32] {
    Sha256::digest(raw_data).into()
}

/// Bandwidth points the signed transaction will consume.
///
/// java-tron charges the serialised size of the whole `Transaction`, with one
/// 65-byte signature and without `ret`, plus a flat 64-byte allowance for the
/// result it writes back. A placeholder signature sizes it exactly, because a
/// TRON signature always has the same length.
pub(crate) fn bandwidth_points(raw_data: &[u8]) -> Result<u64, TronError> {
    let size =
        SignedWire { raw_data: raw_data.to_vec(), signature: vec![vec![0u8; SIGNATURE_BYTES]] }
            .encoded_len();
    if size > MAX_TRANSACTION_BYTES {
        return Err(TronError::TransactionTooLarge {
            bytes: size,
            maximum_bytes: MAX_TRANSACTION_BYTES,
        });
    }
    u64::try_from(size)
        .ok()
        .and_then(|size| size.checked_add(RESULT_ALLOWANCE_BYTES))
        .ok_or(TronError::Overflow { what: "a transaction size" })
}

/// The signed `Transaction` protobuf, with `raw_data` carried as the bytes that
/// were hashed.
pub(crate) fn signed_bytes(raw_data: &[u8], signature: &[u8; SIGNATURE_BYTES]) -> Vec<u8> {
    SignedWire { raw_data: raw_data.to_vec(), signature: vec![signature.to_vec()] }.encode_to_vec()
}

/// `protocol.Transaction` with `raw_data` held as opaque bytes.
///
/// Wrapping, unwrapping and sizing the signed form through this never re-encodes
/// the bytes that were hashed. The tags are `Transaction`'s, and this build never
/// writes `ret`.
#[derive(Clone, PartialEq, Message)]
struct SignedWire {
    #[prost(bytes = "vec", tag = "1")]
    raw_data: Vec<u8>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    signature: Vec<Vec<u8>>,
}

/// A `raw_data` read back into the facts it states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub(crate) owner: [u8; address::BODY_BYTES],
    pub(crate) body: DecodedBody,
    pub(crate) header: Header,
}

/// The two contract bodies this build writes, each with only the fields it
/// stamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodedBody {
    Transfer {
        to: [u8; address::BODY_BYTES],
        amount_sun: u64,
    },
    /// `calldata` is the whole call, selector included.
    Trigger {
        contract: [u8; address::BODY_BYTES],
        calldata: Vec<u8>,
    },
}

impl DecodedBody {
    pub(crate) const fn contract(&self) -> Contract {
        match *self {
            Self::Transfer { .. } => Contract::Transfer,
            Self::Trigger { .. } => Contract::TriggerSmartContract,
        }
    }
}

/// Reads a `raw_data` back into the facts it states.
///
/// A contract body whose `type_url` disagrees with its discriminant, and any
/// contract this build does not write, are refused, because a payload only ever
/// comes from this build.
pub(crate) fn decode(raw_data: &[u8]) -> Result<Decoded, TronError> {
    let raw = protocol::transaction::Raw::decode(raw_data)?;
    let entry = raw.contract.first().ok_or(TronError::Foreign { detail: "no contract at all" })?;
    let parameter =
        entry.parameter.as_ref().ok_or(TronError::Foreign { detail: "a contract with no body" })?;
    let contract = Contract::of_discriminant(entry.r#type)
        .ok_or(TronError::Foreign { detail: "a contract this build does not write" })?;
    if parameter.type_url != contract.type_url() {
        return Err(TronError::Foreign { detail: "a contract body that is not its own type" });
    }
    let ref_block_bytes: [u8; 2] =
        raw.ref_block_bytes.as_slice().try_into().map_err(|_| TronError::Foreign {
            detail: "a block reference that is not two bytes",
        })?;
    let ref_block_hash: [u8; 8] = raw
        .ref_block_hash
        .as_slice()
        .try_into()
        .map_err(|_| TronError::Foreign { detail: "a block hash that is not eight bytes" })?;
    if raw.fee_limit < 0 {
        return Err(TronError::Foreign { detail: "a negative fee_limit" });
    }
    let header = Header {
        ref_block_bytes,
        ref_block_hash,
        timestamp_millis: raw.timestamp,
        expiration_millis: raw.expiration,
        fee_limit_sun: raw.fee_limit,
    };
    match contract {
        Contract::Transfer => {
            let body = protocol::TransferContract::decode(parameter.value.as_slice())?;
            Ok(Decoded {
                owner: address::from_body("owner", &body.owner_address)?,
                body: DecodedBody::Transfer {
                    to: address::from_body("recipient", &body.to_address)?,
                    amount_sun: u64::try_from(body.amount)
                        .map_err(|_| TronError::Foreign { detail: "a negative amount" })?,
                },
                header,
            })
        }
        Contract::TriggerSmartContract => {
            let body = protocol::TriggerSmartContract::decode(parameter.value.as_slice())?;
            Ok(Decoded {
                owner: address::from_body("owner", &body.owner_address)?,
                body: DecodedBody::Trigger {
                    contract: address::from_body("token contract", &body.contract_address)?,
                    calldata: body.data,
                },
                header,
            })
        }
    }
}

/// The `raw_data` bytes and the one signature inside a signed wire form, from a
/// single decode.
///
/// This build signs exactly once, so a wire form carrying any other number of
/// signatures was not produced here.
pub(crate) fn signed_parts(signed: &[u8]) -> Result<(Vec<u8>, [u8; SIGNATURE_BYTES]), TronError> {
    let parsed = SignedWire::decode(signed)?;
    if parsed.raw_data.is_empty() {
        return Err(TronError::Foreign { detail: "a wire form with no transaction" });
    }
    let [only] = parsed.signature.as_slice() else {
        return Err(TronError::Foreign { detail: "a signature count other than one" });
    };
    let signature: [u8; SIGNATURE_BYTES] = only
        .as_slice()
        .try_into()
        .map_err(|_| TronError::Foreign { detail: "a signature that is not sixty-five bytes" })?;
    Ok((parsed.raw_data, signature))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::abi;
    use crate::fixtures::{body, contracts};
    use crate::proto::protocol::transaction::contract::ContractType;
    use crate::test_util::{TRX_OWNER_HEX, TRX_RECIPIENT_HEX};

    /// The bytes and the id of mainnet transaction a46244ab.
    const RECORDED_RAW: &str = "0a02a63d2208437d27f93863a48840a08fc3c6ff335a65080112610a2d747970652e676f6f676c65617069732e636f6d2f70726f746f636f6c2e5472616e73666572436f6e747261637412300a1541a5b43182dad15ff0b707d829ffa67b1af6719483121541df0c86c531205eb7a4e27d9589ba7188dfb32f08180770b1cfbfc6ff33";
    const RECORDED_TXID: &str = "a46244abb4be3f4247c538712715c41476b5b0b0f515f657c6acd76b67674bc8";
    /// The receipt the network wrote for that transaction.
    const RECORDED_RECEIPT: &str = include_str!("../tests/fixtures/tx_trx_info.json");

    fn recorded_header() -> Header {
        Header {
            ref_block_bytes: [0xa6, 0x3d],
            ref_block_hash: [0x43, 0x7d, 0x27, 0xf9, 0x38, 0x63, 0xa4, 0x88],
            timestamp_millis: 1_786_585_802_673,
            expiration_millis: 1_786_585_860_000,
            fee_limit_sun: 0,
        }
    }

    /// The recorded `raw_data`, decoded by the generated code.
    fn recorded_raw() -> protocol::transaction::Raw {
        let raw = hex::decode(RECORDED_RAW).expect("hex");
        protocol::transaction::Raw::decode(raw.as_slice()).expect("it decodes")
    }

    #[test]
    fn the_contract_table_still_agrees_with_the_vendored_protos() {
        assert_eq!(Contract::COUNT, 2);
        assert_eq!(Contract::Transfer.discriminant(), i32::from(ContractType::TransferContract));
        assert_eq!(
            Contract::TriggerSmartContract.discriminant(),
            i32::from(ContractType::TriggerSmartContract)
        );
        assert_eq!(Contract::Transfer.as_str(), ContractType::TransferContract.as_str_name());
        assert_eq!(
            Contract::TriggerSmartContract.as_str(),
            ContractType::TriggerSmartContract.as_str_name()
        );
        for contract in Contract::ALL {
            assert_eq!(Contract::of_discriminant(contract.discriminant()), Some(contract));
        }
        assert_eq!(Contract::of_discriminant(11), None, "staking is not built here");
    }

    #[test]
    fn a_rebuild_reproduces_the_recorded_mainnet_bytes_and_id() {
        let raw = transfer(&recorded_header(), &body(TRX_OWNER_HEX), &body(TRX_RECIPIENT_HEX), 7)
            .expect("a transfer builds");
        assert_eq!(hex::encode(&raw), RECORDED_RAW);
        assert_eq!(hex::encode(txid(&raw)), RECORDED_TXID);
    }

    #[test]
    fn bandwidth_matches_the_receipt_the_network_wrote() {
        let receipt: Value = serde_json::from_str(RECORDED_RECEIPT).expect("the receipt parses");
        assert_eq!(
            receipt.get("id").and_then(Value::as_str),
            Some(RECORDED_TXID),
            "the receipt is the one for these bytes"
        );
        let net_usage = receipt
            .get("receipt")
            .and_then(|inner| inner.get("net_usage"))
            .and_then(Value::as_u64)
            .expect("the recorded bandwidth");

        let raw = hex::decode(RECORDED_RAW).expect("hex");
        assert_eq!(bandwidth_points(&raw).expect("points"), net_usage);
    }

    #[test]
    fn the_reference_is_the_low_sixteen_bits_of_the_height_and_bytes_eight_to_sixteen_of_the_id() {
        let block_id: [u8; 32] =
            hex::decode("000000000515a651af7c721ebc0c55d84f973b989508d8f40ba10affa980fd3e")
                .expect("hex")
                .try_into()
                .expect("thirty-two bytes");
        let tip = BlockRef { number: 85_304_913, block_id, timestamp_millis: 1_786_585_806_000 };
        let header = Header::from_block(&tip, Duration::from_secs(600)).expect("a header");
        assert_eq!(hex::encode(header.ref_block_bytes), "a651");
        assert_eq!(hex::encode(header.ref_block_hash), "af7c721ebc0c55d8");
        assert_eq!(header.expiration_millis, 1_786_586_406_000);
        assert_eq!(header.window_secs(), 600);
    }

    #[test]
    fn an_expiration_window_outside_the_range_this_build_signs_is_clamped_into_it() {
        let tip = BlockRef { number: 1, block_id: [0u8; 32], timestamp_millis: 0 };
        let signed = |window: Duration| {
            let header = Header::from_block(&tip, window).expect("a header");
            Duration::from_millis(u64::try_from(header.expiration_millis).expect("after the tip"))
        };
        assert_eq!(signed(Duration::from_secs(1)), MIN_EXPIRATION);
        assert_eq!(signed(MIN_EXPIRATION), MIN_EXPIRATION);
        assert_eq!(signed(MAX_EXPIRATION), MAX_EXPIRATION);
        assert_eq!(signed(Duration::from_secs(86_401)), MAX_EXPIRATION);
        assert_eq!(
            signed(Duration::from_millis(86_400_500)),
            MAX_EXPIRATION,
            "a sub-second excess is clamped too"
        );
    }

    #[test]
    fn a_fee_limit_above_the_protocol_maximum_is_refused_before_anything_is_signed() {
        let header = recorded_header();
        let error = header
            .with_fee_limit(15_000_000_001, 15_000_000_000)
            .expect_err("the protocol caps fee_limit");
        assert!(matches!(error, TronError::FeeLimitAboveMaximum { .. }), "{error:?}");
        assert_eq!(
            error.to_string(),
            "The fee limit of 15000.000001 TRX exceeds the network maximum of 15000 TRX."
        );
        assert_eq!(
            header.with_fee_limit(40_000_000, 15_000_000_000).expect("in range").fee_limit_sun,
            40_000_000
        );

        // The field is an int64, so that bound holds even if governance publishes
        // a wider one. A saturating conversion would sign a ceiling nobody asked
        // for.
        let error =
            header.with_fee_limit(u64::MAX, u64::MAX).expect_err("the field itself is a ceiling");
        assert!(
            matches!(
                error,
                TronError::FeeLimitAboveMaximum { maximum_sun: 9_223_372_036_854_775_807, .. }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_built_transaction_reads_back_to_the_facts_it_states() {
        let raw = transfer(&recorded_header(), &body(TRX_OWNER_HEX), &body(TRX_RECIPIENT_HEX), 7)
            .expect("a transfer builds");
        let decoded = decode(&raw).expect("it reads back");
        assert_eq!(decoded.owner, body(TRX_OWNER_HEX));
        assert_eq!(
            decoded.body,
            DecodedBody::Transfer { to: body(TRX_RECIPIENT_HEX), amount_sun: 7 }
        );
        assert_eq!(decoded.body.contract(), Contract::Transfer);
        assert_eq!(decoded.header, recorded_header());

        let calldata = abi::encode_transfer(&body(TRX_RECIPIENT_HEX), 5);
        let header = recorded_header().with_fee_limit(100_000_000, u64::MAX).expect("in range");
        let call =
            trigger(&header, &body(TRX_OWNER_HEX), &body(TRX_RECIPIENT_HEX), calldata.clone());
        let decoded = decode(&call).expect("it reads back");
        assert_eq!(
            decoded.body,
            DecodedBody::Trigger { contract: body(TRX_RECIPIENT_HEX), calldata }
        );
        assert_eq!(decoded.body.contract(), Contract::TriggerSmartContract);
        assert_eq!(decoded.header.fee_limit_sun, 100_000_000);
    }

    #[test]
    fn a_body_that_is_not_its_own_type_and_a_contract_this_build_never_writes_are_refused() {
        let mut parsed = recorded_raw();
        if let Some(entry) = parsed.contract.first_mut()
            && let Some(parameter) = entry.parameter.as_mut()
        {
            parameter.type_url = Contract::TriggerSmartContract.type_url();
        }
        let error = decode(&parsed.encode_to_vec()).expect_err("a mismatched envelope");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");

        let mut parsed = recorded_raw();
        if let Some(entry) = parsed.contract.first_mut() {
            entry.r#type = i32::from(ContractType::FreezeBalanceV2Contract);
            if let Some(parameter) = entry.parameter.as_mut() {
                parameter.type_url =
                    String::from("type.googleapis.com/protocol.FreezeBalanceV2Contract");
            }
        }
        let error = decode(&parsed.encode_to_vec()).expect_err("staking is not built here");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");
    }

    #[test]
    fn a_negative_amount_is_a_transaction_this_build_did_not_produce() {
        let transfer = protocol::TransferContract {
            owner_address: body(TRX_OWNER_HEX).to_vec(),
            to_address: body(TRX_RECIPIENT_HEX).to_vec(),
            amount: -1,
        };
        let raw = encode(&recorded_header(), Contract::Transfer, &transfer);
        let error = decode(&raw).expect_err("this build never writes a negative amount");
        assert!(matches!(error, TronError::Foreign { detail: "a negative amount" }), "{error:?}");
    }

    #[test]
    fn the_signed_wire_form_carries_the_bytes_that_were_hashed_verbatim() {
        let raw = hex::decode(RECORDED_RAW).expect("hex");
        let signature = [7u8; SIGNATURE_BYTES];
        let signed = signed_bytes(&raw, &signature);
        let wire = hex::encode(&signed);
        assert!(wire.contains(&hex::encode(&raw)), "raw_data travels verbatim");
        assert!(wire.contains(&hex::encode(signature)));
        // raw_data with its three-byte protobuf header, and a sixty-five byte
        // signature with its own two-byte header.
        assert_eq!(signed.len(), raw.len().saturating_add(70));

        // Reading it back gives the same bytes, so the txid the JSON broadcast
        // body states is the signed transaction's.
        let (read_back, only) = signed_parts(&signed).expect("it reads back");
        assert_eq!(hex::encode(&read_back), RECORDED_RAW);
        assert_eq!(hex::encode(txid(&read_back)), RECORDED_TXID);
        assert_eq!(only, signature);
    }

    #[test]
    fn a_wire_form_this_build_did_not_sign_is_refused() {
        let two = protocol::Transaction {
            raw_data: Some(recorded_raw()),
            signature: vec![vec![1u8; SIGNATURE_BYTES], vec![2u8; SIGNATURE_BYTES]],
            ret: Vec::new(),
        };
        let error = signed_parts(&two.encode_to_vec()).expect_err("this build signs once");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");

        let short = protocol::Transaction {
            raw_data: Some(recorded_raw()),
            signature: vec![vec![1u8; 64]],
            ret: Vec::new(),
        };
        let error = signed_parts(&short.encode_to_vec()).expect_err("sixty-five bytes or nothing");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");

        let bare = protocol::Transaction {
            raw_data: None,
            signature: vec![vec![1u8; SIGNATURE_BYTES]],
            ret: Vec::new(),
        };
        let error = signed_parts(&bare.encode_to_vec()).expect_err("no transaction at all");
        assert!(matches!(error, TronError::Foreign { .. }), "{error:?}");
    }

    #[test]
    fn the_contract_table_is_the_wire_format_it_claims_to_be() {
        let vectors = contracts();
        let entries =
            vectors.get("contracts").and_then(Value::as_array).expect("the contract list");
        assert_eq!(entries.len(), Contract::COUNT, "only two contract bodies are ever built");
        for entry in entries {
            let name = entry.get("name").and_then(Value::as_str).expect("a name");
            let contract =
                Contract::parse(name).unwrap_or_else(|| panic!("no contract named {name}"));
            assert_eq!(
                entry.get("discriminant").and_then(Value::as_i64),
                Some(i64::from(contract.discriminant())),
                "{name}"
            );
            assert_eq!(
                entry.get("typeUrl").and_then(Value::as_str),
                Some(contract.type_url().as_str()),
                "{name}"
            );
            assert_eq!(contract.as_str(), name);
        }
    }
}
