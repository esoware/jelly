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
const SENDS_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedSigned {
    raw_hex: String,
    pub(crate) txid: String,
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
    Signed(SavedSigned),
    Sent(SavedSigned),
}

impl Pending {
    /// The signed transaction, once there is one.
    pub(crate) const fn signed(&self) -> Option<&SavedSigned> {
        match &self.stage {
            Stage::Built => None,
            Stage::Signed(signed) | Stage::Sent(signed) => Some(signed),
        }
    }

    pub(crate) const fn is_sent(&self) -> bool {
        matches!(self.stage, Stage::Sent(_))
    }

    /// The same send as broadcast, which only a signed one can be.
    fn sent(&self) -> Result<Self, ZcashError> {
        let signed = self.signed().ok_or(ZcashError::Internal("a Zcash send is not signed"))?;
        Ok(Self { payment: self.payment.clone(), stage: Stage::Sent(signed.clone()) })
    }
}

/// A [`Pending`] as the `zcash.sends` reservation stores it, so its field names are a
/// stored format. A stored entry is always sent, with `signed` set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSend {
    payment: Payment,
    signed: Option<SavedSigned>,
    is_sent: bool,
}

impl From<&Pending> for StoredSend {
    fn from(pending: &Pending) -> Self {
        Self {
            payment: pending.payment.clone(),
            signed: pending.signed().cloned(),
            is_sent: pending.is_sent(),
        }
    }
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
    /// dropped, and each admitted address holds every stored send it does not hold yet.
    /// Anything stored that cannot be read blocks spending.
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
            for entry in stored {
                if !held.iter().any(|other| other.payment == entry.payment) {
                    held.push(entry);
                }
            }
        }
        Ok(())
    }

    /// The history cursor stored for `address_id`, or a fresh one before its first scan.
    /// One that cannot be read blocks spending.
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
        self.pending.read().values().flatten().any(|entry| {
            entry.payment.inputs.iter().any(|held| held.txid == txid && held.index == index)
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
        let saved = SavedSigned {
            raw_hex: hex::encode(signed.raw.as_bytes()),
            txid: signed.txid.value().to_owned(),
        };
        self.update(payment, |entry| {
            if entry.is_sent() {
                return Err(ZcashError::Invalid("This Zcash payment has already been submitted."));
            }
            entry.stage = Stage::Signed(saved.clone());
            Ok(())
        })
    }

    /// The held payment signed as `raw_hex` with `txid`.
    pub(crate) fn signed_payment(&self, raw_hex: &str, txid: &str) -> Result<Payment, ZcashError> {
        self.pending
            .read()
            .values()
            .flatten()
            .find(|entry| {
                entry
                    .signed()
                    .is_some_and(|signed| signed.raw_hex == raw_hex && signed.txid == txid)
            })
            .map(|entry| entry.payment.clone())
            .ok_or(ZcashError::Invalid("Build the Zcash payment again before sending."))
    }

    /// Marks every held copy of `payment` as sent, and stores the sends of each of its
    /// sources before returning, so a failed write keeps it from ever leaving.
    pub(crate) async fn mark_sent(&self, payment: &Payment) -> Result<(), ZcashError> {
        self.ensure_readable()?;
        for source in &payment.sources {
            let entries = self
                .held(source.address_id)
                .iter()
                .filter(|entry| entry.is_sent() || entry.payment == *payment)
                .map(Pending::sent)
                .collect::<Result<Vec<_>, _>>()?;
            self.persist(source.address_id, entries).await?;
        }
        self.update(payment, |entry| {
            *entry = entry.sent()?;
            Ok(())
        })
        .map_err(|_| ZcashError::Internal("a Zcash send is no longer pending"))
    }

    pub(crate) fn release_unsent(&self, payment: &Payment) {
        let mut pending = self.pending.write();
        for source in &payment.sources {
            if let Some(entries) = pending.get_mut(&source.address_id) {
                entries.retain(|entry| entry.is_sent() || entry.payment != *payment);
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
        let sent: Vec<Pending> = self
            .pending
            .read()
            .get(&address_id)
            .into_iter()
            .flatten()
            .filter(|entry| entry.is_sent())
            .cloned()
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
            for entry in pending
                .get_mut(&source.address_id)
                .into_iter()
                .flatten()
                .filter(|entry| entry.payment == *payment)
            {
                change(entry)?;
                is_held = true;
            }
        }
        if is_held {
            Ok(())
        } else {
            Err(ZcashError::Invalid("Build the Zcash payment again before signing."))
        }
    }

    async fn persist(&self, address_id: i64, pending: Vec<Pending>) -> Result<(), ZcashError> {
        let pending = pending.iter().map(StoredSend::from).collect();
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

/// The sends stored in `rows`, once every stored cursor in `scans` and every stored send
/// reads.
///
/// Each send must be a broadcast one of its own address, whose signed transaction is
/// its payment's and hashes to its txid.
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
        for entry in wire.pending {
            if !entry.is_sent {
                return Err(ZcashError::Internal("a stored Zcash send was never broadcast"));
            }
            if !entry.payment.sources.iter().any(|source| source.address_id == address_id) {
                return Err(ZcashError::Internal("a stored Zcash send belongs to another address"));
            }
            let signed = entry.signed.ok_or(MISMATCH)?;
            let raw = hex::decode(&signed.raw_hex).map_err(|_| MISMATCH)?;
            let tx = sign::validate_signed(&entry.payment, &raw).map_err(|_| MISMATCH)?;
            if tx.txid().to_string() != signed.txid {
                return Err(MISMATCH);
            }
            sends.push(Pending { payment: entry.payment, stage: Stage::Sent(signed) });
        }
        restored.insert(address_id, sends);
    }
    Ok(restored)
}
