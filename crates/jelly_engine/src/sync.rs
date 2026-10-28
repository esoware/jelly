use std::sync::{Arc, Weak};
use std::time::Duration;

use jelly_core::{
    Address, ChainAdapter, ChainId, ChainSyncProgress, CoreError, KeyKind, TxConfirmation, TxId,
    WalletEvent, WatchedAddress, confirmations, duration_millis, random_bytes,
};
use jelly_keypair::Ed25519Key;
use jelly_store::{AddressRow, ChainStatePatch, Patch, TxFilter};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::chains::ChainSlot;
use crate::derive;
use crate::engine::Inner;
use crate::error::blocking;
use crate::session::Session;

/// Ceiling for the backoff after consecutive failing passes.
const SYNC_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// One chain's sync loop.
///
/// Exactly one runs per chain per supervision token (`Chains::supervision`), so a pass
/// never overlaps itself. It wakes on a kick, a push from
/// [`ChainAdapter::wait_activity`], the poll interval, or cancellation. For an adapter
/// that listens for pushes, the interval is only a safety net.
#[derive(Debug)]
pub(crate) struct ChainLoop {
    pub(crate) chain: ChainId,
    pub(crate) adapter: Arc<dyn ChainAdapter>,
    pub(crate) slot: Arc<ChainSlot>,
    pub(crate) supervision: CancellationToken,
    pub(crate) poll: Duration,
}

impl ChainLoop {
    pub(crate) async fn run(self, weak: Weak<Inner>) {
        let mut failures: u32 = 0;
        loop {
            if self.supervision.is_cancelled() {
                break;
            }
            let Some(inner) = weak.upgrade() else {
                break;
            };
            let Some(outcome) = self.supervision.run_until_cancelled(self.pass(&inner)).await
            else {
                break;
            };
            let wait = match outcome {
                Ok(()) => {
                    failures = 0;
                    self.poll
                }
                // The session is gone.
                Err(CoreError::Locked) => break,
                Err(ref error) => {
                    tracing::warn!(
                        target: "jelly::engine::sync",
                        chain = self.chain.as_str(),
                        %error,
                        "sync pass failed"
                    );
                    failures = failures.saturating_add(1);
                    backoff(self.poll, failures)
                }
            };
            drop(inner);
            #[cfg(any(test, feature = "test-util"))]
            self.slot.passes.finished(outcome.err().map(|error| {
                if let CoreError::Internal { context, .. } = error {
                    context.to_owned()
                } else {
                    error.to_string()
                }
            }));

            tokio::select! {
                biased;
                () = self.supervision.cancelled() => break,
                () = self.slot.kicked() => {}
                () = self.adapter.wait_activity() => {}
                () = sleep(wait) => {}
            }
        }
    }

    /// Refreshes the watch set, runs sync, and publishes the result. The adapter's
    /// deadline is shorter than the watchdog, so an adapter that honors it can return
    /// partial progress.
    ///
    /// Canceling the supervision token drops the pass at an await. Committed batches
    /// stay atomic, and uncommitted work resumes from the last cursor.
    async fn pass(&self, inner: &Inner) -> Result<(), CoreError> {
        let chain = self.chain;
        let session = inner.session.current().ok_or(CoreError::Locked)?;
        let rows = inner.store.list_addresses(session.wallet_id(), Some(chain)).await?;
        if rows.is_empty() {
            return Ok(());
        }
        self.adapter.set_watch_set(&watch_set(Arc::clone(&session), rows).await?).await?;

        let sink = inner.events.sink();
        let deadline = inner.clock.deadline_in(inner.config.pass_budget);
        let pass = timeout(inner.config.pass_watchdog(), self.adapter.sync(deadline, &sink));
        match pass.await {
            Ok(Ok(progress)) => {
                inner.chains.status.record_success(chain, progress);
                persist_height(inner, session.wallet_id(), progress).await;
                // The terminal state goes straight out, because routing it through the pump
                // would land it after the status this pass just recorded.
                inner.events.publish(WalletEvent::SyncProgress(progress));
                if let Some(tip) = progress.tip_height() {
                    let prior_tip = self.slot.announce_tip(tip);
                    announce_confirmations(inner, session.wallet_id(), chain, prior_tip, tip).await;
                }
                Ok(())
            }
            Ok(Err(error)) => Err(CoreError::Chain(error)),
            Err(_) => Err(CoreError::internal_without_source(
                "the adapter did not return inside its budget",
            )),
        }
    }
}

