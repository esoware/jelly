//! Shared types and contracts. Every crate reads the clock and OS entropy through here.

#![deny(clippy::float_arithmetic)]

mod adapter;
mod address;
mod amount;
mod asset;
mod build;
mod chain;
mod clock;
mod coin;
mod cursor;
mod endpoint;
mod entry;
mod error;
mod event;
mod fiat;
mod phrase;
mod pin;
mod proxy;
mod random;
mod secret;
mod spec;
mod table;
mod transport;
mod tx;
mod uri;
mod wallet;
mod wallet_color;
mod wire;

#[cfg(feature = "test-util")]
pub mod test_util;

pub use crate::adapter::{
    ChainAdapter, FeeBump, PendingTracker, ScriptProbe, Subaddresses, TxCancel, sole_key,
};
pub use crate::address::{Address, AddressCodec, AddressError};
pub use crate::amount::{Amount, DecimalError, FeeRate};
pub use crate::asset::{AssetId, TokenId};
pub use crate::build::{
    AdapterBuilder, AdapterSet, BuildError, BuildRequest, ChainOutcome, KeyService, SkipReason,
};
pub use crate::chain::{ChainId, ScriptKind};
pub use crate::clock::{Clock, ClockError, Deadline, SystemClock, duration_millis};
pub use crate::coin::Coin;
pub use crate::cursor::{
    CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot, ScanAdvance,
    ScanStream,
};
pub use crate::endpoint::{
    EndpointAttempt, EndpointHealth, EndpointHealthRecord, EndpointLabel, EndpointOverride,
    FailureKind,
};
pub use crate::entry::{ChainEntry, ImportedKey};
pub use crate::error::{
    BoxedSource, ChainError, CoreError, ErrorCode, KeyError, Rejection, RejectionKind, RetryPolicy,
};
pub use crate::event::{BalanceUpdate, ChainSyncProgress, EventSink, TxConfirmation, WalletEvent};
pub use crate::fiat::FiatCurrency;
pub use crate::phrase::phrase_words;
pub use crate::pin::{PinStore, SpkiPin};
pub use crate::proxy::ProxySetting;
pub use crate::random::{EntropyError, random_bytes, random_fill};
pub use crate::secret::{KeyKind, KeyMaterial, SecretText};
pub use crate::spec::ChainSpec;
pub use crate::table::ChainTable;
pub use crate::transport::Transport;
pub use crate::tx::{
    CustomFee, FeeLevel, FeeQuote, FeeSelection, LevelFee, PendingTxId, RawTx, SendAmount,
    SignedTx, TxDetail, TxDetection, TxDirection, TxId, TxOutcome, TxPayload, TxPreview, TxRecord,
    TxRecordBuilder, TxRequest, TxWarning, TxWarningKind, UnsignedTx, confirmations,
};
pub use crate::uri::{PaymentUri, payment_uri};
pub use crate::wallet::{DerivationPath, KeySource, PassphrasePolicy, PhraseWords, WatchedAddress};
pub use crate::wallet_color::WalletColor;
