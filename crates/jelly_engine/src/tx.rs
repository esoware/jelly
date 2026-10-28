use std::collections::BTreeSet;
use std::sync::Arc;

use jelly_core::{
    ChainAdapter, ChainError, ChainId, CoreError, FeeQuote, FeeSelection, PendingTxId, RetryPolicy,
    SignedTx, TxDetection, TxDirection, TxId, TxOutcome, TxPreview, TxRecord, TxRequest,
    UnsignedTx, random_bytes,
};
use jelly_store::{AddressRow, StoreError, SyncBatch, TxFilter, TxWrite};
use secrecy::SecretString;
use tokio::sync::{MutexGuard, OwnedMutexGuard};
use tokio::time::sleep;

use crate::engine::Inner;
use crate::error::task_error;
use crate::events::announcement;
use crate::pending::{PendingTx, PreparedTx, PreviewDigest};
use crate::session::Session;
use crate::{WalletEngine, derive};

impl WalletEngine {
    /// What a transfer would pay at its fee selection, straight from the adapter.
    pub async fn quote_fees(&self, request: TxRequest) -> Result<FeeQuote, CoreError> {
        let (session, _sending) = self.begin_send().await?;
        let (request, _sources) = self.resolve_request(&session, request).await?;
        let adapter = self.adapter(request.asset.chain())?;
        Ok(adapter.quote_fee(&request).await?)
    }

    /// Builds a transaction and authorizes it. The returned preview is exactly what will
    /// be signed, and its digest binds the two together.
    pub async fn prepare_tx(&self, request: TxRequest) -> Result<PreparedTx, CoreError> {
        let (session, _sending) = self.begin_send().await?;
        let (request, sources) = self.resolve_request(&session, request).await?;
        let chain = request.asset.chain();
        let adapter = self.adapter(chain)?;
        self.evict_stale().await;
        let unsigned = adapter.build_tx(&request).await?;
        let signers = signers_of(&unsigned.preview, &sources)?;
        self.hold(chain, signers, &adapter, unsigned)
    }

    /// What speeding up an unconfirmed transaction at `fee` would cost. Nothing is built
    /// and nothing is reserved.
    ///
    /// A fee level is raised to the network's floor for the bump, so
    /// [`WalletEngine::bump_fee`] never refuses it as too cheap.
    pub async fn quote_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<FeeQuote, CoreError> {
        let (_session, _sending) = self.begin_send().await?;
        let adapter = self.adapter(txid.chain())?;
        let bump = adapter.fee_bump().ok_or_else(no_replacement)?;
        Ok(bump.quote_bump(txid, fee).await?)
    }

    /// Plans a fee bump for an unconfirmed transaction, either a replacement or a child
    /// that pays for it, and authorizes it.
    ///
    /// Only the price is read from `fee`. The recipient, amount, and coins, nonce slot, or
    /// change come from the transaction being sped up, so nothing here can redirect a
    /// payment.
    pub async fn bump_fee(&self, txid: &TxId, fee: &FeeSelection) -> Result<PreparedTx, CoreError> {
        let (session, _sending) = self.begin_send().await?;
        self.require_sent_tx(session.wallet_id(), txid).await?;
        let adapter = self.adapter(txid.chain())?;
        let bump = adapter.fee_bump().ok_or_else(no_replacement)?;
        self.evict_stale().await;
        let unsigned = bump.build_bump(txid, fee).await?;
        self.hold_replacement(&session, txid.chain(), &adapter, unsigned).await
    }

    /// What canceling an unconfirmed transaction at `fee` would cost. Nothing is built and
    /// nothing is reserved.
    pub async fn quote_cancel(
        &self,
        txid: &TxId,
        fee: &FeeSelection,
    ) -> Result<FeeQuote, CoreError> {
        let (_session, _sending) = self.begin_send().await?;
        let adapter = self.adapter(txid.chain())?;
        let cancel = adapter.tx_cancel().ok_or_else(no_cancel)?;
        Ok(cancel.quote_cancel(txid, fee).await?)
    }

