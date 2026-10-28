use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jelly_core::{
    CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot, ScanAdvance,
    ScanStream, SignedTx,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::CHAIN;
use crate::cursor::Cursor;
use crate::error::XrpError;
use crate::payload::Payment;
use crate::sign;

const MISMATCH: XrpError = XrpError::Internal("a stored XRP send does not match its hash");
const SENDS_VERSION: u8 = 1;

/// The send one address holds, from its build until it settles.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub(crate) payment: Payment,
    pub(crate) stage: Stage,
}

/// How far a held send has got. Only a sent one is stored.
#[derive(Debug, Clone)]
pub(crate) enum Stage {
    Built,
    Signed(SignedBlob),
    Sent(SignedBlob),
}

/// A signed payment's hash and the blob that submits it, in upper-case hex.
#[derive(Debug, Clone)]
pub(crate) struct SignedBlob {
    pub(crate) hash: String,
    pub(crate) blob: String,
}

impl Pending {
    const fn signed(&self) -> Option<&SignedBlob> {
        match &self.stage {
            Stage::Built => None,
            Stage::Signed(signed) | Stage::Sent(signed) => Some(signed),
        }
    }

    const fn is_sent(&self) -> bool {
        matches!(self.stage, Stage::Sent(_))
    }
}

/// A [`Pending`] as the `xrp.sends` reservation stores it, so its field names are a stored
/// format. A stored entry is always sent, with both `hash` and `blob` set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSend {
    payment: Payment,
    hash: Option<String>,
    blob: Option<String>,
    is_sent: bool,
}

