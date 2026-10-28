use std::fmt;

use serde::{Deserialize, Serialize};

use crate::amount::Amount;
use crate::asset::AssetId;
use crate::chain::ChainId;
use crate::tx::fee::FeeSelection;
use crate::tx::id::TxId;
use crate::wire_enum;

/// Decoded preview or fee detail. Labels are sentence case, not wire enum names.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxDetail {
    pub label: String,
    pub value: String,
}

impl TxDetail {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into() }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Why a preview carries a warning.
    pub enum TxWarningKind {
        DustOutput => "dust_output",
        /// The fee is unusually high: far above the market's rate, or above the chain's
        /// high-fee limit.
        HighFee => "high_fee",
        DroppedChange => "dropped_change",
        AmbiguousAddress => "ambiguous_address",
        RemoteDecoys => "remote_decoys",
        AccountActivation => "account_activation",
        AccountDeletion => "account_deletion",
        RentExemption => "rent_exemption",
        Replacement => "replacement",
        /// A second transaction that pays the fee of an unconfirmed one.
        ChildPays => "child_pays",
        Estimated => "estimated",
        Other => "other",
    }
}

/// A warning shown before signing. `message` is displayed verbatim and must use complete,
/// sentence-case sentences ending in periods.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxWarning {
    pub kind: TxWarningKind,
    pub message: String,
}

impl TxWarning {
    pub fn new(kind: TxWarningKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

/// How much one outgoing transfer moves.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendAmount {
    Exact {
        value: Amount,
    },
    /// Everything the address holds of the asset, less the fee when the asset pays it.
    Max,
}

/// The user's intent for one outgoing transfer.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxRequest {
    /// The addresses the transfer spends from, in the order the user chose them. A UTXO
    /// chain pools their coins and returns change to the first. Every other chain takes
    /// exactly one.
    pub from_address_ids: Vec<i64>,
    /// As the user typed it. The adapter validates it against the chain.
    pub to: String,
    /// The XRP destination tag. Every other chain refuses a request that carries one.
    pub destination_tag: Option<u32>,
    pub comment: Option<String>,
    /// Deletes the sending account and pays the recipient everything it holds, less the
    /// deletion fee. Only a chain whose spec allows it takes one, and the amount must be
    /// [`SendAmount::Max`].
    pub should_delete_account: bool,
    pub asset: AssetId,
    pub amount: SendAmount,
    pub fee: FeeSelection,
    /// Used instead of the sending account's next nonce. Only a chain whose spec allows it
    /// takes one.
    pub nonce: Option<u64>,
    /// Calldata as hex, with or without `0x`, sent in place of the generated transfer.
    /// Only a chain whose spec allows it takes any.
    pub data: Option<String>,
}

impl TxRequest {
    /// The one source of a request that names exactly one, as every chain but a UTXO
    /// chain needs.
    pub const fn sole_source(&self) -> Option<i64> {
        match *self.from_address_ids.as_slice() {
            [address_id] => Some(address_id),
            _ => None,
        }
    }
}

/// Decoded transaction for user approval. Only previewed data may be signed. The adapter
/// supplies canonical `from` and `to` addresses.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxPreview {
    pub chain: ChainId,
    pub asset: AssetId,
    /// The sending addresses the transaction moves funds of, in the order the request
    /// named them. Change, when there is any, returns to the first.
    pub from: Vec<String>,
    pub to: String,
    /// The destination tag the payment carries, on a chain whose payments can carry one.
    pub destination_tag: Option<u32>,
    pub amount: Amount,
    pub fee: Amount,
    /// What the sender pays besides the fee to open the recipient's token account, in
    /// the native asset. Only the recipient can reclaim it.
    pub rent: Option<Amount>,
    pub details: Vec<TxDetail>,
    pub warnings: Vec<TxWarning>,
}

/// State carried with an unsigned transaction, opaque to everything but the adapter
/// that produced it.
#[derive(Clone, PartialEq, Eq)]
pub struct TxPayload(Vec<u8>);

impl TxPayload {
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for TxPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxPayload({} bytes)", self.0.len())
    }
}

/// Signed wire bytes. After an ambiguous broadcast, resend these exact bytes rather than
/// building another payment.
#[derive(Clone, PartialEq, Eq)]
pub struct RawTx(Vec<u8>);

impl RawTx {
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for RawTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawTx({} bytes)", self.0.len())
    }
}

/// Unsigned transaction the engine holds and returns to its adapter. It never crosses IPC,
/// and its chain is recorded only in the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedTx {
    pub preview: TxPreview,
    pub payload: TxPayload,
}

impl UnsignedTx {
    pub const fn chain(&self) -> ChainId {
        self.preview.chain
    }
}

/// Signed bytes and their precomputed transaction ID. The ID supports lookup after an
/// ambiguous broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTx {
    pub raw: RawTx,
    pub txid: TxId,
}

impl SignedTx {
    pub const fn chain(&self) -> ChainId {
        self.txid.chain()
    }
}