/// Builds the watch set on the blocking pool, since each ed25519 row's public key costs a
/// SLIP-10 derivation or a vault read.
async fn watch_set(
    session: Arc<Session>,
    rows: Vec<AddressRow>,
) -> Result<Vec<WatchedAddress>, CoreError> {
    blocking(move || {
        rows.into_iter()
            .map(|row| {
                let is_ed25519 = session.entry(row.chain).spec().key_kind == KeyKind::Ed25519Seed;
                let pubkey = if is_ed25519 {
                    let key = derive::resolve_key(&session, &row)?;
                    Some(Ed25519Key::from_key_material(&key)?.public_bytes())
                } else {
                    None
                };
                Ok(WatchedAddress {
                    id: row.id,
                    address: Address::assume_canonical(row.chain, row.address),
                    source: row.source,
                    pubkey,
                })
            })
            .collect::<Result<_, CoreError>>()
    })
    .await
}

/// Publishes new confirmation depths when the tip moves, since history that didn't change
/// would otherwise leave the interface's counts stale.
///
/// Depths up to one block past settled are announced so the move to settled shows. So is
/// every row that was unsettled at the prior tip, because a tip that jumps many blocks,
/// as on the first pass after an unlock, can carry a row straight past settled.
async fn announce_confirmations(
    inner: &Inner,
    wallet_id: i64,
    chain: ChainId,
    prior_tip: Option<u64>,
    tip: u64,
) {
    if prior_tip == Some(tip) {
        return;
    }
    let filter = TxFilter { chain: Some(chain), ..TxFilter::new(wallet_id) };
    let page = match inner.store.list_txs(filter).await {
        Ok(page) => page,
        Err(error) => {
            tracing::debug!(
                target: "jelly::engine::sync",
                chain = chain.as_str(),
                %error,
                "recent transactions could not be read for a confirmation refresh"
            );
            return;
        }
    };
    let settled = inner.table.spec(chain).settled_confirmations;
    let ceiling = u64::from(settled).saturating_add(1);
    let is_announceable = |height: Option<u64>, at: u64| {
        let depth = confirmations(height, at);
        depth > 0 && u64::from(depth) <= ceiling
    };
    for row in page.rows {
        let is_announceable_before =
            prior_tip.is_some_and(|prior| is_announceable(row.height, prior));
        if is_announceable_before || is_announceable(row.height, tip) {
            inner.events.publish(WalletEvent::TxConfirmed(TxConfirmation {
                txid: TxId::new(chain, row.txid),
                confirmations: confirmations(row.height, tip),
            }));
        }
    }
}

/// Records where the pass got to, so a freshly unlocked wallet can show the last
/// session's progress before the first pass of this one finishes.
async fn persist_height(inner: &Inner, wallet_id: i64, progress: ChainSyncProgress) {
    let Some(height) = progress.synced_height() else {
        return;
    };
    let patch = ChainStatePatch { synced_height: Patch::Set(height), ..ChainStatePatch::default() };
    if let Err(error) = inner.store.set_chain_state(wallet_id, progress.chain(), patch).await {
        tracing::warn!(
            target: "jelly::engine::sync",
            %error,
            "could not record the synced height"
        );
    }
}

/// The wallet's override for the chain, else the shipped default from its spec.
pub(crate) fn interval_for(default_secs: u32, wallet_override_secs: Option<u32>) -> Duration {
    Duration::from_secs(u64::from(wallet_override_secs.unwrap_or(default_secs)))
}

/// The wait before pass `failures + 1`, which is the base interval doubled per
/// consecutive failure, capped, then full-jittered.
fn backoff(base: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return base;
    }
    let shift = failures.min(16);
    let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    full_jitter(base.saturating_mul(factor).min(SYNC_BACKOFF_MAX))
}

/// A uniform draw from `0..=ceiling`. An entropy failure returns the full ceiling, the
/// conservative choice for a retry delay.
fn full_jitter(ceiling: Duration) -> Duration {
    let Ok(bytes) = random_bytes::<8>() else {
        return ceiling;
    };
    let millis = duration_millis(ceiling);
    let Some(span) = millis.checked_add(1) else {
        return ceiling;
    };
    let draw = u64::from_be_bytes(*bytes).checked_rem(span).unwrap_or(0);
    Duration::from_millis(draw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_never_exceeds_its_cap() {
        let base = Duration::from_secs(5);
        assert_eq!(backoff(base, 0), base);
        for failures in 1..=20 {
            assert!(backoff(base, failures) <= SYNC_BACKOFF_MAX);
        }
    }

    #[test]
    fn intervals_prefer_the_wallet_override() {
        assert_eq!(interval_for(30, Some(7)), Duration::from_secs(7));
        assert_eq!(interval_for(8, None), Duration::from_secs(8));
    }
}
