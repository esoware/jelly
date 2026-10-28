//! Balance reads, pending-send lookups and each round of signature pages are
//! batched across the watch set.
//!
//! This wallet's sends are followed up before history is walked, and records are
//! emitted before cursors advance, so persisted progress never skips unreported history.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{
    Amount, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, WalletEvent,
};
use solana_pubkey::Pubkey;

use crate::broadcast::Attempt;
use crate::client::{Holdings, SolanaClient, Tip};
use crate::config::{CHAIN, NATIVE, SolanaConfig};
use crate::cursor::Walk;
use crate::error::SolanaError;
use crate::pending::{self, Tracked, Verdict};
use crate::rpc::SignatureRow;
use crate::state::States;
use crate::token::SolanaToken;
use crate::watched::Watched;
use crate::{broadcast, record};

const TARGET: &str = "jelly::solana::sync";

/// The depth below the tip within which no endpoint has pruned a transaction, about
/// ten minutes of slots. A healthy node trails the cluster by far less, and every node
/// keeps its ledger for far longer.
const UNPRUNED_SLOTS: u64 = 1_500;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PassInputs<'a> {
    pub(crate) client: &'a SolanaClient,
    pub(crate) config: &'a SolanaConfig,
    pub(crate) states: &'a States,
    pub(crate) clock: &'a dyn Clock,
}

pub(crate) async fn run(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<ChainSyncProgress, SolanaError> {
    inputs.states.persist_dirty().await?;
    let tip = inputs.client.tip().await?;
    if watched.is_empty() {
        return Ok(progress(inputs, watched, tip));
    }

    let holdings = emit_balances(inputs, watched, events).await?;
    events.try_emit(WalletEvent::SyncProgress(progress(inputs, watched, tip)));

    if !deadline.is_expired(inputs.clock) {
        follow_up(inputs, tip, events).await?;
    }
    if !deadline.is_expired(inputs.clock) {
        walk(inputs, watched, &holdings, tip, deadline, events).await?;
    }
    Ok(progress(inputs, watched, tip))
}

/// The share of addresses that have finished their first full history walk, because
/// a Solana history has no floor to measure against.
///
/// `synced_height` stays absent until every address has finished, because a wallet
/// still filling in history isn't synced to the tip.
fn progress(inputs: &PassInputs<'_>, watched: &[Watched], tip: Tip) -> ChainSyncProgress {
    let total = watched.len();
    let done = watched.iter().filter(|entry| inputs.states.is_complete(entry.address_id)).count();
    let synced = (done == total).then_some(tip.slot);
    ChainSyncProgress::new(CHAIN)
        .with_fraction(
            u64::try_from(done).unwrap_or(u64::MAX),
            u64::try_from(total).unwrap_or(u64::MAX),
        )
        .with_heights(synced, Some(tip.slot))
}

async fn emit_balances(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    events: &dyn EventSink,
) -> Result<Vec<Holdings>, SolanaError> {
    let owners: Vec<Pubkey> = watched.iter().map(|entry| entry.owner).collect();
    let holdings_by_owner = inputs.client.holdings(&owners).await?;
    for (entry, holdings) in watched.iter().zip(&holdings_by_owner) {
        events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: entry.address_id,
            asset: NATIVE,
            balance: Amount::from(holdings.lamports),
        }));
        for holding in &holdings.tokens {
            events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
                address_id: entry.address_id,
                asset: holding.token.asset(),
                balance: Amount::from(holding.balance.unwrap_or(0)),
            }));
        }
    }
    Ok(holdings_by_owner)
}

/// Settles, resends, or expires each broadcast send this wallet is still following.
///
/// A signed build that was never sent and whose blockhash has expired is dropped
/// without a write, because it was never persisted.
async fn follow_up(
    inputs: &PassInputs<'_>,
    tip: Tip,
    events: &dyn EventSink,
) -> Result<(), SolanaError> {
    let (sent, unsent): (Vec<Tracked>, Vec<Tracked>) =
        inputs.states.pending().into_iter().partition(|entry| entry.is_sent);
    for entry in unsent.iter().filter(|entry| tip.block_height > entry.expiry_height) {
        tracing::debug!(
            target: TARGET,
            signature = %entry.signature,
            "a built transaction was never sent and its blockhash has aged out"
        );
        inputs.states.forget_unsent(|held| held.signature == entry.signature);
    }

    let mut touched: Vec<i64> = Vec::new();
    if !sent.is_empty() {
        let signatures: Vec<&str> = sent.iter().map(|entry| entry.signature.as_str()).collect();
        let readings = inputs.client.statuses(&signatures).await?;
        for (entry, reading) in sent.iter().zip(readings) {
            let status = reading.status.as_ref();
            let verdict = pending::verdict(status, reading.block_height, entry.expiry_height);
            if let Some(address_id) = settle(inputs, entry, verdict, tip, events).await {
                touched.push(address_id);
            }
            if let Some(status) = status.filter(|status| status.err.is_some()) {
                tracing::info!(
                    target: TARGET,
                    signature = %entry.signature,
                    reason = broadcast::refusal_of(status.err.as_ref()).as_str(),
                    "a transaction this wallet sent landed and failed"
                );
            }
        }
    }

    touched.sort_unstable();
    touched.dedup();
    for address_id in touched {
        inputs.states.persist_sends(address_id).await?;
    }
    Ok(())
}

