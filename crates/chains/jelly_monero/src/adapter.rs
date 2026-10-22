use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CursorStore, Deadline,
    EventSink, FeeQuote, KeyMaterial, RetryPolicy, SignedTx, Subaddresses, TxId, TxRequest,
    UnsignedTx, WatchedAddress, duration_millis, sole_key,
};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Mutex as AsyncMutex;

use crate::account::MoneroAccount;
use crate::broadcast::BroadcastReply;
use crate::config::MoneroConfig;
use crate::cursor::Blobs;
use crate::decoys::{Candidate, Distribution};
use crate::error::MoneroError;
use crate::fee::FeeLadder;
use crate::node::{MoneroNode, broadcast_through_ring};
use crate::params::{CHAIN, check_chain};
use crate::pending::InFlight;
use crate::sign::{Signed, parse_signed, sign};
use crate::state::{ScanState, timelock_now_secs};
use crate::subaddress::Subaddress;
use crate::sync::Pass;
use crate::watched::Watched;
use crate::{fee, send, sync};

const TARGET: &str = "jelly::monero::adapter";

/// Signatures remembered between `sign_tx` and `broadcast`. Bounded, because a
/// signature that is never broadcast has nothing to release and must not
/// accumulate.
const MAX_AWAITING_BROADCAST: usize = 8;

/// How long one node's fee ladder stays usable.
const FEE_TTL: Duration = Duration::from_secs(120);

/// How long one node's [`Distribution`] stays usable.
const DISTRIBUTION_TTL: Duration = Duration::from_secs(600);

/// Minimum growth when the subaddress window has to widen, so a run of new
/// subaddresses costs one rescan rather than one each.
const LOOKAHEAD_STEP: u32 = 50;

/// Monero adapter holding account keys for one unlocked session. Scanning needs the view
/// key, so the engine rebuilds this adapter on unlock and drops it on lock.
///
/// There is no fee replacement support. Builds reserve nothing, so `PendingTracker` is
/// absent. Broadcast persists input reservations before sending. Conflicting builds can
/// share inputs, but at most one can confirm.
pub struct MoneroAdapter {
    node: Arc<dyn MoneroNode>,
    account: Arc<MoneroAccount>,
    config: MoneroConfig,
    blobs: Blobs,
    clock: Arc<dyn Clock>,
    state: Mutex<ScanState>,
    watched: RwLock<Arc<Watched>>,
    /// Each signature's broadcast context, which is the node that chose its ring,
    /// the key images it consumes, and what its in-flight record needs.
    awaiting_broadcast: Mutex<VecDeque<Signed>>,
    /// One fee ladder per window, behind a lock held across the fetch so a burst
    /// of quotes costs one round trip.
    fees: AsyncMutex<Option<Cached<FeeLadder>>>,
    /// One output distribution per window, behind its own lock so a quote never
    /// waits behind its download. A build whose ring the node behind it could not
    /// serve drops it, because every ring is pinned to that node.
    distribution: AsyncMutex<Option<Cached<Arc<Distribution>>>>,
    /// The spendable balance each address id was last announced with, which only
    /// a pass reads or writes. Its lock is what makes `sync` single-flight per
    /// instance.
    announced: AsyncMutex<BTreeMap<i64, Amount>>,
}

#[derive(Debug)]
struct Cached<T> {
    value: T,
    at_millis: u64,
}

impl<T> Cached<T> {
    fn is_fresh(&self, now_millis: u64, ttl: Duration) -> bool {
        now_millis.saturating_sub(self.at_millis) < duration_millis(ttl)
    }
}

impl fmt::Debug for MoneroAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let guard = self.state.lock();
        f.debug_struct("MoneroAdapter")
            .field("account", &self.account)
            .field("watched", &self.watched.read().len())
            .field("in_flight", &guard.in_flight_count())
            .field("synced_height", &guard.synced_height())
            .field("tip", &guard.tip_height())
            .finish_non_exhaustive()
    }
}

pub struct MoneroAdapterBuilder {
    node: Arc<dyn MoneroNode>,
    account: MoneroAccount,
    config: MoneroConfig,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    restore_height: u64,
}

impl fmt::Debug for MoneroAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoneroAdapterBuilder")
            .field("account", &self.account)
            .field("restore_height", &self.restore_height)
            .finish_non_exhaustive()
    }
}

