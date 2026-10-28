#[cfg(any(test, feature = "test-util"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use jelly_core::{EventSink, ScanAdvance, TxRecord, WalletEvent};
use jelly_store::TxWrite;
use tokio::sync::{broadcast, mpsc};

/// Events and cursor advances share one FIFO queue, so progress can't overtake the rows
/// that justify it.
///
/// Every advance carries the pump's loss count from when its scope was read. A secret
/// advance also carries its wallet ID, so its write is refused after a session switch.
#[derive(Debug)]
pub(crate) enum Inbound {
    Event(WalletEvent),
    Advance { losses_seen: u64, advance: ScanAdvance },
    SecretAdvance { wallet_id: i64, losses_seen: u64, advance: ScanAdvance },
}

impl Inbound {
    /// An advance from a scope read before the pump last dropped rows may be past rows
    /// that never landed, so it must not commit.
    pub(crate) const fn is_stale(&self, losses: u64) -> bool {
        match *self {
            Self::Event(_) => false,
            Self::Advance { losses_seen, .. } | Self::SecretAdvance { losses_seen, .. } => {
                losses_seen < losses
            }
        }
    }
}

/// A bounded sink producers push into and a broadcast the interface subscribes to, with
/// the pump between them. Some events go straight to the broadcast.
///
/// A subscriber that falls behind loses old events instead of stalling a sync pass.
/// That's safe because every event is only a refresh hint and the next pass reports
/// state again.
#[derive(Debug)]
pub(crate) struct Events {
    sender: mpsc::Sender<Inbound>,
    outbound: broadcast::Sender<WalletEvent>,
    losses: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    dropped: Arc<AtomicU64>,
}

impl Events {
    pub(crate) fn new(capacity: usize) -> (Self, mpsc::Receiver<Inbound>) {
        let (sender, receiver) = mpsc::channel(capacity);
        let (outbound, _) = broadcast::channel(capacity);
        (
            Self {
                sender,
                outbound,
                losses: AtomicU64::new(0),
                #[cfg(any(test, feature = "test-util"))]
                dropped: Arc::new(AtomicU64::new(0)),
            },
            receiver,
        )
    }

    /// How many times the pump has dropped transaction rows it couldn't persist.
    ///
    /// A cursor port notes it when it reads a scope. The pump refuses any advance noted
    /// before the latest loss, since the adapter may have moved past the lost rows. A
    /// scope read after the loss starts from the committed position and finds them again.
    pub(crate) fn losses(&self) -> u64 {
        self.losses.load(Ordering::SeqCst)
    }

    pub(crate) fn record_loss(&self) {
        self.losses.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn sink(&self) -> EngineSink {
        EngineSink {
            sender: self.sender.clone(),
            #[cfg(any(test, feature = "test-util"))]
            dropped: Arc::clone(&self.dropped),
        }
    }

    /// Queues one scan's progress behind everything already emitted. Waits for room
    /// instead of dropping, because losing the cursor would cost a rescan.
    pub(crate) async fn queue(&self, item: Inbound) {
        // An error means the pump has stopped for shutdown.
        let _ = self.sender.send(item).await;
    }

    /// Straight to the interface, past the pump's coalescing.
    pub(crate) fn publish(&self, event: WalletEvent) {
        let _ = self.outbound.send(event);
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<WalletEvent> {
        self.outbound.subscribe()
    }

    /// How many events producers dropped because the sink was full. Only `try_emit`
    /// drops, because `emit` applies backpressure.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }
}

/// The bounded sink every adapter pushes into.
#[derive(Debug)]
pub(crate) struct EngineSink {
    sender: mpsc::Sender<Inbound>,
    #[cfg(any(test, feature = "test-util"))]
    dropped: Arc<AtomicU64>,
}

#[async_trait]
impl EventSink for EngineSink {
    async fn emit(&self, event: WalletEvent) {
        // An error means the pump has stopped for shutdown.
        let _ = self.sender.send(Inbound::Event(event)).await;
    }

    fn try_emit(&self, event: WalletEvent) {
        let outcome = self.sender.try_send(Inbound::Event(event));
        #[cfg(any(test, feature = "test-util"))]
        if outcome.is_err() {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
        drop(outcome);
    }
}

/// What a stored record is announced as. Only a row the store didn't hold before is
/// news.
///
/// The store answers once per record, so `None` can't happen. It falls back to the
/// quieter `TxUpdated`.
pub(crate) const fn announcement(record: TxRecord, write: Option<TxWrite>) -> WalletEvent {
    match write {
        Some(TxWrite::Inserted) => WalletEvent::TxDetected(record),
        Some(TxWrite::Updated) | None => WalletEvent::TxUpdated(record),
    }
}
