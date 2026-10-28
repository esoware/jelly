//! Pending sends are followed up first, and then each address in turn.
//!
//! Each address reports its `/wallet` balances before its indexed history, so a
//! history failure cannot hide them. The balances also decide whether the
//! history is read at all, because they are read every pass anyway and the
//! indexer is the endpoint with the tight budget.

use std::collections::{BTreeSet, HashMap, HashSet};

use futures_util::future::join_all;
use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, WalletEvent,
};

use crate::broadcast::{self, BroadcastReply};
use crate::client::TronClient;
use crate::config::{CHAIN, TRX, TronConfig};
use crate::error::TronError;
use crate::gate::Gate;
use crate::pending::{self, Tracked, Verdict, millis_to_secs};
use crate::shape::BlockRef;
use crate::state::States;
use crate::token::TOKENS;
use crate::walk;
use crate::watched::Watched;

const TARGET: &str = "jelly::tron::sync";

#[derive(Debug, Clone, Copy)]
pub(crate) struct PassInputs<'a> {
    pub(crate) client: &'a TronClient,
    pub(crate) config: &'a TronConfig,
    pub(crate) states: &'a States,
    pub(crate) clock: &'a dyn Clock,
}

/// `gates` carries each address's [`Gate`] from one pass to the next, keyed by
/// address id.
pub(crate) async fn run(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    gates: &mut HashMap<i64, Gate>,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<ChainSyncProgress, TronError> {
    let tip = inputs.client.now_block().await?;
    gates.retain(|address_id, _| watched.iter().any(|entry| entry.address_id == *address_id));
    if watched.is_empty() {
        return Ok(progress(inputs, watched, tip));
    }

    let reported_sends = if deadline.is_expired(inputs.clock) {
        HashSet::new()
    } else {
        follow_up(inputs, tip, events).await?
    };

    for entry in watched {
        if deadline.is_expired(inputs.clock) {
            break;
        }
        let balances = balances(inputs, entry, events).await;
        if deadline.is_expired(inputs.clock) {
            break;
        }
        let gate = gates.entry(entry.address_id).or_default();
        gate.observe(&balances, inputs.clock);
        let has_own_send = reported_sends.contains(&entry.address_id);
        walk::run(inputs, entry, tip, deadline, events, gate, has_own_send).await;
        events.try_emit(WalletEvent::SyncProgress(progress(inputs, watched, tip)));
    }
    Ok(progress(inputs, watched, tip))
}

/// The share of addresses that have finished their first full walk of every
/// stream, because a TRON history has no floor to measure against.
///
/// `synced_height` stays absent until every address has finished, because a
/// wallet still filling in history is not synced to the tip.
fn progress(inputs: &PassInputs<'_>, watched: &[Watched], tip: BlockRef) -> ChainSyncProgress {
    let total = watched.len();
    let done = watched.iter().filter(|entry| inputs.states.is_complete(entry.address_id)).count();
    let synced = (done == total).then_some(tip.number);
    ChainSyncProgress::new(CHAIN)
        .with_fraction(
            u64::try_from(done).unwrap_or(u64::MAX),
            u64::try_from(total).unwrap_or(u64::MAX),
        )
        .with_heights(synced, Some(tip.number))
}

/// A missing account row reads as a zero balance, and a failed token read is
/// logged and skipped. If the account read fails, the token reads are skipped
/// rather than repeating failing requests.
///
/// Answers every balance an endpoint reported, in base units.
async fn balances(
    inputs: &PassInputs<'_>,
    entry: &Watched,
    events: &dyn EventSink,
) -> Vec<(AssetId, u128)> {
    let balance_sun = match inputs.client.account(&entry.address).await {
        Ok(account) => u128::from(account.balance_sun()),
        Err(error) => {
            tracing::warn!(
                target: TARGET,
                address_id = entry.address_id,
                %error,
                "no endpoint reported this address's TRX balance this pass"
            );
            return Vec::new();
        }
    };
    emit_balance(events, entry.address_id, TRX, balance_sun);
    let mut balances = vec![(TRX, balance_sun)];
    let reads = TOKENS.map(|token| inputs.client.token_balance(&entry.address, &entry.body, token));
    for (token, read) in TOKENS.into_iter().zip(join_all(reads).await) {
        match read {
            Ok(balance) => {
                emit_balance(events, entry.address_id, AssetId::Token(token), balance);
                balances.push((AssetId::Token(token), balance));
            }
            Err(error) => tracing::debug!(
                target: TARGET,
                address_id = entry.address_id,
                token = token.as_str(),
                %error,
                "no endpoint reported this token balance this pass"
            ),
        }
    }
    balances
}

fn emit_balance(events: &dyn EventSink, address_id: i64, asset: AssetId, balance: u128) {
    events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
        address_id,
        asset,
        balance: Amount::from_base_units(balance),
    }));
}