impl MoneroAdapterBuilder {
    #[cfg(feature = "test-util")]
    #[must_use]
    pub const fn config(mut self, config: MoneroConfig) -> Self {
        self.config = config;
        self
    }

    /// Restores the persisted scan state, so a balance is available the moment the
    /// wallet unlocks rather than after the first pass.
    pub async fn build(self) -> Result<MoneroAdapter, ChainError> {
        let blobs = Blobs::new(self.cursors);
        let state = blobs
            .restore(
                self.account.public_spend(),
                self.restore_height,
                (self.config.lookahead_major, self.config.lookahead_minor),
            )
            .await?;
        Ok(MoneroAdapter {
            node: self.node,
            account: Arc::new(self.account),
            config: self.config,
            blobs,
            clock: self.clock,
            state: Mutex::new(state),
            watched: RwLock::new(Arc::default()),
            awaiting_broadcast: Mutex::new(VecDeque::new()),
            fees: AsyncMutex::new(None),
            distribution: AsyncMutex::new(None),
            announced: AsyncMutex::new(BTreeMap::new()),
        })
    }
}

impl MoneroAdapter {
    /// There is no Monero adapter without account keys, a store, a clock, and a
    /// wallet birthday, so they are taken here.
    ///
    /// A `restore_height` above the true birthday silently misses funds, so a caller
    /// that does not know it passes zero.
    pub fn builder(
        node: Arc<dyn MoneroNode>,
        account: MoneroAccount,
        cursors: Arc<dyn CursorStore>,
        clock: Arc<dyn Clock>,
        restore_height: u64,
    ) -> MoneroAdapterBuilder {
        MoneroAdapterBuilder {
            node,
            account,
            config: MoneroConfig::default(),
            cursors,
            clock,
            restore_height,
        }
    }

    #[cfg(feature = "test-util")]
    pub(crate) const fn state(&self) -> &Mutex<ScanState> {
        &self.state
    }

    #[cfg(feature = "test-util")]
    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Widens the scanner's window to cover every index named, and says whether
    /// the window grew, which is the only thing that forces a rescan.
    fn widen_to(&self, wanted: impl IntoIterator<Item = Subaddress>) -> bool {
        let mut guard = self.state.lock();
        let mut has_widened = false;
        for subaddress in wanted {
            has_widened |= guard.widen(subaddress, LOOKAHEAD_STEP);
        }
        if has_widened {
            guard.reset_scan();
        }
        has_widened
    }

    /// The four fee tiers, from one round trip per window.
    async fn ladder(&self) -> Result<FeeLadder, MoneroError> {
        let mut guard = self.fees.lock().await;
        let now_millis = self.clock.elapsed_millis();
        if let Some(cached) = guard.as_ref()
            && cached.is_fresh(now_millis, FEE_TTL)
        {
            return Ok(cached.value.clone());
        }
        let ladder = self.node.fee_ladder().await?;
        *guard = Some(Cached { value: ladder.clone(), at_millis: now_millis });
        Ok(ladder)
    }

    /// The output distribution a ring for account `major` is chosen against,
    /// reused for a window as [`Distribution`] explains.
    ///
    /// A cached one is fetched again once the scan's tip has unlocked an output
    /// of the account that was still locked at its height, because a ring chosen
    /// there would hold a real input newer than every decoy it could draw.
    async fn distribution(&self, major: u32) -> Result<Arc<Distribution>, MoneroError> {
        let mut guard = self.distribution.lock().await;
        let now_millis = self.clock.elapsed_millis();
        if let Some(cached) = guard.as_ref()
            && cached.is_fresh(now_millis, DISTRIBUTION_TTL)
        {
            let has_unlocked_since = self.state.lock().has_unlocked_since(
                major,
                cached.value.height(),
                timelock_now_secs(self.clock.as_ref()),
            );
            if !has_unlocked_since {
                return Ok(Arc::clone(&cached.value));
            }
        }
        let height = self.node.tip_height().await?;
        let fetched = Arc::new(self.node.distribution(height).await?);
        *guard = Some(Cached { value: Arc::clone(&fetched), at_millis: now_millis });
        Ok(fetched)
    }

    /// Drops `failed` from the cache, unless a newer distribution has replaced
    /// it, so the next build fetches one rather than going back to a node that
    /// could not serve its ring.
    async fn forget_distribution(&self, failed: &Arc<Distribution>) {
        let mut guard = self.distribution.lock().await;
        if guard.as_ref().is_some_and(|cached| Arc::ptr_eq(&cached.value, failed)) {
            *guard = None;
        }
    }

