use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jelly_core::{
    CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot, ScanAdvance,
    ScanStream, SignedTx,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::cursor::Cursor;
use crate::error::ZcashError;
use crate::payload::Payment;
use crate::sign;

const MISMATCH: ZcashError =
    ZcashError::Internal("a stored Zcash send does not match its transaction");
const NOT_SIGNED: ZcashError = ZcashError::Internal("a Zcash send is not signed");
const SENDS_VERSION: u8 = 1;

/// A signed payment's txid and its raw transaction, in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignedHex {
    pub(crate) txid: String,
    pub(crate) raw_hex: String,
}

impl From<&SignedTx> for SignedHex {
    fn from(tx: &SignedTx) -> Self {
        Self { txid: tx.txid.value().to_owned(), raw_hex: hex::encode(tx.raw.as_bytes()) }
    }
}

/// One send an address holds, from its build until it settles.
///
/// A payment from several sources is held under each of them, so every address's coins
/// stay reserved whichever address's state is read first.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub(crate) payment: Payment,
    stage: Stage,
}

/// How far a held send has got. Only a sent one is stored.
#[derive(Debug, Clone)]
enum Stage {
    Built,
    Signed(SignedHex),
    Sent(SignedHex),
}

impl Pending {
    /// The signed transaction, once there is one.
    pub(crate) const fn signed(&self) -> Option<&SignedHex> {
        match &self.stage {
            Stage::Built => None,
            Stage::Signed(signed) | Stage::Sent(signed) => Some(signed),
        }
    }

    pub(crate) const fn is_sent(&self) -> bool {
        matches!(self.stage, Stage::Sent(_))
    }

    /// This send as the reservation stores it once it is sent, if it is signed.
    fn stored(&self) -> Option<StoredSend> {
        let signed = self.signed()?;
        Some(StoredSend {
            payment: self.payment.clone(),
            txid: signed.txid.clone(),
            raw_hex: signed.raw_hex.clone(),
        })
    }
}

/// A sent [`Pending`] as the `zcash.sends` reservation stores it, so its field names are
/// a stored format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSend {
    payment: Payment,
    txid: String,
    raw_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sends {
    version: u8,
    pending: Vec<StoredSend>,
}

/// The sends each watched address holds and the stored state behind them. Once any
/// stored state proves unreadable, every send is refused.
#[derive(Debug)]
pub(crate) struct States {
    cursors: Arc<dyn CursorStore>,
    is_blocked: AtomicBool,
    pending: RwLock<BTreeMap<i64, Vec<Pending>>>,
}

impl States {
    pub(crate) fn new(cursors: Arc<dyn CursorStore>) -> Self {
        Self { cursors, is_blocked: AtomicBool::new(false), pending: RwLock::new(BTreeMap::new()) }
    }

    fn block_spending(&self) {
        self.is_blocked.store(true, Ordering::Release);
    }

