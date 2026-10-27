//! The JSON-RPC shapes this build sends and reads.
//!
//! Members the wallet does not use are not declared, and an amount that arrives
//! as a decimal string is parsed here once.

use std::borrow::Cow;
use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::wire_enum;
use serde::de::{Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use solana_hash::Hash;
use solana_pubkey::Pubkey;

use crate::address;

wire_enum! {
    /// How settled a value must be before this build reads it.
    ///
    /// `processed` can vanish with a fork, `confirmed` is voted by a
    /// supermajority, and `finalized` is rooted and irreversible. This is also
    /// the vocabulary `getSignatureStatuses` answers with in
    /// `confirmationStatus`.
    pub(crate) enum Commitment {
        Processed => "processed",
        Confirmed => "confirmed",
        Finalized => "finalized",
    }
}

impl Commitment {
    /// True once a supermajority has voted for the block, which is when the
    /// wallet stops watching its own transaction.
    pub(crate) const fn is_settled(self) -> bool {
        matches!(self, Self::Confirmed | Self::Finalized)
    }
}

wire_enum! {
    /// The encodings this build sends transactions in and reads account and
    /// transaction data in.
    pub(crate) enum Encoding {
        Base64 => "base64",
        JsonParsed => "jsonParsed",
    }
}

/// `{"commitment": ...}`, the configuration object almost every read takes.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct CommitmentQuery {
    pub(crate) commitment: Commitment,
}

/// `{"encoding": ..., "commitment": ...}`, the configuration `getAccountInfo`
/// takes.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountQuery {
    pub(crate) encoding: Encoding,
    pub(crate) commitment: Commitment,
}

/// One page of `getSignaturesForAddress`, newest first.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageQuery<'a> {
    pub(crate) limit: u32,
    pub(crate) commitment: Commitment,
    /// Walks backwards from this signature, exclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) before: Option<&'a str>,
}

/// `getSignatureStatuses` looks only at a recent cache unless it is told to
/// search history, and a wallet asking about its own send needs the search.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusQuery {
    pub(crate) search_transaction_history: bool,
}

/// The version cap is the newest transaction format this build reads.
///
/// Without it a node refuses every versioned transaction, and a transaction
/// newer than the cap is refused with `-32015` rather than answered in a shape
/// this build could misread.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TxQuery {
    pub(crate) encoding: Encoding,
    pub(crate) commitment: Commitment,
    pub(crate) max_supported_transaction_version: u8,
}

/// A measurement-only simulation, with no signature check and the node's own
/// blockhash, so a compute-unit reading never needs a fresh blockhash.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SimulateQuery {
    pub(crate) encoding: Encoding,
    pub(crate) sig_verify: bool,
    pub(crate) replace_recent_blockhash: bool,
    pub(crate) commitment: Commitment,
}

/// `max_retries` is left out, so the node forwards the transaction to successive
/// leaders until its blockhash expires. Every pass also rebroadcasts the same
/// bytes.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendQuery {
    pub(crate) encoding: Encoding,
    /// False for a first attempt, so the node's simulation classifies a refusal.
    /// True for a rebroadcast, where preflight only reports what is already
    /// known.
    pub(crate) skip_preflight: bool,
    pub(crate) preflight_commitment: Commitment,
}

/// The `context` and `value` wrapper state reads answer with. The context slot is
/// not read, because the commitment asked for decides a value's age.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Contextual<T> {
    pub(crate) value: T,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockhashInfo {
    #[serde(deserialize_with = "base58_hash")]
    pub(crate) blockhash: Hash,
    /// The last block height at which a transaction carrying this blockhash can
    /// still land. Past it the transaction is dead and a rebuild is provably
    /// safe.
    pub(crate) last_valid_block_height: u64,
}

/// An account read with base64: the program that owns it and its data, which a
/// token account or a mint is unpacked from.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawAccount {
    #[serde(deserialize_with = "base58_pubkey")]
    pub(crate) owner: Pubkey,
    pub(crate) data: (String, Encoding),
}

impl RawAccount {
    /// `None` for data in another encoding or that does not decode.
    pub(crate) fn bytes(&self) -> Option<Vec<u8>> {
        if self.data.1 != Encoding::Base64 {
            return None;
        }
        BASE64.decode(&self.data.0).ok()
    }
}

