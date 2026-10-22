use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use jelly_core::{
    CoreError, CursorBytes, CursorScope, CursorStore, HeldReservation, ReservationSlot,
    ScanAdvance, ScanStream,
};
use parking_lot::Mutex;

use crate::engine::{Inner, WalletEngine};
use crate::events::Inbound;
use crate::secret_cursor::{is_secret_scan, is_secret_slot, read_secrets, write_secret};

/// Routes secret cursors to the vault and other cursors to `SQLite`. Exhaustive matches
/// classify every stream and reservation slot.
///
/// `advance` queues progress behind emitted rows. `SQLite` progress commits with those
/// rows, and vault progress commits afterward. `reserve` writes through durably before
/// broadcast and must never be buffered.
///
/// Weak engine references avoid keeping retired sessions alive. Each port retains its
/// wallet ID to reject writes into a different session.
///
/// An advance carries the pump's loss count from the last read of its scope, or from
/// when the port was made for a scope it never read, so the pump can refuse one that
/// may have moved past rows it dropped.
#[derive(Debug)]
struct CursorRouter {
    wallet_id: i64,
    engine: Weak<Inner>,
    losses_at_creation: u64,
    losses_at_read: Mutex<BTreeMap<(CursorScope, ScanStream), u64>>,
}

impl CursorRouter {
    /// The engine, for as long as one call needs it.
    fn engine(&self) -> Result<Arc<Inner>, CoreError> {
        self.engine.upgrade().ok_or(CoreError::Locked)
    }
}

#[async_trait]
impl CursorStore for CursorRouter {
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        if scopes.is_empty() || streams.is_empty() {
            return Ok(Vec::new());
        }
        let inner = self.engine()?;
        let losses = inner.events.losses();
        let (secret, plain): (Vec<ScanStream>, Vec<ScanStream>) =
            streams.iter().copied().partition(|&stream| is_secret_scan(stream));

        let mut scans = Vec::new();
        if !secret.is_empty() {
            let wanted: Vec<(&'static str, ScanStream)> =
                secret.into_iter().map(|stream| (stream.as_str(), stream)).collect();
            let found = read_secrets(&inner, self.wallet_id, scopes, &wanted)?;
            scans.extend(found.into_iter().map(|(scope, stream, cursor)| ScanAdvance {
                scope,
                stream,
                cursor,
            }));
        }
        if !plain.is_empty() {
            scans.extend(inner.store.load_scans(scopes.to_vec(), plain).await?);
        }
        let mut losses_at_read = self.losses_at_read.lock();
        for &scope in scopes {
            for &stream in streams {
                losses_at_read.insert((scope, stream), losses);
            }
        }
        Ok(scans)
    }

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        if scopes.is_empty() || slots.is_empty() {
            return Ok(Vec::new());
        }
        let inner = self.engine()?;
        let (secret, plain): (Vec<ReservationSlot>, Vec<ReservationSlot>) =
            slots.iter().copied().partition(|&slot| is_secret_slot(slot));

        let mut reservations = Vec::new();
        if !secret.is_empty() {
            let wanted: Vec<(&'static str, ReservationSlot)> =
                secret.into_iter().map(|slot| (slot.as_str(), slot)).collect();
            let found = read_secrets(&inner, self.wallet_id, scopes, &wanted)?;
            reservations.extend(found.into_iter().map(|(scope, slot, payload)| HeldReservation {
                scope,
                slot,
                payload,
            }));
        }
        if !plain.is_empty() {
            reservations.extend(inner.store.load_reservations(scopes.to_vec(), plain).await?);
        }
        Ok(reservations)
    }

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        let inner = self.engine()?;
        let losses_seen = self
            .losses_at_read
            .lock()
            .get(&(scope, stream))
            .copied()
            .unwrap_or(self.losses_at_creation);
        let advance = ScanAdvance { scope, stream, cursor };
        let queued = if is_secret_scan(stream) {
            Inbound::SecretAdvance { wallet_id: self.wallet_id, losses_seen, advance }
        } else {
            Inbound::Advance { losses_seen, advance }
        };
        inner.events.queue(queued).await;
        Ok(())
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        let inner = self.engine()?;
        if is_secret_slot(slot) {
            return write_secret(&inner, self.wallet_id, scope, slot.as_str(), &payload).await;
        }
        Ok(inner.store.reserve(scope, slot, payload).await?)
    }
}

impl WalletEngine {
    /// Creates the adapter cursor port for one wallet. It weakly references the engine and
    /// resolves the session on every call. A stale port rejects a different open wallet
    /// instead of writing into its vault.
    pub(crate) fn cursors(&self, wallet_id: i64) -> Arc<dyn CursorStore> {
        Arc::new(CursorRouter {
            wallet_id,
            engine: Arc::downgrade(&self.inner),
            losses_at_creation: self.inner.events.losses(),
            losses_at_read: Mutex::new(BTreeMap::new()),
        })
    }
}