    pub(crate) fn ensure_readable(&self) -> Result<(), ZcashError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(ZcashError::Internal("stored zcash state is unreadable"));
        }
        Ok(())
    }

    /// Reads the stored sends of `address_ids` and the stored cursors of the `admitted`
    /// ones, which are new to the watch set. Sends of addresses outside `address_ids` are
    /// dropped, and each admitted address holds every stored send it doesn't hold yet.
    /// Anything stored that can't be read blocks spending.
    pub(crate) async fn load(
        &self,
        address_ids: &[i64],
        admitted: &[i64],
    ) -> Result<(), ZcashError> {
        self.ensure_readable()?;
        let scans = if admitted.is_empty() {
            Vec::new()
        } else {
            self.cursors.load_scans(&scopes(admitted), &[ScanStream::ZcashTransactions]).await?
        };
        let rows = self
            .cursors
            .load_reservations(&scopes(address_ids), &[ReservationSlot::ZcashSends])
            .await?;
        let restored = restore(&scans, &rows).inspect_err(|_| self.block_spending())?;
        let mut pending = self.pending.write();
        pending.retain(|address_id, _| address_ids.contains(address_id));
        for (address_id, stored) in restored {
            if !admitted.contains(&address_id) {
                continue;
            }
            let held = pending.entry(address_id).or_default();
            for send in stored {
                if !held.iter().any(|other| other.payment == send.payment) {
                    held.push(send);
                }
            }
        }
        Ok(())
    }

    /// The history cursor stored for `address_id`, or a fresh one before its first scan.
    /// One that can't be read blocks spending.
    pub(crate) async fn cursor(&self, address_id: i64) -> Result<Cursor, ZcashError> {
        let stored = self
            .cursors
            .load_scan(CursorScope::Address(address_id), ScanStream::ZcashTransactions)
            .await?;
        let cursor = stored
            .map(|stored| stored.with_bytes(Cursor::decode).inspect_err(|_| self.block_spending()))
            .transpose()?;
        Ok(cursor.unwrap_or_default())
    }

    pub(crate) async fn advance(&self, address_id: i64, cursor: &Cursor) -> Result<(), ZcashError> {
        let scope = CursorScope::Address(address_id);
        self.cursors.advance(scope, ScanStream::ZcashTransactions, cursor.encode()?).await?;
        Ok(())
    }

    pub(crate) fn held(&self, address_id: i64) -> Vec<Pending> {
        self.pending.read().get(&address_id).cloned().unwrap_or_default()
    }

    /// Whether a payment held for any address spends the coin at `txid` and `index`.
    pub(crate) fn is_reserved(&self, txid: &str, index: u32) -> bool {
        self.pending.read().values().flatten().any(|send| {
            send.payment.inputs.iter().any(|coin| coin.txid == txid && coin.index == index)
        })
    }

    /// Holds `payment` under every one of its sources.
    pub(crate) fn hold(&self, payment: &Payment) {
        let mut pending = self.pending.write();
        for source in &payment.sources {
            pending
                .entry(source.address_id)
                .or_default()
                .push(Pending { payment: payment.clone(), stage: Stage::Built });
        }
    }

    /// Keeps the signed form of the held, unsent `payment`.
    pub(crate) fn attach_signed(
        &self,
        payment: &Payment,
        signed: &SignedTx,
    ) -> Result<(), ZcashError> {
        let signed = SignedHex::from(signed);
        self.update(payment, |send| {
            if send.is_sent() {
                return Err(ZcashError::Invalid("This Zcash payment has already been submitted."));
            }
            send.stage = Stage::Signed(signed.clone());
            Ok(())
        })
    }

    /// The held payment that `signed` signs.
    pub(crate) fn signed_payment(&self, signed: &SignedHex) -> Result<Payment, ZcashError> {
        self.pending
            .read()
            .values()
            .flatten()
            .find(|send| send.signed() == Some(signed))
            .map(|send| send.payment.clone())
            .ok_or(ZcashError::Invalid("Build the Zcash payment again before sending."))
    }

    /// Marks every held copy of `payment` as sent, after storing each source's sends with
    /// it included, so a failed write keeps it from being broadcast.
    pub(crate) async fn mark_sent(&self, payment: &Payment) -> Result<(), ZcashError> {
        self.ensure_readable()?;
        for source in &payment.sources {
            let sends = self
                .held(source.address_id)
                .iter()
                .filter(|send| send.is_sent() || send.payment == *payment)
                .map(|send| send.stored().ok_or(NOT_SIGNED))
                .collect::<Result<Vec<_>, _>>()?;
            self.persist(source.address_id, sends).await?;
        }
        self.update(payment, |send| {
            let signed = send.signed().cloned().ok_or(NOT_SIGNED)?;
            send.stage = Stage::Sent(signed);
            Ok(())
        })
        .map_err(|_| ZcashError::Internal("a Zcash send is no longer pending"))
    }

    pub(crate) fn release_unsent(&self, payment: &Payment) {
        let mut pending = self.pending.write();
        for source in &payment.sources {
            if let Some(sends) = pending.get_mut(&source.address_id) {
                sends.retain(|send| send.is_sent() || send.payment != *payment);
            }
        }
    }

    /// Stops holding `finished` under `address_id` alone, then stores what it still holds.
    pub(crate) async fn settle(
        &self,
        address_id: i64,
        finished: &[Payment],
    ) -> Result<(), ZcashError> {
        if let Some(sends) = self.pending.write().get_mut(&address_id) {
            sends.retain(|send| !finished.contains(&send.payment));
        }
        self.ensure_readable()?;
        let sent: Vec<StoredSend> = self
            .pending
            .read()
            .get(&address_id)
            .into_iter()
            .flatten()
            .filter(|send| send.is_sent())
            .filter_map(Pending::stored)
            .collect();
        self.persist(address_id, sent).await
    }

    /// Applies `change` to every held copy of `payment`, failing when no source holds
    /// one.
    fn update(
        &self,
        payment: &Payment,
        mut change: impl FnMut(&mut Pending) -> Result<(), ZcashError>,
    ) -> Result<(), ZcashError> {
        let mut pending = self.pending.write();
        let mut is_held = false;
        for source in &payment.sources {
            for send in pending
                .get_mut(&source.address_id)
                .into_iter()
                .flatten()
                .filter(|send| send.payment == *payment)
            {
                change(send)?;
                is_held = true;
            }
        }
        if is_held {
            Ok(())
        } else {
            Err(ZcashError::Invalid("Build the Zcash payment again before signing."))
        }
    }

    async fn persist(&self, address_id: i64, pending: Vec<StoredSend>) -> Result<(), ZcashError> {
        let bytes = serde_json::to_vec(&Sends { version: SENDS_VERSION, pending })
            .map_err(|_| ZcashError::Internal("a Zcash send did not serialize"))?;
        self.cursors
            .reserve(
                CursorScope::Address(address_id),
                ReservationSlot::ZcashSends,
                CursorBytes::new(bytes),
            )
            .await?;
        Ok(())
    }
}