/// Base units as a decimal string. The scale is the compiled registry's, never
/// the node's.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UiTokenAmount {
    #[serde(deserialize_with = "decimal_u64")]
    pub(crate) amount: u64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrioritizationFee {
    pub(crate) prioritization_fee: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignatureRow {
    pub(crate) signature: String,
    pub(crate) slot: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignatureStatus {
    pub(crate) slot: u64,
    #[serde(default)]
    pub(crate) err: Option<TransactionFailure>,
    /// Absent on nodes that predate the member, which reads as `processed`.
    #[serde(default)]
    pub(crate) confirmation_status: Option<Commitment>,
}

impl SignatureStatus {
    pub(crate) fn is_settled(&self) -> bool {
        self.confirmation_status.is_some_and(Commitment::is_settled)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Simulation {
    #[serde(default)]
    pub(crate) err: Option<TransactionFailure>,
    #[serde(default)]
    pub(crate) units_consumed: Option<u64>,
}

/// A validator `TransactionError`, reduced to its variant name.
///
/// The wire form is a bare string such as `"BlockhashNotFound"` or a
/// single-member object such as `{"InstructionError":[0,{"Custom":1}]}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransactionFailure(String);

impl TransactionFailure {
    #[cfg(test)]
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The variant name, or an empty string for a shape with no name in it.
    pub(crate) fn name(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FailureWire {
    Named(String),
    /// A single-member object whose one key is the variant name. The member's own
    /// value is a runtime type this build does not read.
    #[expect(
        clippy::zero_sized_map_values,
        reason = "a map is what the wire shape is; the key is the whole answer"
    )]
    Structured(BTreeMap<String, IgnoredAny>),
}

impl<'de> Deserialize<'de> for TransactionFailure {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = match FailureWire::deserialize(deserializer)? {
            FailureWire::Named(text) => text,
            FailureWire::Structured(map) => map.into_keys().next().unwrap_or_default(),
        };
        Ok(Self(name))
    }
}

/// One transaction as `jsonParsed` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransactionRow {
    pub(crate) slot: u64,
    #[serde(default)]
    pub(crate) block_time: Option<i64>,
    pub(crate) transaction: ParsedTransaction,
    #[serde(default)]
    pub(crate) meta: Option<TransactionMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ParsedTransaction {
    pub(crate) message: ParsedMessage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ParsedMessage {
    #[serde(default)]
    pub(crate) account_keys: Vec<ParsedAccountKey>,
    #[serde(default)]
    pub(crate) instructions: Vec<ParsedInstruction>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ParsedAccountKey {
    pub(crate) pubkey: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ParsedInstruction {
    #[serde(default)]
    pub(crate) program: Option<String>,
    /// Most programs' `parsed` member is an object, but spl-memo's is a bare
    /// string. A shape this build does not read names nothing it needs, so it
    /// reads as absent rather than failing the transaction.
    #[serde(default, deserialize_with = "readable_or_absent")]
    pub(crate) parsed: Option<ParsedBody>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ParsedBody {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) info: Option<ParsedInfo>,
}

/// The members the wallet reads out of a parsed instruction. Every one is
/// optional because the member set differs per instruction type.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ParsedInfo {
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) destination: Option<String>,
    #[serde(default)]
    pub(crate) lamports: Option<u64>,
    #[serde(default, rename = "newAccount")]
    pub(crate) new_account: Option<String>,
}

/// A node answers `null` for a list it did not record, which is how older
/// transactions arrive, so every list reads `null` as empty.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransactionMeta {
    #[serde(default)]
    pub(crate) err: Option<TransactionFailure>,
    #[serde(default)]
    pub(crate) fee: u64,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) pre_balances: Vec<u64>,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) post_balances: Vec<u64>,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) pre_token_balances: Vec<TokenBalance>,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) post_token_balances: Vec<TokenBalance>,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) inner_instructions: Vec<InnerInstructions>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InnerInstructions {
    #[serde(default)]
    pub(crate) instructions: Vec<ParsedInstruction>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenBalance {
    pub(crate) mint: String,
    /// Absent on very old records, where the account cannot be attributed to an
    /// owner and is therefore not this wallet's.
    #[serde(default)]
    pub(crate) owner: Option<String>,
    pub(crate) ui_token_amount: UiTokenAmount,
}

/// One JSON-RPC response kept whole.
///
/// The shared decoder keeps only an error's code and message. A
/// `sendTransaction` refusal carries its preflight result in `data`, so the send
/// path reads the envelope itself.
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope<'a> {
    #[serde(borrow, default)]
    pub(crate) result: Option<&'a RawValue>,
    #[serde(default)]
    pub(crate) error: Option<RpcErrorObject>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RpcErrorObject {
    pub(crate) code: i64,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) data: Option<Preflight>,
}

