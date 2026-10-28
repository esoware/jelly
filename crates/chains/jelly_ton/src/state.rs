use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use jelly_core::{
    Clock, CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot, ScanAdvance,
    ScanStream, SignedTx,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tonlib_core::cell::Cell;
use tonlib_core::tlb_types::block::message::Message;
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::wallet::version_helper::VersionHelper;

use crate::config::CHAIN;
use crate::cursor::Cursor;
use crate::error::TonError;
use crate::message;
use crate::payload::Payment;
use crate::sign::{normalized_hash, signature_of};
use crate::wallet::ton_wallet;

const MISMATCH: TonError = TonError::Internal("a TON send does not match its signed message");
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
    Signed(SignedBoc),
    Sent(SignedBoc),
}

/// A signed payment's normalized hash and its external message, as base64 BOC.
#[derive(Debug, Clone)]
pub(crate) struct SignedBoc {
    pub(crate) hash: String,
    pub(crate) boc: String,
}

impl Pending {
    const fn signed(&self) -> Option<&SignedBoc> {
        match &self.stage {
            Stage::Built => None,
            Stage::Signed(signed) | Stage::Sent(signed) => Some(signed),
        }
    }

    const fn is_sent(&self) -> bool {
        matches!(self.stage, Stage::Sent(_))
    }
}

/// Checks that `signed.boc` is the signed external message of `payment`, which the caller
/// has already validated, and that `signed.hash` is its normalized hash.
fn validate_signed(payment: &Payment, signed: &SignedBoc) -> Result<(), TonError> {
    let cell = Cell::from_boc(&STANDARD.decode(&signed.boc).map_err(|_| MISMATCH)?)?;
    let wallet = ton_wallet(&payment.pubkey, payment.version)?;
    let external = Message::from_cell(&cell)?;
    let body = &external.body.value;
    let signature = signature_of(payment.version, body).map_err(|_| MISMATCH)?;
    let expected = VersionHelper::sign_msg(wallet.version, &message::body(payment)?, &signature)?;
    if *body.as_ref() != expected
        || signed.hash != normalized_hash(&wallet.address, body)?
        || wallet.wrap_signed_body(expected, payment.is_deploy)? != cell
    {
        return Err(MISMATCH);
    }
    Ok(())
}

/// A [`Pending`] as the `ton.sends` reservation stores it, so its field names are a stored
/// format. A stored entry is always sent, with both `hash` and `boc` set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSend {
    payment: Payment,
    hash: Option<String>,
    boc: Option<String>,
    is_sent: bool,
}