    /// Plans a replacement that moves nothing for an unconfirmed transaction and
    /// authorizes it, under the same rules as [`WalletEngine::bump_fee`]. Only the price
    /// is read from `fee`, and the sender and slot come from the transaction canceled.
    pub async fn cancel_tx(
        &self,
        txid: &TxId,
        fee: &FeeSelection,
    ) -> Result<PreparedTx, CoreError> {
        let (session, _sending) = self.begin_send().await?;
        self.require_sent_tx(session.wallet_id(), txid).await?;
        let adapter = self.adapter(txid.chain())?;
        let cancel = adapter.tx_cancel().ok_or_else(no_cancel)?;
        self.evict_stale().await;
        let unsigned = cancel.build_cancel(txid, fee).await?;
        self.hold_replacement(&session, txid.chain(), &adapter, unsigned).await
    }

    /// Keeps the authorization through the password check and signing, so those failures
    /// stay retryable, and consumes it immediately before broadcast.
    ///
    /// After a successful send, a local persistence failure is retried in the background,
    /// not reported as a failed payment.
    pub async fn submit_tx(
        &self,
        id: PendingTxId,
        password: SecretString,
        digest: PreviewDigest,
    ) -> Result<TxId, CoreError> {
        let session = self.require_session().await?;
        let sending = Arc::clone(&self.inner.sends).lock_owned().await;
        if session.cancel.is_cancelled() {
            return Err(CoreError::Locked);
        }
        self.authorized(id, digest).await?;
        session.verify_password(password).await?;

        // Password checking and signing can each outlast the authorization's TTL.
        let held = self.authorized(id, digest).await?;
        let adapter = self.adapter(held.chain())?;
        let (signed, _sending) =
            sign(Arc::clone(&session), Arc::clone(&adapter), held.signers, held.unsigned, sending)
                .await?;

        if session.cancel.is_cancelled() {
            return Err(CoreError::Locked);
        }
        // Read now, while a failure still leaves the authorization to retry. Nothing after
        // the broadcast may fail the send.
        let seen_unix_secs = self
            .inner
            .clock
            .now_secs()
            .map_err(|error| CoreError::internal("the system clock is unusable", error))?;
        // The guard still excludes publication, eviction, and other builds.
        let now_millis = self.inner.clock.elapsed_millis();
        let held = match self.inner.pending.take_spendable(id, now_millis, &adapter) {
            Ok(held) => held,
            Err(error) => {
                self.evict_stale().await;
                return Err(error);
            }
        };
        let txid = match adapter.broadcast(&signed).await {
            Ok(txid) => txid,
            Err(error) => {
                release_refused(adapter.as_ref(), &held.unsigned, &error).await;
                return Err(CoreError::Chain(error));
            }
        };
        let record = sent_record(&held, &txid, seen_unix_secs);
        // Persistence precedes the announcement here too, so a record the database
        // refused is announced only by the retry that lands it.
        match record_sent(&self.inner, record.clone()).await {
            Ok(write) => self.inner.events.publish(announcement(record, write)),
            Err(error) => {
                tracing::warn!(
                    target: "jelly::engine::tx",
                    %error,
                    "the broadcast succeeded but the local record failed"
                );
                self.retry_record(record);
            }
        }
        tracing::info!(
            target: "jelly::engine::tx",
            chain = held.chain().as_str(),
            "transaction broadcast"
        );
        Ok(txid)
    }

    /// Drops an authorization without signing it, releasing whatever the adapter
    /// reserved for it.
    pub async fn delete_pending_tx(&self, id: PendingTxId) -> Result<(), CoreError> {
        let (_session, _sending) = self.begin_send().await?;
        let pending = self.inner.pending.take(id)?;
        release(&pending).await;
        Ok(())
    }