    /// Gathers everything a build needs under one short guard.
    ///
    /// What is spendable is judged at `ring_height`, the block the ring is chosen
    /// against, because a real input still locked there would be newer than every
    /// decoy beside it.
    fn plan(
        &self,
        parsed: send::Request,
        ladder: &FeeLadder,
        ring_height: u64,
    ) -> Result<send::Plan, MoneroError> {
        let (priority, rate) = ladder.resolve(parsed.fee)?;
        let Subaddress { major, minor } = parsed.subaddress;
        let from = self.account.address(major, minor)?;
        let now = timelock_now_secs(self.clock.as_ref());
        let guard = self.state.lock();
        let spendable_outputs = guard.spendable_outputs(major, ring_height, now);
        let is_truncated = spendable_outputs.len() > self.config.inputs();
        let candidates = spendable_outputs
            .into_iter()
            .take(self.config.inputs())
            .map(|owned| {
                let key_image = owned.key_image();
                Candidate {
                    output: owned.output().clone(),
                    key_image,
                    ring: guard.rings.get(&key_image).cloned(),
                }
            })
            .collect();
        Ok(send::Plan {
            recipient: parsed.recipient,
            from,
            subaddress: parsed.subaddress,
            target_piconero: parsed.target_piconero,
            priority,
            rate,
            candidates,
            is_truncated,
            balance: guard.account_balance(major, ring_height, now),
            scan_gap: ring_height.max(guard.tip_height()).saturating_sub(guard.synced_height()),
        })
    }

    fn remember_signed(&self, signed: Signed) {
        let mut guard = self.awaiting_broadcast.lock();
        guard.retain(|known| known.txid != signed.txid);
        guard.push_back(signed);
        while guard.len() > MAX_AWAITING_BROADCAST {
            guard.pop_front();
        }
    }

    fn take_signed(&self, txid: &[u8; 32]) -> Option<Signed> {
        let mut guard = self.awaiting_broadcast.lock();
        let position = guard.iter().position(|known| known.txid == *txid)?;
        guard.remove(position)
    }

    /// Records the send in flight and makes it durable, answering with the node
    /// that chose its ring.
    ///
    /// This is the last moment refusing is free. A crash between the send and the
    /// record would leave a payment in the air with its inputs unreserved, so a
    /// second send could pay twice.
    async fn reserve(&self, signed: Signed) -> Result<String, MoneroError> {
        let Signed {
            tx,
            txid,
            endpoint,
            key_images,
            build_height,
            amount_piconero,
            recipient,
            subaddress,
        } = signed;
        let ring_endpoint = endpoint.clone();
        {
            let mut guard = self.state.lock();
            // The budget counts blocks from the broadcast, so a scan tip left over
            // from before an unlock must not stand in for the chain's height.
            let sent_at_height = guard.tip_height().max(build_height);
            guard.track(InFlight {
                txid,
                key_images,
                raw: tx.raw.as_bytes().to_vec(),
                sent_at_height,
                amount_piconero,
                recipient,
                subaddress,
                endpoint,
            });
        }
        if let Err(error) = self.blobs.write_sends(&self.state).await {
            // Nothing has gone out yet, so the truthful state is one where this
            // transaction does not exist. Keeping the reservation would hold coins
            // for a payment nobody made.
            self.state.lock().forget(&txid);
            return Err(error);
        }
        Ok(ring_endpoint)
    }
}

#[async_trait]
impl ChainAdapter for MoneroAdapter {
    fn chain(&self) -> ChainId {
        CHAIN
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        let watched = Watched::from(addresses);
        // The state is widened before the watch set is replaced, and the locks are
        // never taken the other way round.
        let has_widened = self.widen_to(watched.subaddresses());
        *self.watched.write() = Arc::new(watched);
        if has_widened {
            tracing::info!(
                target: TARGET,
                "a monero subaddress fell outside the scanned window; rescanning"
            );
            self.blobs.write_scan(&self.state).await?;
        }
        Ok(())
    }

    /// Single-flight per instance, so a second call waits for the running pass
    /// rather than running alongside it.
    ///
    /// Every step checks the engine's deadline and stops with the progress it
    /// has. The hard cutoff is the engine's watchdog, because a timeout here
    /// would fire at the deadline itself and throw that progress away.
    async fn sync(
        &self,
        deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        let mut announced = self.announced.lock().await;
        let watched = Arc::clone(&self.watched.read());
        let pass = Pass {
            node: &self.node,
            account: &self.account,
            config: &self.config,
            state: &self.state,
            blobs: &self.blobs,
            clock: self.clock.as_ref(),
            events,
            deadline,
            watched: &watched,
        };
        Ok(sync::run(&pass, &mut announced).await?)
    }

