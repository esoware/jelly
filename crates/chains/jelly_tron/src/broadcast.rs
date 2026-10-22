//! A refusal arrives as HTTP 200 with a `code` member, and every node would say
//! the same about the same bytes. So it travels as a value rather than as a
//! network error a rotation would shop around.

use jelly_core::{Rejection, RejectionKind, RetryPolicy, wire_enum};
use serde::{Deserialize, Serialize};

use crate::client::TronClient;
use crate::error::TronError;
use crate::node::Answered;
use crate::raw::{self, DecodedBody};
use crate::shape::read_message;

wire_enum! {
    /// java-tron's `response_code`, spelled as it is upstream.
    ///
    /// `BANDWITH_ERROR` is misspelled in `api.proto` and must match as is, and
    /// [`BroadcastCode::classify`] also accepts the corrected spelling some forks
    /// use. An unknown code is [`BroadcastCode::Unclassified`], which is safe
    /// because the wallet re-checks the transaction by txid.
    enum BroadcastCode {
        /// The node accepted the transaction.
        Success => "SUCCESS",
        /// The signature does not verify.
        SigError => "SIGERROR",
        /// The network refused the transfer as invalid, for example for a missing
        /// balance or account.
        ContractValidateError => "CONTRACT_VALIDATE_ERROR",
        /// The contract ran and reverted.
        ContractExeError => "CONTRACT_EXE_ERROR",
        /// Not enough bandwidth, and not enough TRX to pay for it either.
        BandwidthError => "BANDWITH_ERROR",
        /// The network already has this transaction, which is a success.
        DupTransactionError => "DUP_TRANSACTION_ERROR",
        /// The reference block is no longer recent.
        TaposError => "TAPOS_ERROR",
        /// Above the protocol's size limit.
        TooBigTransactionError => "TOO_BIG_TRANSACTION_ERROR",
        /// The expiration passed before the transaction was accepted.
        TransactionExpirationError => "TRANSACTION_EXPIRATION_ERROR",
        /// The node is overloaded.
        ServerBusy => "SERVER_BUSY",
        /// The node has no peers to relay to.
        NoConnection => "NO_CONNECTION",
        /// The node has too few peers to relay.
        NotEnoughEffectiveConnection => "NOT_ENOUGH_EFFECTIVE_CONNECTION",
        /// The node's head is too far past its last solidified block, so it takes
        /// no transaction until it catches up.
        BlockUnsolidified => "BLOCK_UNSOLIDIFIED",
        /// The node reported an unspecified error.
        OtherError => "OTHER_ERROR",
        /// A code this build has no name for.
        Unclassified => "UNCLASSIFIED",
    }
}

impl BroadcastCode {
    /// Reads one `code` member. This is the one place a TRON node's code string
    /// is interpreted.
    fn classify(text: &str) -> Self {
        match text.trim().to_ascii_uppercase().as_str() {
            // Spellings some forks use instead of the upstream ones.
            "BANDWIDTH_ERROR" => Self::BandwidthError,
            "DUP_TRANSACTION" => Self::DupTransactionError,
            other => Self::parse(other).unwrap_or(Self::Unclassified),
        }
    }

    /// True when the network already holds the transaction, so the send is done.
    const fn is_accepted(self) -> bool {
        matches!(self, Self::Success | Self::DupTransactionError)
    }

    const fn kind(self) -> RejectionKind {
        match self {
            Self::DupTransactionError => RejectionKind::AlreadyKnown,
            Self::TaposError | Self::TransactionExpirationError => RejectionKind::StaleReference,
            Self::ContractExeError => RejectionKind::Reverted,
            Self::Success
            | Self::SigError
            | Self::ContractValidateError
            | Self::BandwidthError
            | Self::TooBigTransactionError
            | Self::ServerBusy
            | Self::NoConnection
            | Self::NotEnoughEffectiveConnection
            | Self::BlockUnsolidified
            | Self::OtherError
            | Self::Unclassified => RejectionKind::Unclassified,
        }
    }