    /// Authorizations still awaiting a submit, stale ones already released.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) async fn list_pending_txs(&self) -> Result<Vec<PreparedTx>, CoreError> {
        let (_session, _sending) = self.begin_send().await?;
        self.evict_stale().await;
        Ok(self.inner.pending.views())
    }

    /// The open session and the send guard, refusing a session a lock canceled while
    /// this waited for the guard.
    async fn begin_send(&self) -> Result<(Arc<Session>, MutexGuard<'_, ()>), CoreError> {
        let session = self.require_session().await?;
        let sending = self.inner.sends.lock().await;
        if session.cancel.is_cancelled() {
            return Err(CoreError::Locked);
        }
        Ok((session, sending))
    }

    /// The authorization for `id`, refusing one that has outlived the pending TTL or its
    /// adapter, and a digest that is not the one it was issued with.
    async fn authorized(
        &self,
        id: PendingTxId,
        digest: PreviewDigest,
    ) -> Result<PendingTx, CoreError> {
        self.evict_stale().await;
        let pending = self.inner.pending.get(id)?;
        if pending.digest == digest { Ok(pending) } else { Err(CoreError::RebuildRequired) }
    }

    /// Authorizes a replacement of one of the wallet's sends, signed by whichever of the
    /// chain's addresses its preview names.
    async fn hold_replacement(
        &self,
        session: &Arc<Session>,
        chain: ChainId,
        adapter: &Arc<dyn ChainAdapter>,
        unsigned: UnsignedTx,
    ) -> Result<PreparedTx, CoreError> {
        let rows = self.inner.store.list_addresses(session.wallet_id(), Some(chain)).await?;
        let signers = signers_of(&unsigned.preview, &rows)?;
        // Verified like a fresh send's sources, since these rows' keys sign the
        // replacement.
        let signers = derive::verify_rows(Arc::clone(session), signers).await?;
        self.hold(chain, signers, adapter, unsigned)
    }

    /// Authorizes `unsigned`, which `adapter` built on `chain` from `signers`, the rows of
    /// its preview's sources in order.
    ///
    /// The preview's chain is what the digest covers and what picks the adapter that signs
    /// and broadcasts, so a preview that names a different chain from its sources is
    /// refused.
    fn hold(
        &self,
        chain: ChainId,
        signers: Vec<AddressRow>,
        adapter: &Arc<dyn ChainAdapter>,
        unsigned: UnsignedTx,
    ) -> Result<PreparedTx, CoreError> {
        if unsigned.chain() != chain || signers.iter().any(|row| row.chain != chain) {
            return Err(CoreError::internal_without_source(
                "the adapter built a transaction for another chain",
            ));
        }
        let fee_warning = self.inner.table.spec(chain).high_fee_warning(unsigned.preview.fee);
        let now_millis = self.inner.clock.elapsed_millis();
        let id = PendingTxId::from_bytes(*random_bytes::<{ PendingTxId::LEN }>()?);
        let pending = PendingTx::new(signers, unsigned, fee_warning, adapter, now_millis)?;
        let view = pending.view(id);
        self.inner.pending.insert(id, pending);
        Ok(view)
    }

    /// Releases every authorization that has run out or whose adapter a rebuild replaced.
    /// The replacement never saw the old reservations and could hand the same coins or
    /// nonce to another send, so such an authorization mustn't stay submittable.
    ///
    /// Telling the adapter keeps an abandoned preview from holding an outpoint lock or a
    /// nonce slot for the rest of the session.
    async fn evict_stale(&self) {
        let now_millis = self.inner.clock.elapsed_millis();
        let adapters = self.inner.chains.adapters();
        for pending in self.inner.pending.stale(now_millis, &adapters) {
            release(&pending).await;
        }
    }

    /// Retries a record the store refused after a successful broadcast, on the engine's
    /// tracker so shutdown waits for it.
    ///
    /// The attempt that lands announces it. That's the only event the interface gets for
    /// this transaction until a sync pass finds it.
    fn retry_record(&self, record: TxRecord) {
        let weak = Arc::downgrade(&self.inner);
        let cancel = self.inner.cancel.clone();
        let delay = self.inner.config.retry_delay;
        let attempts = self.inner.config.attempts().saturating_sub(1);
        self.inner.tasks.spawn(async move {
            for _ in 0..attempts {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = sleep(delay) => {}
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                match record_sent(&inner, record.clone()).await {
                    Ok(write) => {
                        inner.events.publish(announcement(record, write));
                        return;
                    }
                    Err(error) => tracing::warn!(
                        target: "jelly::engine::tx",
                        %error,
                        "a broadcast transaction is still not recorded locally"
                    ),
                }
            }
            tracing::error!(
                target: "jelly::engine::tx",
                "a broadcast transaction could not be recorded; the next sync pass has to find it"
            );
        });
    }

    /// Resolves the sending addresses to the rows the store holds and verifies each, so a
    /// command can't smuggle in a foreign address or a different key. A UTXO chain pools
    /// several addresses, and every other chain sends from exactly one.
    ///
    /// The request comes back with its recipient as the chain's payment target. TON keeps
    /// the validated payment URI because its comment is part of the signed transfer, and
    /// other chains get the canonical address.
    async fn resolve_request(
        &self,
        session: &Arc<Session>,
        mut request: TxRequest,
    ) -> Result<(TxRequest, Vec<AddressRow>), CoreError> {
        let ids = &request.from_address_ids;
        if ids.is_empty() {
            return Err(CoreError::invalid_input(SOURCES, "Choose an address to send from."));
        }
        let mut named = BTreeSet::new();
        if !ids.iter().all(|id| named.insert(id)) {
            return Err(CoreError::invalid_input(SOURCES, "Choose each address once."));
        }
        let mut rows = Vec::with_capacity(ids.len());
        for &address_id in ids {
            rows.push(self.require_row(session.wallet_id(), address_id).await?);
        }
        let chain = request.asset.chain();
        if rows.iter().any(|row| row.chain != chain) {
            return Err(CoreError::invalid_input("asset", "Choose an asset on this chain."));
        }
        if !chain.is_utxo() && rows.len() != 1 {
            return Err(CoreError::invalid_input(SOURCES, "Choose one address to send from."));
        }
        let rows = derive::verify_rows(Arc::clone(session), rows).await?;
        let spec = self.inner.table.spec(chain);
        if request.destination_tag.is_some() && !spec.has_destination_tags {
            return Err(CoreError::invalid_input(
                "destinationTag",
                "Only XRP payments use a destination tag.",
            ));
        }
        if request.comment.is_some() && !spec.has_comments {
            return Err(CoreError::invalid_input("comment", "Only TON payments use a comment."));
        }
        if request.should_delete_account && !spec.can_delete_account {
            return Err(CoreError::invalid_input(
                "shouldDeleteAccount",
                "Only an XRP account can be deleted.",
            ));
        }
        if request.nonce.is_some() && !spec.can_pin_nonce {
            return Err(CoreError::invalid_input("nonce", "Only an EVM send takes a nonce."));
        }
        if request.data.is_some() && !spec.can_carry_data {
            return Err(CoreError::invalid_input("data", "Only an EVM send carries data."));
        }
        request.to = self.inner.table.payment_target(chain, &request.to)?;
        Ok((request, rows))
    }

    /// Refuses `txid` unless the wallet holds an outgoing or self-transfer row of it,
    /// which proves one of its addresses spent in it. An incoming row isn't enough, since
    /// a send to another of the wallet's own addresses has one for an address whose key
    /// signed nothing.
    async fn require_sent_tx(&self, wallet_id: i64, txid: &TxId) -> Result<(), CoreError> {
        for direction in [TxDirection::Outgoing, TxDirection::SelfTransfer] {
            let filter = TxFilter {
                chain: Some(txid.chain()),
                txid: Some(txid.value().to_owned()),
                direction: Some(direction),
                ..TxFilter::new(wallet_id)
            };
            let page = self.inner.store.list_txs(filter).await?;
            if !page.rows.is_empty() {
                return Ok(());
            }
        }
        Err(CoreError::not_found("transaction", txid.value()))
    }
}