/// Acts on one verdict, and returns the address whose state changed.
async fn settle(
    inputs: &PassInputs<'_>,
    entry: &Tracked,
    verdict: Verdict,
    tip: Tip,
    events: &dyn EventSink,
) -> Option<i64> {
    match verdict {
        // A block only one leader has processed can still be skipped, so the send stays
        // pending until a supermajority votes for it.
        Verdict::Landed { is_settled: false, .. } => None,
        Verdict::Landed { slot, is_failed, is_settled: true } => {
            events
                .emit(WalletEvent::TxDetected(entry.landed_record(slot, tip.slot, is_failed)))
                .await;
            if is_failed && let Some(record) = entry.failed_token_record(slot, tip.slot) {
                events.emit(WalletEvent::TxDetected(record)).await;
            }
            inputs.states.forget(&entry.signature)
        }
        Verdict::InFlight => {
            resend(inputs, entry).await;
            None
        }
        Verdict::Expired => {
            let seen_at = match inputs.clock.now_secs() {
                Ok(secs) => Some(secs),
                Err(error) => {
                    tracing::warn!(
                        target: TARGET,
                        %error,
                        signature = %entry.signature,
                        "the system clock is unreadable, so this record is left for the store to \
                         time"
                    );
                    None
                }
            };
            events.emit(WalletEvent::TxDetected(entry.expired_record(seen_at))).await;
            tracing::info!(
                target: TARGET,
                signature = %entry.signature,
                "a transaction this wallet sent expired without landing"
            );
            inputs.states.forget(&entry.signature)
        }
    }
}

/// Resends the identical bytes with preflight skipped, because a node that has seen
/// the transaction would refuse one that's doing fine.
///
/// A failure is only logged, because a resend is a best effort on top of the node's
/// own retries.
async fn resend(inputs: &PassInputs<'_>, entry: &Tracked) {
    let Ok(raw) = BASE64.decode(&entry.raw) else {
        tracing::warn!(
            target: TARGET,
            signature = %entry.signature,
            "a tracked send's bytes are unreadable and cannot be resent"
        );
        return;
    };
    match broadcast::deliver(inputs.client, &raw, &entry.signature, Attempt::Rebroadcast).await {
        Ok(_) => tracing::debug!(
            target: TARGET,
            signature = %entry.signature,
            "resent a transaction that has not landed yet"
        ),
        Err(error) => tracing::debug!(
            target: TARGET,
            signature = %entry.signature,
            %error,
            "a resend was not accepted; the next pass tries again"
        ),
    }
}

/// One account whose signature history a pass walks.
struct Target<'a> {
    entry: &'a Watched,
    /// `None` for the owner's own history, else the token whose associated
    /// account this is.
    token: Option<SolanaToken>,
    account: String,
}

/// Each owner, followed by the associated account of every carried token the owner
/// holds or whose history hasn't been read through yet.
///
/// A node lists a signature only under the accounts its transaction names, so token
/// accounts are walked beside the owner. An account the owner doesn't hold is skipped
/// once its history is read, because the instruction that creates it names the owner,
/// and the first pass that finds it held walks forward from where its cursor stopped.
fn targets<'a>(
    inputs: &PassInputs<'_>,
    watched: &'a [Watched],
    holdings: &[Holdings],
) -> Vec<Target<'a>> {
    let mut targets = Vec::new();
    for (entry, holdings) in watched.iter().zip(holdings) {
        for (token, account) in entry.accounts() {
            let should_walk = token.is_none_or(|token| {
                let is_held = holdings
                    .tokens
                    .iter()
                    .any(|holding| holding.token == token && holding.balance.is_some());
                is_held || !inputs.states.scan(entry.address_id, Some(token)).is_complete()
            });
            if should_walk {
                targets.push(Target { entry, token, account: account.to_string() });
            }
        }
    }
    targets
}