    async fn quote_fee(&self, request: &TxRequest) -> Result<FeeQuote, ChainError> {
        let parsed = send::Request::parse(request, &self.watched.read())?;
        let ladder = self.ladder().await?;
        // The outputs a build would draw on, in the order it would draw on them,
        // judged at the scan's last tip rather than at the height a build's ring
        // is chosen against, so a quote never waits for a distribution.
        let amounts_piconero: Vec<u64> = {
            let guard = self.state.lock();
            guard
                .spendable_outputs(
                    parsed.subaddress.major,
                    guard.tip_height(),
                    timelock_now_secs(self.clock.as_ref()),
                )
                .iter()
                .map(|owned| owned.amount())
                .collect()
        };
        let (_, rate) = ladder.resolve(parsed.fee)?;
        Ok(fee::quote(&self.config, &ladder, rate, &amounts_piconero, parsed.target_piconero))
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let parsed = send::Request::parse(request, &self.watched.read())?;
        let ladder = self.ladder().await?;
        let distribution = self.distribution(parsed.subaddress.major).await?;
        let plan = self.plan(parsed, &ladder, distribution.height())?;
        let mut chosen = BTreeMap::new();
        let built = send::build(
            self.node.as_ref(),
            &self.config,
            &self.account,
            &plan,
            &distribution,
            &mut chosen,
        )
        .await;
        if let Err(error) = &built
            && error.is_ring_node_fault()
        {
            self.forget_distribution(&distribution).await;
        }
        // A ring is durable before the transaction carrying it leaves the adapter, so no
        // later build can offer the same output with another one.
        if self.state.lock().remember_rings(chosen) {
            self.blobs.write_sends(&self.state).await?;
        }
        Ok(built?)
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        let key = sole_key(keys)?;
        let tip = self.state.lock().tip_height();
        let signed = sign(&self.account, tx, key, tip, self.config.max_payload_age_blocks)?;
        let answer = signed.tx.clone();
        self.remember_signed(signed);
        Ok(answer)
    }

    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        check_chain(tx.chain())?;
        let transaction = parse_signed(tx.raw.as_bytes())?;
        let txid = transaction.hash();

        let endpoint = if let Some(signed) = self.take_signed(&txid) {
            self.reserve(signed).await?
        } else {
            self.state
                .lock()
                .in_flight_sends()
                .find(|send| send.txid == txid)
                .map(|send| send.endpoint.clone())
                .ok_or(MoneroError::NotSignedHere)?
        };

        match broadcast_through_ring(self.node.as_ref(), &endpoint, &transaction).await {
            Ok(BroadcastReply::Accepted) => Ok(TxId::new(CHAIN, hex::encode(txid))),
            Ok(BroadcastReply::Refused(rejection)) => {
                if rejection.retry == RetryPolicy::AlreadyLanded {
                    return Ok(TxId::new(CHAIN, hex::encode(txid)));
                }
                // A refusal that proves the transaction is in no mempool holds
                // nothing, so its inputs go back immediately rather than after
                // the budget, and the rebuild a refusal like this asks for can
                // use them.
                if matches!(rejection.retry, RetryPolicy::FinalEverywhere | RetryPolicy::DidNotLand)
                {
                    self.state.lock().forget(&txid);
                    if let Err(error) = self.blobs.write_sends(&self.state).await {
                        tracing::warn!(
                            target: TARGET,
                            %error,
                            "a refused monero transaction could not be released on disk"
                        );
                    }
                }
                Err(MoneroError::Rejected(rejection).into())
            }
            // The outcome is unknown and it may have landed, so the reservation stays.
            Err(error) => Err(error.into()),
        }
    }

    fn subaddresses(&self) -> Option<&dyn Subaddresses> {
        Some(self)
    }
}

#[async_trait]
impl Subaddresses for MoneroAdapter {
    async fn register_window(&self, major: u32, minor: u32) -> Result<(), ChainError> {
        if self.widen_to([Subaddress::new(major, minor)]) {
            self.blobs.write_scan(&self.state).await?;
        }
        Ok(())
    }
}