/// One receipt lookup per send, plus an existence lookup for a send with no
/// receipt, which separates one still in a mempool from a dead one.
///
/// A build no endpoint was handed is not asked about, because no node can know
/// it. It waits for a broadcast to claim it and is dropped without a record once
/// its window closes, because it never was a transaction.
///
/// Nothing is rebuilt here. A resend is the identical bytes, because a rebuild
/// would put two live transactions in the air.
///
/// Answers the addresses whose sends this pass reported landed or dead, which
/// is news for their history.
async fn follow_up(
    inputs: &PassInputs<'_>,
    tip: BlockRef,
    events: &dyn EventSink,
) -> Result<HashSet<i64>, TronError> {
    let mut reported_ids = HashSet::new();
    let sends = inputs.states.pending();
    if sends.is_empty() {
        return Ok(reported_ids);
    }
    let mut touched_ids = BTreeSet::new();
    for entry in sends {
        match follow_up_send(inputs, &entry, tip, events).await {
            Ok(FollowedUp::Waiting) => {}
            Ok(FollowedUp::Reported { forgotten }) => {
                reported_ids.insert(entry.address_id);
                touched_ids.extend(forgotten);
            }
            Err(error) => tracing::warn!(
                target: TARGET,
                txid = %entry.txid,
                %error,
                "a send this wallet made could not be followed up this pass"
            ),
        }
    }
    for address_id in touched_ids {
        inputs.states.persist_sends(address_id).await?;
    }
    Ok(reported_ids)
}

/// What following up one send did.
#[derive(Debug, Clone, Copy)]
enum FollowedUp {
    /// Nothing was reported, because the send was never handed to an endpoint
    /// or is still in flight.
    Waiting,
    /// The send was reported landed or dead. `forgotten` names the address whose
    /// followed sends changed, once the send is no longer followed.
    Reported { forgotten: Option<i64> },
}

/// Acts on one send.
///
/// Every time reported here comes from the head block's clock, the one the
/// network judged the transaction by, so a machine with an unusable clock still
/// files its own sends correctly.
async fn follow_up_send(
    inputs: &PassInputs<'_>,
    entry: &Tracked,
    tip: BlockRef,
    events: &dyn EventSink,
) -> Result<FollowedUp, TronError> {
    if !entry.is_sent {
        if pending::has_expired(&tip, entry.expiration_millis) {
            inputs.states.forget_unsent(&entry.txid);
        }
        return Ok(FollowedUp::Waiting);
    }
    let info = inputs.client.transaction_info(&entry.txid).await?;
    // The existence question is only asked when there is no receipt, which is the
    // only time the answer changes anything.
    let is_held_by_network = if info.height().is_some() {
        true
    } else {
        inputs.client.holds_transaction(&entry.txid).await?
    };
    match pending::verdict(&info, is_held_by_network, &tip, entry.expiration_millis) {
        Verdict::Landed { landed, is_settled } => {
            events.emit(WalletEvent::TxDetected(entry.landed_record(&landed, tip.number))).await;
            if !landed.result.is_success() {
                tracing::info!(
                    target: TARGET,
                    txid = %entry.txid,
                    reason = landed.result.as_str(),
                    "a transaction this wallet sent landed and failed"
                );
            }
            let forgotten = if is_settled { inputs.states.forget(&entry.txid) } else { None };
            Ok(FollowedUp::Reported { forgotten })
        }
        Verdict::InFlight => {
            resend(inputs, entry).await;
            Ok(FollowedUp::Waiting)
        }
        Verdict::Expired => {
            events
                .emit(WalletEvent::TxDetected(
                    entry.expired_record(millis_to_secs(tip.timestamp_millis)),
                ))
                .await;
            tracing::info!(
                target: TARGET,
                txid = %entry.txid,
                "a transaction this wallet sent expired without landing"
            );
            Ok(FollowedUp::Reported { forgotten: inputs.states.forget(&entry.txid) })
        }
    }
}

/// Resends the identical bytes, which is always safe because the network answers
/// `DUP_TRANSACTION_ERROR` for a transaction it already holds.
///
/// A failure is only logged, because a resend is a best effort on top of the
/// network's own relaying.
async fn resend(inputs: &PassInputs<'_>, entry: &Tracked) {
    let Ok(signed) = hex::decode(&entry.raw) else {
        tracing::warn!(
            target: TARGET,
            txid = %entry.txid,
            "a tracked send's bytes are unreadable and cannot be resent"
        );
        return;
    };
    match broadcast::deliver(inputs.client, &signed).await {
        Ok(BroadcastReply::Accepted { .. }) => tracing::debug!(
            target: TARGET,
            txid = %entry.txid,
            "resent a transaction that has not landed yet"
        ),
        Ok(BroadcastReply::Refused(rejection)) => tracing::debug!(
            target: TARGET,
            txid = %entry.txid,
            kind = rejection.kind.as_str(),
            "a node refused a resend; the next pass asks about the transaction again"
        ),
        Err(error) => tracing::debug!(
            target: TARGET,
            txid = %entry.txid,
            %error,
            "a resend reached no node; the next pass tries again"
        ),
    }
}