/// The field a request's sending addresses arrive in.
const SOURCES: &str = "fromAddressIds";

/// The rows of `preview`'s sources, in its order, each one of `candidates`. An adapter
/// that names an address the request didn't offer is refused, so it can't sign with a
/// key the user didn't choose.
fn signers_of(
    preview: &TxPreview,
    candidates: &[AddressRow],
) -> Result<Vec<AddressRow>, CoreError> {
    if preview.from.is_empty() {
        return Err(CoreError::internal_without_source(
            "the adapter built a transaction from no address",
        ));
    }
    preview
        .from
        .iter()
        .map(|address| {
            candidates.iter().find(|row| row.address == *address).cloned().ok_or(
                CoreError::internal_without_source(
                    "the adapter built a transaction from an address the request did not name",
                ),
            )
        })
        .collect()
}

/// The row a broadcast transaction leaves behind, taken from the preview the user
/// approved, not from the request.
///
/// It's filed under the first source, which the transaction always spends from, so the
/// sync pass that sees the transaction rewrites it with that address's own share.
fn sent_record(pending: &PendingTx, txid: &TxId, seen_unix_secs: i64) -> TxRecord {
    let preview = pending.preview();
    let direction = if preview.from.contains(&preview.to) {
        TxDirection::SelfTransfer
    } else {
        TxDirection::Outgoing
    };
    let mut record = TxRecord::builder(
        preview.asset,
        pending.record_address_id,
        txid.clone(),
        direction,
        preview.amount,
    )
    .fee(preview.fee)
    .counterparty(preview.to.clone())
    .outcome(TxOutcome::Unknown)
    .detection(TxDetection::Full)
    .seen_at(seen_unix_secs);
    if let Some(rent) = preview.rent {
        record = record.rent(rent);
    }
    record.build()
}