    /// A stale reference means this attempt is dead and a rebuild is safe. A
    /// busy, peerless or lagging node is worth asking elsewhere. Everything else
    /// is final, because every node would refuse the transaction as written.
    const fn retry(self) -> RetryPolicy {
        match self {
            Self::DupTransactionError => RetryPolicy::AlreadyLanded,
            Self::TaposError | Self::TransactionExpirationError => RetryPolicy::DidNotLand,
            Self::ServerBusy
            | Self::NoConnection
            | Self::NotEnoughEffectiveConnection
            | Self::BlockUnsolidified => RetryPolicy::TryAnotherEndpoint,
            Self::Success
            | Self::SigError
            | Self::ContractValidateError
            | Self::ContractExeError
            | Self::BandwidthError
            | Self::TooBigTransactionError
            | Self::OtherError
            | Self::Unclassified => RetryPolicy::FinalEverywhere,
        }
    }

    /// What this code means, in words a person reads.
    const fn explain(self) -> &'static str {
        match self {
            Self::Success => "the node accepted the transaction",
            Self::SigError => "the node rejected the signature",
            Self::ContractValidateError => "the network rejected the transfer as invalid",
            Self::ContractExeError => "the token contract reverted",
            Self::BandwidthError => {
                "not enough bandwidth, and not enough TRX to pay the bandwidth fee"
            }
            Self::DupTransactionError => "this transaction was already submitted",
            Self::TaposError => {
                "the reference block is no longer recent, so rebuild and sign again"
            }
            Self::TooBigTransactionError => "the transaction is too large for the network",
            Self::TransactionExpirationError => {
                "the transaction expired before it was accepted, so rebuild and sign again"
            }
            Self::ServerBusy => "the node is overloaded",
            Self::NoConnection => "the node has no peers to relay to",
            Self::NotEnoughEffectiveConnection => "the node has too few peers to relay",
            Self::BlockUnsolidified => {
                "the node is too far past its last solidified block to take transactions"
            }
            Self::OtherError => "the node reported an unspecified error",
            Self::Unclassified => "the node reported an unrecognised error code",
        }
    }

    /// The full rejection, with the node's own wording kept for diagnostics.
    fn into_rejection(self, message: Option<&str>) -> Rejection {
        let mut detail = format!("{}: {}", self.as_str(), self.explain());
        let words = message.map(read_message).unwrap_or_default();
        if !words.is_empty() {
            detail.push_str(" (");
            detail.push_str(&words);
            detail.push(')');
        }
        Rejection::new(self.kind(), self.retry(), detail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BroadcastReply {
    /// The network holds the transaction. `txid` is what the node echoed, when it
    /// echoed one.
    Accepted { txid: Option<String> },
    /// The network refused, already classified.
    Refused(Rejection),
}

impl BroadcastReply {
    /// A bare `{"result": true}` is accepted. A `code` member decides whatever
    /// `result` says, because some gateways omit `result` on a refusal and others
    /// set it false alongside a code that means success.
    fn read(answer: &Answered) -> Result<Self, TronError> {
        let reply: Response = answer.decode()?;
        match (reply.result, reply.code.as_deref()) {
            (_, Some(code)) => {
                let code = BroadcastCode::classify(code);
                if code.is_accepted() {
                    return Ok(Self::Accepted { txid: reply.txid });
                }
                Ok(Self::Refused(code.into_rejection(reply.message.as_deref())))
            }
            (Some(true), None) => Ok(Self::Accepted { txid: reply.txid }),
            (_, None) => Ok(Self::Refused(
                BroadcastCode::OtherError.into_rejection(reply.message.as_deref()),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    result: Option<bool>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    txid: Option<String>,
}

/// One attempt against one endpoint. The JSON fallback body is built here from
/// the same signed bytes, so one place derives it.
pub(crate) async fn deliver(
    client: &TronClient,
    signed: &[u8],
) -> Result<BroadcastReply, TronError> {
    let (raw_data, signature) = raw::signed_parts(signed)?;
    let json = json_body(&raw_data, &signature)?;
    let answer = client.broadcast(signed, json).await?;
    BroadcastReply::read(&answer)
}

/// The JSON broadcast body, for a gateway that does not serve `broadcasthex`.
///
/// java-tron rebuilds the transaction from the decoded `raw_data` object and
/// ignores `raw_data_hex`, so `raw_data` is emitted field by field from the
/// signed bytes. It merges `parameter.value` as an object of the contract's own
/// fields, with addresses in hex because the body carries no `visible` flag.
/// The hex route is preferred, because a re-serialisation that differs by one
/// varint changes the txid and invalidates the signature.
fn json_body(raw_data: &[u8], signature: &[u8; raw::SIGNATURE_BYTES]) -> Result<String, TronError> {
    let decoded = raw::decode(raw_data)?;
    let kind = decoded.body.contract();
    let owner_address = hex::encode(decoded.owner);
    let value = match decoded.body {
        DecodedBody::Transfer { to, amount_sun } => {
            JsonBody::Transfer { owner_address, to_address: hex::encode(to), amount: amount_sun }
        }
        DecodedBody::Trigger { contract, calldata } => JsonBody::Trigger {
            owner_address,
            contract_address: hex::encode(contract),
            data: hex::encode(calldata),
        },
    };
    let body = JsonBroadcast {
        raw_data: JsonRaw {
            ref_block_bytes: hex::encode(decoded.header.ref_block_bytes),
            ref_block_hash: hex::encode(decoded.header.ref_block_hash),
            expiration: decoded.header.expiration_millis,
            timestamp: decoded.header.timestamp_millis,
            fee_limit: (decoded.header.fee_limit_sun > 0).then_some(decoded.header.fee_limit_sun),
            contract: vec![JsonContract {
                kind: kind.as_str(),
                parameter: JsonParameter { type_url: kind.type_url(), value },
            }],
        },
        raw_data_hex: hex::encode(raw_data),
        txid: hex::encode(raw::txid(raw_data)),
        signature: vec![hex::encode(signature)],
    };
    serde_json::to_string(&body).map_err(|_| TronError::Encode { what: "the broadcast body" })
}

#[derive(Debug, Serialize)]
struct JsonBroadcast {
    raw_data: JsonRaw,
    raw_data_hex: String,
    #[serde(rename = "txID")]
    txid: String,
    signature: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JsonRaw {
    ref_block_bytes: String,
    ref_block_hash: String,
    expiration: i64,
    timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee_limit: Option<i64>,
    contract: Vec<JsonContract>,
}

#[derive(Debug, Serialize)]
struct JsonContract {
    #[serde(rename = "type")]
    kind: &'static str,
    parameter: JsonParameter,
}

#[derive(Debug, Serialize)]
struct JsonParameter {
    type_url: String,
    value: JsonBody,
}

/// The fields of the two contract bodies this build writes. They are all java-tron
/// needs to re-encode the signed body, because the build leaves every other
/// field at its default.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum JsonBody {
    Transfer { owner_address: String, to_address: String, amount: u64 },
    Trigger { owner_address: String, contract_address: String, data: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::answered;
    use crate::route::Route;

    fn reply(body: &str) -> Result<BroadcastReply, TronError> {
        BroadcastReply::read(&answered(Route::BroadcastHex, body))
    }

    #[test]
    fn the_upstream_misspelling_and_the_corrected_one_mean_the_same_thing() {
        assert_eq!(BroadcastCode::classify("BANDWITH_ERROR"), BroadcastCode::BandwidthError);
        assert_eq!(BroadcastCode::classify("BANDWIDTH_ERROR"), BroadcastCode::BandwidthError);
        assert_eq!(
            BroadcastCode::classify(" dup_transaction "),
            BroadcastCode::DupTransactionError
        );
        assert_eq!(BroadcastCode::classify("SOMETHING_NEW"), BroadcastCode::Unclassified);
    }

    #[test]
    fn a_duplicate_is_a_send_that_already_landed_and_a_revert_is_final() {
        let duplicate = reply(r#"{"result":false,"code":"DUP_TRANSACTION_ERROR","txid":"aa"}"#)
            .expect("an answer");
        assert_eq!(duplicate, BroadcastReply::Accepted { txid: Some(String::from("aa")) });

        let reverted = reply(
            r#"{"result":false,"code":"CONTRACT_EXE_ERROR","message":"524556455254206f70636f6465"}"#,
        )
        .expect("an answer");
        let BroadcastReply::Refused(rejection) = reverted else {
            panic!("a refusal is an answer");
        };
        assert_eq!(rejection.kind, RejectionKind::Reverted);
        assert_eq!(rejection.retry, RetryPolicy::FinalEverywhere);
        assert!(rejection.detail.contains("REVERT opcode"), "{}", rejection.detail);
    }

    #[test]
    fn a_bare_result_true_is_accepted_and_a_body_with_neither_is_a_refusal() {
        assert_eq!(
            reply(r#"{"result":true}"#).expect("an answer"),
            BroadcastReply::Accepted { txid: None }
        );
        let empty = reply("{}").expect("an answer");
        assert!(matches!(empty, BroadcastReply::Refused(_)), "{empty:?}");
    }

    #[test]
    fn every_documented_response_code_classifies_and_only_two_are_successes() {
        assert_eq!(BroadcastCode::COUNT, 15);
        let table = [
            ("SUCCESS", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
            ("SIGERROR", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
            ("CONTRACT_VALIDATE_ERROR", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
            ("CONTRACT_EXE_ERROR", RejectionKind::Reverted, RetryPolicy::FinalEverywhere),
            ("BANDWITH_ERROR", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
            ("DUP_TRANSACTION_ERROR", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
            ("TAPOS_ERROR", RejectionKind::StaleReference, RetryPolicy::DidNotLand),
            (
                "TOO_BIG_TRANSACTION_ERROR",
                RejectionKind::Unclassified,
                RetryPolicy::FinalEverywhere,
            ),
            (
                "TRANSACTION_EXPIRATION_ERROR",
                RejectionKind::StaleReference,
                RetryPolicy::DidNotLand,
            ),
            ("SERVER_BUSY", RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
            ("NO_CONNECTION", RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
            (
                "NOT_ENOUGH_EFFECTIVE_CONNECTION",
                RejectionKind::Unclassified,
                RetryPolicy::TryAnotherEndpoint,
            ),
            ("OTHER_ERROR", RejectionKind::Unclassified, RetryPolicy::FinalEverywhere),
            (
                "SOMETHING_THE_NODE_INVENTED",
                RejectionKind::Unclassified,
                RetryPolicy::FinalEverywhere,
            ),
        ];
        for (text, kind, retry) in table {
            let code = BroadcastCode::classify(text);
            assert_eq!(code.kind(), kind, "{text}");
            assert_eq!(code.retry(), retry, "{text}");
            assert!(!code.explain().is_empty(), "{text}");
        }
        for code in BroadcastCode::ALL {
            assert_eq!(BroadcastCode::parse(code.as_str()), Some(code), "{code}");
            assert_eq!(BroadcastCode::classify(code.as_str()), code, "{code}");
        }
        assert!(BroadcastCode::Success.is_accepted());
        assert!(BroadcastCode::DupTransactionError.is_accepted());
        assert_eq!(
            BroadcastCode::ALL.iter().filter(|code| code.is_accepted()).count(),
            2,
            "only a success and a duplicate mean the network holds the transaction"
        );

        // java-tron answers BLOCK_UNSOLIDIFIED when the node's own head is too far
        // ahead of its solidified block. It checks that beside SERVER_BUSY and
        // NO_CONNECTION, before it reads the transaction, so the reference block
        // plays no part and a node that is keeping up can take the same bytes.
        let unsolidified = BroadcastCode::classify("BLOCK_UNSOLIDIFIED");
        assert_eq!(
            (unsolidified.kind(), unsolidified.retry()),
            (RejectionKind::Unclassified, RetryPolicy::TryAnotherEndpoint),
            "a lagging node refused before validating anything"
        );
    }
}