/// Cursors move only over the rows whose transactions were turned into records.
async fn walk(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    holdings: &[Holdings],
    tip: Tip,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<(), SolanaError> {
    let targets = targets(inputs, watched, holdings);
    let mut walks: Vec<Walk> = targets
        .iter()
        .map(|target| {
            Walk::new(inputs.states.scan(target.entry.address_id, target.token), inputs.config)
        })
        .collect();

    loop {
        let mut active: Vec<&mut Walk> = Vec::new();
        let mut requests = Vec::new();
        for (target, walk) in targets.iter().zip(walks.iter_mut()) {
            if let Some(page) = walk.next_page() {
                requests.push((target.account.as_str(), page));
                active.push(walk);
            }
        }
        if requests.is_empty() {
            break;
        }
        let pages = inputs.client.signature_pages(&requests).await?;
        for (walk, rows) in active.into_iter().zip(pages) {
            walk.accept(rows);
        }
        events.try_emit(WalletEvent::SyncProgress(progress(inputs, watched, tip)));
        if deadline.is_expired(inputs.clock) {
            break;
        }
    }

    let found = interleave(&walks);
    let recorded = emit_records(inputs, watched, &found, tip, deadline, events).await?;

    // A cursor is written only when it moved, so a quiet pass costs no database
    // transaction. An address's targets are adjacent, so one write covers them.
    let mut moved: Vec<i64> = Vec::new();
    for (target, walk) in targets.iter().zip(walks) {
        let count =
            walk.rows().iter().take_while(|row| recorded.contains(row.signature.as_str())).count();
        let cursor = walk.finish(count);
        let address_id = target.entry.address_id;
        if inputs.states.scan(address_id, target.token) != cursor {
            inputs.states.set_scan(address_id, target.token, cursor);
            moved.push(address_id);
        }
    }
    moved.dedup();
    for address_id in moved {
        inputs.states.persist_scan(address_id).await?;
    }
    Ok(())
}

/// Every row the walks found, each signature once, taking one row from each walk
/// in turn.
///
/// One transaction can be in two walks, and a pass that runs out of time still
/// moves every walk rather than spending itself on one long history.
fn interleave(walks: &[Walk]) -> Vec<SignatureRow> {
    let deepest = walks.iter().map(|walk| walk.rows().len()).max().unwrap_or(0);
    let mut seen: HashSet<&str> = HashSet::new();
    let mut found: Vec<SignatureRow> = Vec::new();
    for depth in 0..deepest {
        for walk in walks {
            if let Some(row) = walk.rows().get(depth)
                && seen.insert(row.signature.as_str())
            {
                found.push(row.clone());
            }
        }
    }
    found
}

/// One fetched transaction serves the whole watch set, so a transfer between two
/// of this wallet's addresses is fetched once and reported from both sides.
///
/// Stops fetching at the deadline and returns the signatures dealt with. A transaction
/// the endpoint doesn't have counts as dealt with only once it's old enough to have
/// been pruned. A newer one just isn't rooted on that node yet, which
/// happens when a load-balanced pool lists a signature on one node and fetches it from
/// another, so it's left for the next pass.
///
/// A transaction newer than the version cap fails the pass, so no cursor moves past
/// history this build can't read.
async fn emit_records<'a>(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    rows: &'a [SignatureRow],
    tip: Tip,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<HashSet<&'a str>, SolanaError> {
    let mut dealt_with = HashSet::with_capacity(rows.len());
    for row in rows {
        if deadline.is_expired(inputs.clock) {
            break;
        }
        let signature = row.signature.as_str();
        let Some(transaction) = inputs.client.transaction(signature).await? else {
            if tip.slot.saturating_sub(row.slot) <= UNPRUNED_SLOTS {
                tracing::debug!(
                    target: TARGET,
                    %signature,
                    "the endpoint has not rooted this transaction yet; the next pass asks again"
                );
            } else {
                tracing::debug!(
                    target: TARGET,
                    %signature,
                    "the endpoint no longer keeps this transaction, so it cannot be recorded"
                );
                dealt_with.insert(signature);
            }
            continue;
        };
        for entry in watched {
            for record in record::of_transaction(
                &transaction,
                signature,
                &entry.address,
                entry.address_id,
                tip.slot,
            ) {
                events.emit(WalletEvent::TxDetected(record)).await;
            }
        }
        dealt_with.insert(signature);
    }
    Ok(dealt_with)
}
