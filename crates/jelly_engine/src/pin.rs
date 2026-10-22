use std::collections::HashMap;
use std::sync::{Arc, Weak};

use jelly_core::{ChainId, CoreError, PinStore, SpkiPin};
use jelly_store::{PinRow, Store, StoreError};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::engine::Inner;

const TARGET: &str = "jelly::engine::pins";

/// How many pin writes may be in flight before one is dropped. A host's key is
/// written when it is first seen and again only if it changes, so the queue only
/// ever holds a burst.
const QUEUE_DEPTH: usize = 64;

/// Trust on first use for endpoint TLS keys, answered from memory.
///
/// [`PinStore`] is called from inside a certificate verifier mid-handshake, so neither
/// method may block or touch the disk. Reads come from the map, and writes go to
/// [`persist`] through a bounded queue. A lost write costs the pin its persistence but
/// not its effect, because the rest of the session compares against the map.
#[derive(Debug)]
pub(crate) struct PinCache {
    /// Keyed by chain first, so a lookup borrows the host rather than allocating a
    /// key for it.
    pins: RwLock<HashMap<ChainId, HashMap<String, SpkiPin>>>,
    writes: mpsc::Sender<PinRow>,
}

impl PinStore for PinCache {
    fn get(&self, chain: ChainId, host: &str) -> Result<Option<SpkiPin>, CoreError> {
        Ok(self.pins.read().get(&chain).and_then(|hosts| hosts.get(host)).copied())
    }

    fn put(&self, chain: ChainId, host: &str, pin: SpkiPin) -> Result<(), CoreError> {
        self.pins.write().entry(chain).or_default().insert(host.to_owned(), pin);
        let row = PinRow { chain, host: host.to_owned(), pin };
        if self.writes.try_send(row).is_err() {
            tracing::warn!(
                target: TARGET,
                chain = chain.as_str(),
                "a tls pin was not queued for the database"
            );
        }
        Ok(())
    }
}

/// Reads every recorded pin and hands back the store the verifier uses together
/// with the queue that [`persist`] drains.
pub(crate) async fn load(
    store: &Store,
) -> Result<(Arc<PinCache>, mpsc::Receiver<PinRow>), StoreError> {
    let rows = store.list_pins().await?;
    let mut pins: HashMap<ChainId, HashMap<String, SpkiPin>> = HashMap::new();
    for row in rows {
        pins.entry(row.chain).or_default().insert(row.host, row.pin);
    }
    let (writes, incoming) = mpsc::channel(QUEUE_DEPTH);
    Ok((Arc::new(PinCache { pins: RwLock::new(pins), writes }), incoming))
}

/// Drains queued pins into the database until the engine is cancelled.
///
/// Cancellation closes the queue, and the writer still stores what was already in it,
/// because the store stays open until the tracker has drained.
///
/// Holds the engine weakly, so the writer never keeps alive an engine that was
/// dropped without a shutdown.
pub(crate) async fn persist(
    weak: Weak<Inner>,
    mut incoming: mpsc::Receiver<PinRow>,
    cancel: CancellationToken,
) {
    loop {
        let queued = tokio::select! {
            biased;
            queued = incoming.recv() => queued,
            () = cancel.cancelled() => {
                incoming.close();
                continue;
            }
        };
        let Some(row) = queued else {
            return;
        };
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if let Err(error) = inner.store.put_pin(row).await {
            tracing::warn!(target: TARGET, %error, "a tls pin was not stored");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tokio::time::sleep;

    use super::*;
    use crate::WalletEngine;
    use crate::fixtures::deps;
    use crate::test_util::config;

    fn pins() -> (PinCache, mpsc::Receiver<PinRow>) {
        let (writes, incoming) = mpsc::channel(QUEUE_DEPTH);
        (PinCache { pins: RwLock::new(HashMap::new()), writes }, incoming)
    }

    async fn engine_on(directory: &Path) -> WalletEngine {
        WalletEngine::start(config(), deps(directory)).await.expect("the engine starts").engine
    }

    #[test]
    fn a_pin_is_answered_per_chain_and_host() {
        let (store, _incoming) = pins();
        let pin = SpkiPin::new([7u8; 32], 1);
        store.put(ChainId::Bitcoin, "one.example", pin).expect("the map accepts a pin");

        assert_eq!(store.get(ChainId::Bitcoin, "one.example").expect("a read"), Some(pin));
        assert_eq!(store.get(ChainId::Bitcoin, "two.example").expect("a read"), None);
        assert_eq!(store.get(ChainId::Litecoin, "one.example").expect("a read"), None);
    }

    #[test]
    fn a_recorded_pin_is_queued_once() {
        let (store, mut incoming) = pins();
        let pin = SpkiPin::new([1u8; 32], 2);
        store.put(ChainId::Monero, "node.example", pin).expect("the map accepts a pin");

        let row = incoming.try_recv().expect("the write was queued");
        assert_eq!(row.chain, ChainId::Monero);
        assert_eq!(row.host, "node.example");
        assert_eq!(row.pin, pin);
        assert!(incoming.try_recv().is_err(), "one pin is one write");
    }

    #[test]
    fn a_full_queue_still_records_the_pin_in_memory() {
        let (store, _incoming) = pins();
        let pin = SpkiPin::new([2u8; 32], 3);
        for n in 0..QUEUE_DEPTH.saturating_add(4) {
            let host = format!("host-{n}.example");
            store.put(ChainId::Ethereum, &host, pin).expect("a full queue is not a failure");
        }
        let last = format!("host-{}.example", QUEUE_DEPTH.saturating_add(3));
        assert_eq!(store.get(ChainId::Ethereum, &last).expect("a read"), Some(pin));
    }

    #[tokio::test]
    async fn a_recorded_pin_is_answered_again_by_the_next_engine() {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let pin = SpkiPin::new([3u8; 32], 4);

        let engine = engine_on(directory.path()).await;
        engine.pin_store().put(ChainId::Bitcoin, "node.example", pin).expect("the map accepts it");
        // The writer stores the pin in the background and signals nothing, so the test
        // reads until the row lands, however long the write takes.
        let stored = loop {
            let stored = engine.inner.store.list_pins().await.expect("a read");
            if !stored.is_empty() {
                break stored;
            }
            sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            stored,
            vec![PinRow { chain: ChainId::Bitcoin, host: "node.example".to_owned(), pin }]
        );
        engine.shutdown().await.expect("the engine shuts down");

        let engine = engine_on(directory.path()).await;
        assert_eq!(
            engine.pin_store().get(ChainId::Bitcoin, "node.example").expect("a read"),
            Some(pin)
        );
        engine.shutdown().await.expect("the engine shuts down");
    }

    #[tokio::test]
    async fn a_pin_still_queued_when_the_engine_is_cancelled_is_stored() {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let engine = engine_on(directory.path()).await;
        let (writes, incoming) = mpsc::channel(QUEUE_DEPTH);
        let row = PinRow {
            chain: ChainId::Bitcoin,
            host: "node.example".to_owned(),
            pin: SpkiPin::new([5u8; 32], 6),
        };
        writes.try_send(row.clone()).expect("the queue has room");
        let cancel = CancellationToken::new();
        cancel.cancel();

        persist(Arc::downgrade(&engine.inner), incoming, cancel).await;
        assert_eq!(engine.inner.store.list_pins().await.expect("a read"), vec![row]);
        engine.shutdown().await.expect("the engine shuts down");
    }
}