impl From<&Pending> for StoredSend {
    fn from(pending: &Pending) -> Self {
        let signed = pending.signed();
        Self {
            payment: pending.payment.clone(),
            hash: signed.map(|signed| signed.hash.clone()),
            boc: signed.map(|signed| signed.boc.clone()),
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

    pub(crate) fn ensure_readable(&self) -> Result<(), TonError> {
        if self.is_blocked.load(Ordering::Acquire) {
            return Err(TonError::Internal("stored ton state is unreadable"));
        }
        Ok(())
    }

    /// Reads the stored sends of `address_ids` and the stored cursors of the `admitted`
    /// ones, which are new to the watch set, and holds each admitted address's stored send
    /// unless it already holds one. Anything stored that can't be read blocks spending.
    pub(crate) async fn load(&self, address_ids: &[i64], admitted: &[i64]) -> Result<(), TonError> {
        self.ensure_readable()?;
        let scans = if admitted.is_empty() {
            Vec::new()
        } else {
            self.cursors.load_scans(&scopes(admitted), &[ScanStream::TonTransactions]).await?
        };
        let rows = self
            .cursors
            .load_reservations(&scopes(address_ids), &[ReservationSlot::TonSends])
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
    pub(crate) async fn cursor(&self, address_id: i64) -> Result<Cursor, TonError> {
        let stored = self
            .cursors
            .load_scan(CursorScope::Address(address_id), ScanStream::TonTransactions)
            .await?;
        let cursor = stored
            .map(|stored| stored.with_bytes(Cursor::decode).inspect_err(|_| self.block_spending()))
            .transpose()?;
        Ok(cursor.unwrap_or_default())
    }

    pub(crate) async fn advance(&self, address_id: i64, cursor: &Cursor) -> Result<(), TonError> {
        let scope = CursorScope::Address(address_id);
        self.cursors.advance(scope, ScanStream::TonTransactions, cursor.encode()?).await?;
        Ok(())
    }

    pub(crate) fn held(&self, address_id: i64) -> Option<Pending> {
        self.pending.read().get(&address_id).cloned()
    }

    pub(crate) fn is_held(&self, address_id: i64) -> bool {
        self.pending.read().contains_key(&address_id)
    }

    /// The lt the sent send of `address_id` was built after, which a walk reaches back to
    /// so it sees the send settle.
    pub(crate) fn sent_floor(&self, address_id: i64) -> Option<u64> {
        self.pending
            .read()
            .get(&address_id)
            .filter(|pending| pending.is_sent())
            .map(|pending| pending.payment.before_lt)
    }

    /// Whether `txid` is the send `address_id` holds as sent, which its record settles.
    pub(crate) fn is_settled_by(&self, address_id: i64, txid: &str) -> bool {
        self.pending
            .read()
            .get(&address_id)
            .is_some_and(|pending| matches!(&pending.stage, Stage::Sent(sent) if sent.hash == txid))
    }

    pub(crate) fn hold(&self, address_id: i64, payment: Payment) {
        self.pending.write().insert(address_id, Pending { payment, stage: Stage::Built });
    }

    /// Keeps the signed form of the held, unsent `payment`.
    pub(crate) fn attach_signed(
        &self,
        payment: &Payment,
        signed: &SignedTx,
    ) -> Result<(), TonError> {
        let mut pending = self.pending.write();
        let entry = pending
            .values_mut()
            .find(|entry| entry.payment == *payment)
            .ok_or(TonError::Invalid("Build the TON payment again before signing."))?;
        if entry.is_sent() {
            return Err(TonError::Busy);
        }
        entry.stage = Stage::Signed(SignedBoc {
            hash: signed.txid.value().to_owned(),
            boc: STANDARD.encode(signed.raw.as_bytes()),
        });
        Ok(())
    }

    /// Marks the held send that `tx` signs as sent, and stores it before returning the
    /// BOC to send, so a failed write keeps it from ever leaving.
    pub(crate) async fn mark_sent(
        &self,
        tx: &SignedTx,
        clock: &dyn Clock,
    ) -> Result<String, TonError> {
        self.ensure_readable()?;
        let invalid = TonError::Invalid("The signed TON payment is not valid.");
        if tx.chain() != CHAIN {
            return Err(invalid);
        }
        let (address_id, payment, signed) = self
            .pending
            .read()
            .iter()
            .find_map(|(address_id, entry)| {
                let signed = entry.signed().filter(|signed| signed.hash == tx.txid.value())?;
                Some((*address_id, entry.payment.clone(), signed.clone()))
            })
            .ok_or(TonError::Invalid("Build the TON payment again before sending."))?;
        let boc = STANDARD.encode(tx.raw.as_bytes());
        if signed.boc != boc {
            return Err(invalid);
        }
        payment.ensure_unexpired(clock)?;
        payment.validate()?;
        validate_signed(&payment, &signed)?;
        let sent = Pending { payment, stage: Stage::Sent(signed) };
        self.persist(address_id, Some(&sent)).await?;
        self.pending.write().insert(address_id, sent);
        Ok(boc)
    }

    pub(crate) fn release_unsent(&self, payment: &Payment) {
        self.pending.write().retain(|_, entry| entry.is_sent() || entry.payment != *payment);
    }

    pub(crate) async fn persist(
        &self,
        address_id: i64,
        pending: Option<&Pending>,
    ) -> Result<(), TonError> {
        self.ensure_readable()?;
        let bytes = serde_json::to_vec(&Sends {
            version: SENDS_VERSION,
            pending: pending.map(StoredSend::from),
        })
        .map_err(|_| TonError::Internal("a TON send did not serialize"))?;
        self.cursors
            .reserve(
                CursorScope::Address(address_id),
                ReservationSlot::TonSends,
                CursorBytes::new(bytes),
            )
            .await?;
        Ok(())
    }

    /// Stores that `address_id` holds no send, then stops holding the one it did.
    pub(crate) async fn settle(&self, address_id: i64) -> Result<(), TonError> {
        self.persist(address_id, None).await?;
        self.pending.write().remove(&address_id);
        Ok(())
    }
}

fn scopes(address_ids: &[i64]) -> Vec<CursorScope> {
    address_ids.iter().copied().map(CursorScope::Address).collect()
}

/// The sends stored in `rows`. Fails unless every cursor in `scans` and every stored send
/// reads, and each send was broadcast with a BOC that matches its payment and hash.
fn restore(
    scans: &[ScanAdvance],
    rows: &[HeldReservation],
) -> Result<BTreeMap<i64, Pending>, TonError> {
    for row in scans {
        row.cursor.with_bytes(Cursor::decode)?;
    }
    let mut restored = BTreeMap::new();
    for row in rows {
        let address_id = row
            .scope
            .address_id()
            .ok_or(TonError::Internal("a stored TON send belongs to no address"))?;
        let wire: Sends = row
            .payload
            .with_bytes(|bytes| serde_json::from_slice(bytes))
            .map_err(|_| TonError::Internal("a stored TON send is not readable"))?;
        if wire.version != SENDS_VERSION {
            return Err(TonError::Internal("stored ton sends have an unsupported format"));
        }
        if let Some(stored) = wire.pending {
            if !stored.is_sent {
                return Err(TonError::Internal("a stored TON send was never broadcast"));
            }
            stored.payment.validate()?;
            let boc = stored.boc.ok_or(MISMATCH)?;
            let hash = stored.hash.ok_or(MISMATCH)?;
            let signed = SignedBoc { hash, boc };
            validate_signed(&stored.payment, &signed)?;
            restored.insert(
                address_id,
                Pending { payment: stored.payment, stage: Stage::Sent(signed) },
            );
        }
    }
    Ok(restored)
}