/// Files a broadcast transaction's record and returns what the store did with it.
async fn record_sent(inner: &Inner, record: TxRecord) -> Result<Option<TxWrite>, StoreError> {
    let batch = SyncBatch { txs: vec![record], ..SyncBatch::default() };
    Ok(inner.store.apply_sync_batch(batch).await?.into_iter().next())
}

/// Only the adapter that built a transaction holds its reservations. Asking another
/// one could free coins or a nonce it reserved for a different send.
async fn release(pending: &PendingTx) {
    let Some(adapter) = pending.adapter.upgrade() else {
        return;
    };
    let Some(tracker) = adapter.pending_tracker() else {
        return;
    };
    if let Err(error) = tracker.discard(&pending.unsigned).await {
        tracing::warn!(
            target: "jelly::engine::tx",
            %error,
            "the adapter could not release a discarded transaction"
        );
    }
}

/// Releases a refused transaction's reservations, but only when the refusal proves it's
/// in no mempool anywhere.
///
/// Anything transport-ambiguous keeps them, because the bytes may have landed and a
/// released outpoint or nonce is how one payment becomes two.
async fn release_refused(adapter: &dyn ChainAdapter, unsigned: &UnsignedTx, error: &ChainError) {
    let is_definitive = matches!(
        *error,
        ChainError::Rejected(ref rejection)
            if matches!(rejection.retry, RetryPolicy::DidNotLand | RetryPolicy::FinalEverywhere)
    );
    if !is_definitive {
        return;
    }
    let Some(tracker) = adapter.pending_tracker() else {
        return;
    };
    if let Err(error) = tracker.discard(unsigned).await {
        tracing::warn!(
            target: "jelly::engine::tx",
            %error,
            "the adapter could not release a refused transaction"
        );
    }
}

/// Signs on the blocking pool.
///
/// Key material is resolved inside the task, so it never exists on a runtime worker,
/// and an expensive signer such as Monero's can take as long as it needs without
/// stalling one.
async fn sign(
    session: Arc<Session>,
    adapter: Arc<dyn ChainAdapter>,
    signers: Vec<AddressRow>,
    unsigned: UnsignedTx,
    sending: OwnedMutexGuard<()>,
) -> Result<(SignedTx, OwnedMutexGuard<()>), CoreError> {
    let tasks = session.tasks.clone();
    tasks
        .spawn_blocking(move || {
            let keys = signers
                .iter()
                .map(|row| derive::resolve_key(&session, row))
                .collect::<Result<Vec<_>, _>>()?;
            let signed = adapter.sign_tx(&unsigned, &keys).map_err(CoreError::Chain)?;
            // The task holds the send guard, so even if the caller is dropped, nothing can
            // retire the adapter or release its nonce until the signer is done with its
            // reservations.
            Ok((signed, sending))
        })
        .await
        .map_err(task_error)?
}

const fn no_replacement() -> CoreError {
    CoreError::Unsupported("this chain cannot replace a transaction")
}

const fn no_cancel() -> CoreError {
    CoreError::Unsupported("this chain cannot cancel a transaction")
}