fn scopes(address_ids: &[i64]) -> Vec<CursorScope> {
    address_ids.iter().copied().map(CursorScope::Address).collect()
}

/// The sends stored in `rows`, refused unless every cursor in `scans` and every send
/// decodes.
///
/// Each send must name the row's address among its sources and carry a signed
/// transaction of its payment that hashes to its txid.
fn restore(
    scans: &[ScanAdvance],
    rows: &[HeldReservation],
) -> Result<BTreeMap<i64, Vec<Pending>>, ZcashError> {
    for row in scans {
        row.cursor.with_bytes(Cursor::decode)?;
    }
    let mut restored = BTreeMap::new();
    for row in rows {
        let address_id = row
            .scope
            .address_id()
            .ok_or(ZcashError::Internal("a stored Zcash send belongs to no address"))?;
        let wire: Sends = row
            .payload
            .with_bytes(|bytes| serde_json::from_slice(bytes))
            .map_err(|_| ZcashError::Internal("a stored Zcash send is not readable"))?;
        if wire.version != SENDS_VERSION {
            return Err(ZcashError::Internal("stored zcash sends have an unsupported format"));
        }
        let mut sends = Vec::with_capacity(wire.pending.len());
        for StoredSend { payment, txid, raw_hex } in wire.pending {
            if !payment.sources.iter().any(|source| source.address_id == address_id) {
                return Err(ZcashError::Internal("a stored Zcash send belongs to another address"));
            }
            let raw = hex::decode(&raw_hex).map_err(|_| MISMATCH)?;
            let tx = sign::validate_signed(&payment, &raw).map_err(|_| MISMATCH)?;
            if tx.txid().to_string() != txid {
                return Err(MISMATCH);
            }
            sends.push(Pending { payment, stage: Stage::Sent(SignedHex { txid, raw_hex }) });
        }
        restored.insert(address_id, sends);
    }
    Ok(restored)
}