impl From<&Pending> for StoredSend {
    fn from(pending: &Pending) -> Self {
        let signed = pending.signed();
        Self {
            payment: pending.payment.clone(),
            hash: signed.map(|signed| signed.hash.clone()),
            blob: signed.map(|signed| signed.blob.clone()),
            is_sent: pending.is_sent(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sends {
    version: u8,
    #[serde(deserialize_with = "Option::deserialize")]
    pending: Option<StoredSend>,
}

/// The send each watched address holds, at most one, and the stored state behind it.
/// Once any stored state proves unreadable, every send is refused.
#[derive(Debug)]
pub(crate) struct States {
    cursors: Arc<dyn CursorStore>,
    is_blocked: AtomicBool,
    pending: RwLock<BTreeMap<i64, Pending>>,
}

impl States {
    pub(crate) fn new(cursors: Arc<dyn CursorStore>) -> Self {
        Self { cursors, is_blocked: AtomicBool::new(false), pending: RwLock::new(BTreeMap::new()) }
    }

    fn block_spending(&self) {
        self.is_blocked.store(true, Ordering::Release);
    }

    pub(crate) fn ensure_readable(&self) -> Result<(), XrpError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(XrpError::Internal("stored xrp state is unreadable"));
        }
        Ok(())
    }

    /// Reads the stored sends of `address_ids` and the stored cursors of the `admitted`
    /// ones, which are new to the watch set, and holds each admitted address's stored send
    /// unless it already holds one. Anything stored that can't be read blocks spending.
    pub(crate) async fn load(&self, address_ids: &[i64], admitted: &[i64]) -> Result<(), XrpError> {
        self.ensure_readable()?;
        let scans = if admitted.is_empty() {
            Vec::new()
        } else {
            self.cursors.load_scans(&scopes(admitted), &[ScanStream::XrpTransactions]).await?
        };
        let rows = self
            .cursors
            .load_reservations(&scopes(address_ids), &[ReservationSlot::XrpSends])
            .await?;
        let restored = restore(&scans, &rows).inspect_err(|_| self.block_spending())?;
        let mut pending = self.pending.write();
        for (address_id, stored) in restored {
            if admitted.contains(&address_id) {
                pending.entry(address_id).or_insert(stored);
            }
        }
        Ok(())
    }

    /// The history cursor stored for `address_id`, or a fresh one before its first walk.
    /// One that can't be read blocks spending.
    pub(crate) async fn cursor(&self, address_id: i64) -> Result<Cursor, XrpError> {
        let stored = self
            .cursors
            .load_scan(CursorScope::Address(address_id), ScanStream::XrpTransactions)
            .await?;
        let cursor = stored
            .map(|stored| stored.with_bytes(Cursor::decode).inspect_err(|_| self.block_spending()))
            .transpose()?;
        Ok(cursor.unwrap_or_default())
    }

    pub(crate) async fn advance(&self, address_id: i64, cursor: &Cursor) -> Result<(), XrpError> {
        let scope = CursorScope::Address(address_id);
        self.cursors.advance(scope, ScanStream::XrpTransactions, cursor.encode()?).await?;
        Ok(())
    }

    pub(crate) fn held(&self, address_id: i64) -> Option<Pending> {
        self.pending.read().get(&address_id).cloned()
    }

    pub(crate) fn is_held(&self, address_id: i64) -> bool {
        self.pending.read().contains_key(&address_id)
    }

    pub(crate) fn hold(&self, address_id: i64, payment: Payment) {
        self.pending.write().insert(address_id, Pending { payment, stage: Stage::Built });
    }

    /// Keeps the signed form of the held, unsent `payment`.
    pub(crate) fn attach_signed(
        &self,
        payment: &Payment,
        signed: &SignedTx,
    ) -> Result<(), XrpError> {
        let mut pending = self.pending.write();
        let entry = pending
            .values_mut()
            .find(|entry| entry.payment == *payment)
            .ok_or(XrpError::Invalid("Build the XRP payment again before signing."))?;
        if entry.is_sent() {
            return Err(XrpError::Busy);
        }
        entry.stage = Stage::Signed(SignedBlob {
            hash: signed.txid.value().to_owned(),
            blob: hex::encode_upper(signed.raw.as_bytes()),
        });
        Ok(())
    }

    /// Marks the held send that `tx` signs as sent, and stores it before returning the
    /// blob to submit, so a failed write keeps it from ever leaving.
    pub(crate) async fn mark_sent(&self, tx: &SignedTx) -> Result<String, XrpError> {
        self.ensure_readable()?;
        let invalid = XrpError::Invalid("The signed XRP payment is not valid.");
        if tx.chain() != CHAIN || sign::txid(tx.raw.as_bytes()) != tx.txid.value() {
            return Err(invalid);
        }
        let blob = hex::encode_upper(tx.raw.as_bytes());
        let (address_id, payment, signed) = self
            .pending
            .read()
            .iter()
            .find_map(|(address_id, entry)| {
                let signed = entry.signed().filter(|signed| signed.hash == tx.txid.value())?;
                Some((*address_id, entry.payment.clone(), signed.clone()))
            })
            .ok_or(XrpError::Invalid("Build the XRP payment again before sending."))?;
        if signed.blob != blob {
            return Err(invalid);
        }
        let sent = Pending { payment, stage: Stage::Sent(signed) };
        self.persist(address_id, Some(&sent)).await?;
        self.pending.write().insert(address_id, sent);
        Ok(blob)
    }

    /// Stops holding the unsent send of `address_id` without storing anything, because
    /// none of it was stored.
    pub(crate) fn release(&self, address_id: i64) {
        self.pending.write().remove(&address_id);
    }

    pub(crate) fn release_unsent(&self, payment: &Payment) {
        self.pending.write().retain(|_, entry| entry.is_sent() || entry.payment != *payment);
    }

    pub(crate) async fn persist(
        &self,
        address_id: i64,
        pending: Option<&Pending>,
    ) -> Result<(), XrpError> {
        self.ensure_readable()?;
        let bytes = serde_json::to_vec(&Sends {
            version: SENDS_VERSION,
            pending: pending.map(StoredSend::from),
        })
        .map_err(|_| XrpError::Internal("an XRP send did not serialize"))?;
        self.cursors
            .reserve(
                CursorScope::Address(address_id),
                ReservationSlot::XrpSends,
                CursorBytes::new(bytes),
            )
            .await?;
        Ok(())
    }

    /// Stores that `address_id` holds no send, then stops holding the one it did.
    pub(crate) async fn settle(&self, address_id: i64) -> Result<(), XrpError> {
        self.persist(address_id, None).await?;
        self.pending.write().remove(&address_id);
        Ok(())
    }
}

fn scopes(address_ids: &[i64]) -> Vec<CursorScope> {
    address_ids.iter().copied().map(CursorScope::Address).collect()
}

/// The sends stored in `rows`. Fails unless every cursor in `scans` and every send reads,
/// and each send was broadcast with a blob that hashes to its id.
fn restore(
    scans: &[ScanAdvance],
    rows: &[HeldReservation],
) -> Result<BTreeMap<i64, Pending>, XrpError> {
    for row in scans {
        row.cursor.with_bytes(Cursor::decode)?;
    }
    let mut restored = BTreeMap::new();
    for row in rows {
        let address_id = row
            .scope
            .address_id()
            .ok_or(XrpError::Internal("a stored XRP send belongs to no address"))?;
        let wire: Sends = row
            .payload
            .with_bytes(|bytes| serde_json::from_slice(bytes))
            .map_err(|_| XrpError::Internal("a stored XRP send is not readable"))?;
        if wire.version != SENDS_VERSION {
            return Err(XrpError::Internal("stored xrp sends have an unsupported format"));
        }
        if let Some(stored) = wire.pending {
            stored.payment.validate()?;
            if !stored.is_sent {
                return Err(XrpError::Internal("a stored XRP send was never broadcast"));
            }
            let blob = stored.blob.ok_or(MISMATCH)?;
            let hash = stored.hash.ok_or(MISMATCH)?;
            let bytes = hex::decode(&blob).map_err(|_| MISMATCH)?;
            if sign::txid(&bytes) != hash {
                return Err(MISMATCH);
            }
            let stage = Stage::Sent(SignedBlob { hash, blob });
            restored.insert(address_id, Pending { payment: stored.payment, stage });
        }
    }
    Ok(restored)
}
