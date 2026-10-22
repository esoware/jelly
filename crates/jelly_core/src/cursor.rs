use std::fmt;

use async_trait::async_trait;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::CoreError;
use crate::wire_enum;

wire_enum! {
    /// Where one chain records how far a pass has read.
    ///
    /// The string form is `<chain-or-family>.<stream>`. It is fixed in
    /// [`ScanStream::as_str`] and never concatenated at runtime.
    pub enum ScanStream {
        BitcoinElectrumStatus => "bitcoin.electrum_status",
        LitecoinElectrumStatus => "litecoin.electrum_status",
        DogecoinElectrumStatus => "dogecoin.electrum_status",
        BitcoinCashElectrumStatus => "bitcoin_cash.electrum_status",
        ZcashTransactions => "zcash.transactions",
        EvmLogScan => "evm.log_scan",
        SolanaSignatures => "solana.signatures",
        TronTransfers => "tron.transfers",
        XrpTransactions => "xrp.transactions",
        TonTransactions => "ton.transactions",
        MoneroScan => "monero.scan",
    }
}

impl ScanStream {
    /// Whether the stream records only how much history a pass has covered, so forgetting
    /// it costs a rescan and nothing else. The others keep their last committed
    /// checkpoint, conflict and reconciliation state included, for the next pass to
    /// reconcile against.
    pub const fn is_reconstructible(self) -> bool {
        match self {
            Self::SolanaSignatures
            | Self::TronTransfers
            | Self::XrpTransactions
            | Self::TonTransactions => true,
            Self::BitcoinElectrumStatus
            | Self::LitecoinElectrumStatus
            | Self::DogecoinElectrumStatus
            | Self::BitcoinCashElectrumStatus
            | Self::EvmLogScan
            | Self::ZcashTransactions
            | Self::MoneroScan => false,
        }
    }
}

wire_enum! {
    /// Reservations that must survive a restart. Zcash retains its expiring outpoints,
    /// account chains retain nonces or signatures, and Monero retains key images.
    pub enum ReservationSlot {
        ZcashSends => "zcash.sends",
        EvmNonces => "evm.nonces",
        SolanaSends => "solana.sends",
        TronSends => "tron.sends",
        XrpSends => "xrp.sends",
        TonSends => "ton.sends",
        MoneroSends => "monero.sends",
    }
}

/// Opaque cursor bytes wiped on drop. Monero payloads can contain output keys, key images,
/// and signed transactions.
///
/// Read through `with_bytes`. No serialization or display is available, and `Debug` reveals
/// only the length.
#[derive(Clone, Default, PartialEq, Eq, Zeroize)]
pub struct CursorBytes(Zeroizing<Vec<u8>>);

impl CursorBytes {
    /// Takes ownership without copying secret bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn copied(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }

    #[cfg(feature = "test-util")]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(feature = "test-util")]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrows bytes within the closure. Callers must zeroize any copies they retain.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(&self.0)
    }
}

impl ZeroizeOnDrop for CursorBytes {}

impl fmt::Debug for CursorBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CursorBytes(<redacted {} bytes>)", self.0.len())
    }
}

/// Whose state a cursor holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CursorScope {
    /// The wallet as a whole, for state that belongs to no one address, such as Monero's
    /// scan and its sends.
    Wallet,
    /// One watched address, by its row ID.
    Address(i64),
}

impl CursorScope {
    pub const fn address_id(self) -> Option<i64> {
        match self {
            Self::Wallet => None,
            Self::Address(address_id) => Some(address_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanAdvance {
    pub scope: CursorScope,
    pub stream: ScanStream,
    pub cursor: CursorBytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldReservation {
    pub scope: CursorScope,
    pub slot: ReservationSlot,
    pub payload: CursorBytes,
}

/// Stores scan progress and transaction reservations. The engine routes each opaque payload
/// to the database or vault without exposing that choice to adapters.
///
/// - `advance` buffers progress until the database transaction containing the scanned rows
///   commits. It must never persist progress ahead of those rows. A crash may require
///   rescanning.
/// - `reserve` is durable on success. Every adapter that holds a `ReservationSlot` must
///   record its reservation before broadcast. A failed write must stop the send.
///
/// Adapters must include enough context in scan payloads to detect reorgs and rewind.
#[async_trait]
pub trait CursorStore: Send + Sync + fmt::Debug {
    /// Reads every pair of `scopes` and `streams` in one query. Unwritten pairs are absent.
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError>;

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError>;

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError>;

    /// Replaces the slot's payload in one write. An error means the slot still holds what
    /// it held before, so an adapter that persists a send before broadcasting it can trust
    /// that a failed write left no record of a send it never made.
    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError>;

    async fn load_scan(
        &self,
        scope: CursorScope,
        stream: ScanStream,
    ) -> Result<Option<CursorBytes>, CoreError> {
        let rows = self.load_scans(&[scope], &[stream]).await?;
        Ok(rows.into_iter().next().map(|row| row.cursor))
    }

    async fn load_reservation(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
    ) -> Result<Option<CursorBytes>, CoreError> {
        let rows = self.load_reservations(&[scope], &[slot]).await?;
        Ok(rows.into_iter().next().map(|row| row.payload))
    }
}