/// What a refused `sendTransaction` carries in its error `data`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preflight {
    #[serde(default)]
    pub(crate) err: Option<TransactionFailure>,
    #[serde(default, deserialize_with = "nullable_list")]
    pub(crate) logs: Vec<String>,
}

fn decimal_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = Cow::<'de, str>::deserialize(deserializer)?;
    text.parse::<u64>().map_err(|_| D::Error::custom("base units are not a whole number"))
}

fn base58_pubkey<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Pubkey, D::Error> {
    let text = Cow::<'de, str>::deserialize(deserializer)?;
    address::parse("account owner", &text)
        .map_err(|_| D::Error::custom("an account owner is not an address"))
}

fn base58_hash<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Hash, D::Error> {
    let text = Cow::<'de, str>::deserialize(deserializer)?;
    text.parse().map_err(|_| D::Error::custom("a blockhash is not a hash"))
}

fn nullable_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn readable_or_absent<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient<T> {
        Readable(T),
        Other(IgnoredAny),
    }
    Ok(match Lenient::<T>::deserialize(deserializer)? {
        Lenient::Readable(value) => Some(value),
        Lenient::Other(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use solana_system_interface::program::ID as SYSTEM_PROGRAM;

    use super::*;

    fn parse<T: DeserializeOwned>(json: &str) -> T {
        serde_json::from_str(json).expect("the fixture parses")
    }

    #[test]
    fn only_a_voted_commitment_is_settled() {
        assert_eq!(Commitment::COUNT, 3);
        assert!(!Commitment::Processed.is_settled());
        assert!(Commitment::Confirmed.is_settled());
        assert!(Commitment::Finalized.is_settled());
    }

    #[test]
    fn a_page_omits_the_anchors_it_does_not_have() {
        let bare = PageQuery { limit: 10, commitment: Commitment::Finalized, before: None };
        assert_eq!(
            serde_json::to_string(&bare).expect("it serialises"),
            r#"{"limit":10,"commitment":"finalized"}"#
        );
        let anchored = PageQuery { before: Some("a"), ..bare };
        assert_eq!(
            serde_json::to_string(&anchored).expect("it serialises"),
            r#"{"limit":10,"commitment":"finalized","before":"a"}"#
        );
    }

    #[test]
    fn a_token_amount_arrives_as_a_string_and_is_read_as_an_integer() {
        let amount: UiTokenAmount = parse(r#"{"amount":"18446744073709551615","decimals":6}"#);
        assert_eq!(amount.amount, u64::MAX);
        serde_json::from_str::<UiTokenAmount>(r#"{"amount":"1.5","decimals":6}"#).unwrap_err();
        serde_json::from_str::<UiTokenAmount>(r#"{"amount":1,"decimals":6}"#).unwrap_err();
    }

    #[test]
    fn a_transaction_failure_is_read_as_a_name_whichever_shape_it_arrives_in() {
        assert_eq!(
            parse::<TransactionFailure>(r#""BlockhashNotFound""#).name(),
            "BlockhashNotFound"
        );
        assert_eq!(
            parse::<TransactionFailure>(r#"{"InstructionError":[0,{"Custom":1}]}"#).name(),
            "InstructionError"
        );
        assert_eq!(
            parse::<TransactionFailure>(r#"{"InsufficientFundsForRent":{"account_index":1}}"#)
                .name(),
            "InsufficientFundsForRent"
        );
        assert_eq!(parse::<TransactionFailure>("{}").name(), "");
    }

    #[test]
    fn a_status_without_a_confirmation_level_is_not_settled() {
        let bare: SignatureStatus = parse(r#"{"slot":7,"confirmations":1,"err":null}"#);
        assert_eq!(bare.slot, 7);
        assert!(bare.err.is_none());
        assert!(!bare.is_settled());

        let voted: SignatureStatus =
            parse(r#"{"slot":7,"err":null,"confirmationStatus":"confirmed"}"#);
        assert!(voted.is_settled());
    }

    #[test]
    fn a_send_refusal_keeps_its_structured_preflight_result() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32002,
            "message":"Transaction simulation failed",
            "data":{"err":"BlockhashNotFound","logs":[],"unitsConsumed":0}}}"#;
        let envelope: Envelope<'_> = serde_json::from_str(body).expect("the fixture parses");
        let failure = envelope.error.expect("an error object");
        assert_eq!(failure.code, -32002);
        let preflight = failure.data.expect("preflight data");
        assert_eq!(preflight.err.expect("a failure").name(), "BlockhashNotFound");

        for logs in [r#""logs":null,"#, ""] {
            let bare: Preflight = parse(&format!(r#"{{{logs}"err":"AccountInUse"}}"#));
            assert!(bare.logs.is_empty(), "{logs}");
        }
    }

    #[test]
    fn a_metadata_list_the_node_did_not_record_reads_as_empty() {
        let metadata: TransactionMeta = parse(
            r#"{"err":null,"fee":5000,"preBalances":[1],"postBalances":[1],
                "preTokenBalances":null,"postTokenBalances":null,"innerInstructions":null}"#,
        );
        assert!(metadata.pre_token_balances.is_empty());
        assert!(metadata.post_token_balances.is_empty());
        assert!(metadata.inner_instructions.is_empty());
        assert_eq!(metadata.pre_balances, vec![1]);
    }

    #[test]
    fn a_parsed_member_that_is_not_an_object_reads_as_absent() {
        let memo: ParsedInstruction = parse(r#"{"parsed":"invoice 42","program":"spl-memo"}"#);
        assert!(memo.parsed.is_none());
        assert_eq!(memo.program.as_deref(), Some("spl-memo"));

        let transfer: ParsedInstruction =
            parse(r#"{"parsed":{"type":"transfer","info":{"lamports":5}},"program":"system"}"#);
        let body = transfer.parsed.expect("a parsed body");
        assert_eq!(body.kind, "transfer");
        assert_eq!(body.info.and_then(|info| info.lamports), Some(5));
    }

    #[test]
    fn an_account_hands_over_its_data_only_when_it_arrived_as_base64() {
        let account: RawAccount =
            parse(r#"{"data":["AQID","base64"],"owner":"11111111111111111111111111111111"}"#);
        assert_eq!(account.bytes(), Some(vec![1, 2, 3]));
        let parsed: RawAccount =
            parse(r#"{"data":["AQID","jsonParsed"],"owner":"11111111111111111111111111111111"}"#);
        assert_eq!(parsed.bytes(), None, "only base64 is read as bytes");
        let broken: RawAccount =
            parse(r#"{"data":["not base64","base64"],"owner":"11111111111111111111111111111111"}"#);
        assert_eq!(broken.bytes(), None);
    }

    #[test]
    fn a_blockhash_and_an_owner_are_read_as_keys_and_refused_when_they_are_not() {
        let info: BlockhashInfo = parse(
            r#"{"blockhash":"HezQLwwK3aecM7H2cRSqFhidamdAHUNXcUgiyP7FcTjA","lastValidBlockHeight":9}"#,
        );
        assert_eq!(info.blockhash.to_string(), "HezQLwwK3aecM7H2cRSqFhidamdAHUNXcUgiyP7FcTjA");
        serde_json::from_str::<BlockhashInfo>(
            r#"{"blockhash":"not a hash","lastValidBlockHeight":9}"#,
        )
        .unwrap_err();

        let account: RawAccount =
            parse(r#"{"data":["","base64"],"owner":"11111111111111111111111111111111"}"#);
        assert_eq!(account.owner, SYSTEM_PROGRAM);
        serde_json::from_str::<RawAccount>(r#"{"data":["","base64"],"owner":"not an address"}"#)
            .unwrap_err();
    }

    #[test]
    fn a_token_amount_with_an_escaped_digit_still_reads() {
        let amount: UiTokenAmount = parse(r#"{"amount":"\u0037","decimals":6}"#);
        assert_eq!(amount.amount, 7);
        let value = serde_json::json!({ "amount": "42", "decimals": 6 });
        let owned: UiTokenAmount = serde_json::from_value(value).expect("an owned answer reads");
        assert_eq!(owned.amount, 42);
    }
}
