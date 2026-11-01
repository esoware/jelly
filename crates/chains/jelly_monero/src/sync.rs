use std::collections::BTreeMap;
use std::sync::Arc;

use jelly_core::{
    Amount, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, TxRecord, WalletEvent,
    duration_millis,
};
use monero_address::SubaddressIndex;
use monero_interface::ScannableBlock;
use monero_wallet::{ScanError, Scanner};
use parking_lot::Mutex;
use tokio::task::{JoinSet, spawn_blocking};

use crate::account::MoneroAccount;
use crate::balance::MoneroBalance;
use crate::config::MoneroConfig;
use crate::cursor::Blobs;
use crate::error::MoneroError;
use crate::node::{MoneroNode, height_of_index};
use crate::params::{ASSET, CHAIN, REORG_WINDOW};
use crate::pending::follow_up;
use crate::record::{Scanned, record_block, spends_in};
use crate::state::{BlockLink, BlockRef, ScanState, timelock_now_secs};
use crate::subaddress::Subaddress;
use crate::watched::Watched;

const TARGET: &str = "jelly::monero::sync";

/// How many times one pass rewinds and retries before it gives up locating a
/// reorganization and rescans from the wallet birthday.
///
/// The first rewind comes from a walk that re-read nothing, so only the ones after
/// it probe deeper.
const MAX_REWINDS: u32 = 5;

/// Minimum block advance between two progress events within a pass.
const PROGRESS_EVERY_BLOCKS: u64 = 200;

/// What one pass borrows from the adapter. Nothing here holds a state guard across
/// an await.
pub(crate) struct Pass<'a> {
    pub node: &'a Arc<dyn MoneroNode>,
    pub account: &'a Arc<MoneroAccount>,
    pub config: &'a MoneroConfig,
    pub state: &'a Mutex<ScanState>,
    pub blobs: &'a Blobs,
    pub clock: &'a dyn Clock,
    pub events: &'a dyn EventSink,
    pub deadline: Deadline,
    pub watched: &'a Watched,
}

/// One pass, bounded by the block budget and the deadline.
///
/// Blocks are scanned first, so the sends in flight are judged against the newest
/// view of the chain, and the balance events go out last, so the interface is told
/// what the pass finished with.
///
/// Balances are compared with `announced`, the spendable amount each address id was
/// last announced with, not with the state the pass started from. An unlock, a
/// reservation, or a failed pass's receipts can change a balance between passes,
/// where no comparison within one pass would see it.
pub(crate) async fn run(
    pass: &Pass<'_>,
    announced: &mut BTreeMap<i64, Amount>,
) -> Result<ChainSyncProgress, MoneroError> {
    let (restore_height, lookahead, generation) = {
        let state = pass.state.lock();
        (state.restore_height(), state.lookahead(), state.generation())
    };
    let committed =
        pass.blobs.restore_scan(pass.account.public_spend(), restore_height, lookahead).await?;
    let has_acknowledged = {
        let mut state = pass.state.lock();
        if state.generation() == generation {
            if state.next_height() != committed.next_height()
                || !state.recent_blocks().eq(committed.recent_blocks())
            {
                announced.clear();
            }
            state.replace_scan(committed)
        } else {
            false
        }
    };
    let tip = pass.node.tip_height().await?;
    pass.state.lock().set_tip_height(tip);

    scan_to_tip(pass, tip).await?;
    follow_up(pass, tip, has_acknowledged).await?;

    let (synced, balances) = {
        let guard = pass.state.lock();
        let now_secs = timelock_now_secs(pass.clock);
        (guard.synced_height(), guard.balances(tip, now_secs))
    };
    for update in balance_changes(pass.watched, announced, &balances) {
        pass.events.emit(WalletEvent::BalanceChanged(update)).await;
        announced.insert(update.address_id, update.balance);
    }
    pass.blobs.write_scan(pass.state).await?;
    Ok(progress_of(synced, tip))
}

/// Scans forward, rewinding and retrying when a batch disagrees with what was
/// already scanned.
///
/// The first walk reads only blocks not scanned yet, because one whose parent is
/// the last block scanned proves nothing under it changed. Once a fork shows, each
/// retry re-reads further back, starting at the configured overlap and doubling,
/// so a reorganization deeper than the first probe is still located. A fork at the
/// oldest block remembered, or forks still turning up after [`MAX_REWINDS`], may
/// reach below anything this wallet remembers, so the scan restarts from the
/// birthday.
async fn scan_to_tip(pass: &Pass<'_>, tip: u64) -> Result<(), MoneroError> {
    let first_probe = pass.config.reorg_overlap_blocks.clamp(1, REORG_WINDOW);
    let mut overlap = 0;
    let mut rewinds = 0u32;
    loop {
        let Some(fork) = scan_forward(pass, tip, overlap).await? else { return Ok(()) };
        rewinds = rewinds.saturating_add(1);
        let can_look_under = pass.state.lock().has_block_below(fork);
        if rewinds > MAX_REWINDS || !can_look_under {
            pass.state.lock().reset_scan();
            tracing::warn!(
                target: TARGET,
                fork,
                "a monero reorganization is deeper than this wallet remembers; rescanning"
            );
            pass.blobs.write_scan(pass.state).await?;
            return Ok(());
        }
        pass.state.lock().rewind_to(fork);
        tracing::info!(target: TARGET, fork, "a monero reorganization was located; rewinding");
        pass.blobs.write_scan(pass.state).await?;
        overlap = overlap.saturating_mul(2).clamp(first_probe, REORG_WINDOW);
    }
}

/// One forward walk. `Some(height)` is a fork located at that height, and nothing
/// from the batch that disagreed has been applied.
///
/// The batch prefetched ahead of the scan lives in a [`JoinSet`], so an error, a
/// fork, or a dropped pass aborts it.
async fn scan_forward(pass: &Pass<'_>, tip: u64, overlap: u64) -> Result<Option<u64>, MoneroError> {
    let (restore, next, lookahead, generation) = {
        let guard = pass.state.lock();
        (guard.restore_height(), guard.next_height(), guard.lookahead(), guard.generation())
    };
    // A tip below the last block scanned means a node that's behind or a chain
    // that got shorter. Only a block id tells the two apart, so read the node's
    // own tip block instead of undoing anything on its word.
    let from = if next > tip.saturating_add(1) { tip } else { next };
    // The overlap re-reads blocks already scanned, to find how deep a
    // reorganization goes in the same request that fetches new ones. The budget
    // counts from the frontier, so the overlap never eats into it.
    let start = from.saturating_sub(overlap).max(restore);
    if start > tip || pass.deadline.is_expired(pass.clock) {
        return Ok(None);
    }
    let budget_end = next.saturating_add(pass.config.pass_blocks().saturating_sub(1)).min(tip);

    let account = Arc::clone(pass.account);
    let mut scanner = spawn_blocking(move || scanner_for(&account, lookahead))
        .await
        .map_err(|_| MoneroError::ScanTask)?;
    let mut reported = next;
    let checkpoint_every_millis = duration_millis(pass.config.checkpoint_interval);
    let mut last_checkpoint_millis = pass.clock.elapsed_millis();
    let mut fetching = JoinSet::new();
    spawn_fetch(pass, &mut fetching, start, budget_end);

    while let Some(joined) = fetching.join_next().await {
        let fetched = joined.map_err(|_| MoneroError::ScanTask)?;
        let (mut blocks, asked_from) = fetched?;
        if blocks.is_empty() {
            break;
        }
        // The frontier may only move over blocks actually received, so the batch
        // is measured instead of assumed. Otherwise a node that answers short or
        // from elsewhere would leave unscanned blocks marked scanned.
        let links = links_of(&blocks, asked_from)?;
        let batch_end = links.last().map_or(asked_from, |link| link.height);
        let frontier = batch_end.saturating_add(1);
        if frontier <= budget_end && !pass.deadline.is_expired(pass.clock) {
            spawn_fetch(pass, &mut fetching, frontier, budget_end);
        }

        let diverged = pass.state.lock().fork_in(&links);
        if let Some(fork) = diverged {
            return Ok(Some(fork));
        }
        // Blocks below the frontier were read only to check them. They're already
        // in the state, and noting one again would forget the blocks remembered
        // above it.
        let checked_only = usize::try_from(next.saturating_sub(asked_from))
            .unwrap_or(usize::MAX)
            .min(blocks.len());
        blocks.drain(..checked_only);

        let account = Arc::clone(pass.account);
        let scanned = spawn_blocking(move || scan_blocks(scanner, &account, blocks))
            .await
            .map_err(|_| MoneroError::ScanTask)?;
        let (returned, scanned) = scanned?;
        scanner = returned;

        let Some(events) = apply(pass, scanned, batch_end, tip, generation) else {
            tracing::info!(
                target: TARGET,
                "the monero scan was reset during a pass; it starts again on the next one"
            );
            return Ok(None);
        };
        for event in events {
            pass.events.emit(event).await;
        }

        if frontier.saturating_sub(reported) >= PROGRESS_EVERY_BLOCKS || frontier > budget_end {
            reported = frontier;
            pass.events.try_emit(WalletEvent::SyncProgress(progress_of(batch_end, tip)));
        }

        let now_millis = pass.clock.elapsed_millis();
        if now_millis.saturating_sub(last_checkpoint_millis) >= checkpoint_every_millis {
            last_checkpoint_millis = now_millis;
            pass.blobs.write_scan(pass.state).await?;
        }
    }
    Ok(None)
}

/// Fetches one batch in its own task, answering with it and the height it was
/// asked to start at.
fn spawn_fetch(
    pass: &Pass<'_>,
    fetching: &mut JoinSet<Result<(Vec<ScannableBlock>, u64), MoneroError>>,
    from: u64,
    budget_end: u64,
) {
    let node = Arc::clone(pass.node);
    let to = from.saturating_add(pass.config.batch().saturating_sub(1)).min(budget_end);
    fetching.spawn(async move { node.blocks(from..=to).await.map(|blocks| (blocks, from)) });
}

/// A scanner registered with the whole subaddress window. The caller runs it on a
/// blocking thread, because each subaddress costs a point multiplication.
///
/// Subaddresses have to be registered before a block is scanned, so the window is a
/// wallet-wide setting instead of following the watch set. An address outside it
/// was invisible to every block already scanned.
fn scanner_for(account: &MoneroAccount, lookahead: (u32, u32)) -> Scanner {
    let mut scanner = Scanner::new(account.view_pair().clone());
    let (major, minor) = lookahead;
    for account_index in 0..major {
        for address_index in 0..minor {
            if let Some(index) = SubaddressIndex::new(account_index, address_index) {
                scanner.register_subaddress(index);
            }
        }
    }
    scanner
}

/// Scans a batch. The caller runs it on a blocking thread, because scanning is CPU
/// work.
///
/// A block past the hardfork ceiling becomes `ProtocolTooNew`, so the interface can
/// ask for an update.
fn scan_blocks(
    mut scanner: Scanner,
    account: &MoneroAccount,
    blocks: Vec<ScannableBlock>,
) -> Result<(Scanner, Vec<Scanned>), MoneroError> {
    let mut scanned = Vec::with_capacity(blocks.len());
    for block in blocks {
        let height = height_of_index(block.block.number())?;
        let hash = block.block.hash();
        let timestamp_secs = block.block.header.timestamp;
        let spends = spends_in(&block);
        let found = match scanner.scan(block) {
            Ok(found) => found.ignore_additional_timelock(),
            Err(ScanError::UnsupportedProtocol(version)) => {
                return Err(MoneroError::ProtocolTooNew { version, height });
            }
            Err(ScanError::InvalidScannableBlock(_)) => {
                return Err(MoneroError::UnscannableBlock { height });
            }
        };
        let mut outputs = Vec::with_capacity(found.len());
        for output in found {
            let image = account.key_image(&output);
            outputs.push((output, image));
        }
        scanned.push(Scanned { block: BlockRef { height, hash }, timestamp_secs, outputs, spends });
    }
    Ok((scanner, scanned))
}

/// Applies one scanned batch under a short guard and returns the events it owes.
///
/// `None` when the scan was reset after the walk read `generation`. The batch was
/// scanned for a frontier and a window that no longer exist, and applying it would
/// mark the cleared range scanned.
fn apply(
    pass: &Pass<'_>,
    scanned: Vec<Scanned>,
    batch_end: u64,
    tip: u64,
    generation: u64,
) -> Option<Vec<WalletEvent>> {
    let mut records: Vec<TxRecord> = Vec::new();
    let mut guard = pass.state.lock();
    if guard.generation() != generation {
        return None;
    }
    for block in scanned {
        records.extend(record_block(pass.watched, &mut guard, block, tip));
    }
    guard.advance_to(batch_end.saturating_add(1));
    drop(guard);
    Some(records.into_iter().map(WalletEvent::TxDetected).collect())
}

/// Each watched address whose spendable balance differs from its last announcement.
///
/// A subaddress left with no outputs after a reorganization or a rescan announces
/// zero like any other balance.
fn balance_changes(
    watched: &Watched,
    announced: &BTreeMap<i64, Amount>,
    balances: &BTreeMap<Subaddress, MoneroBalance>,
) -> Vec<BalanceUpdate> {
    let mut updates = Vec::new();
    for (subaddress, address_id) in watched.iter() {
        let balance = balances.get(&subaddress).map_or(Amount::ZERO, |balance| balance.spendable());
        if announced.get(&address_id) == Some(&balance) {
            continue;
        }
        updates.push(BalanceUpdate { address_id, asset: ASSET, balance });
    }
    updates
}

/// One batch as the chain check sees it, refusing anything but the consecutive run
/// starting at `from` that was asked for.
///
/// A shorter run than requested is allowed, because a node may have fewer blocks
/// than it claimed, and the next batch resumes where this one stopped.
fn links_of(blocks: &[ScannableBlock], from: u64) -> Result<Vec<BlockLink>, MoneroError> {
    let mut links = Vec::with_capacity(blocks.len());
    let mut expected = from;
    for block in blocks {
        let height = height_of_index(block.block.number())?;
        if height != expected {
            return Err(MoneroError::BlockRange { expected, found: height });
        }
        expected = height.saturating_add(1);
        links.push(BlockLink {
            height,
            hash: block.block.hash(),
            previous: block.block.header.previous,
        });
    }
    Ok(links)
}

/// A wallet that has scanned past the tip of a node that's behind claims no more
/// of the chain than that node has.
fn progress_of(synced: u64, tip: u64) -> ChainSyncProgress {
    ChainSyncProgress::new(CHAIN).with_heights(Some(synced.min(tip)), Some(tip))
}

#[cfg(test)]
mod tests {
    use monero_wallet::block::{Block, BlockHeader};
    use monero_wallet::transaction::{Input, Timelock, Transaction, TransactionPrefix};

    use super::*;
    use crate::fixtures::account;
    use crate::params::MAX_HARDFORK_VERSION;

    #[test]
    fn progress_reports_the_synced_height_no_higher_than_the_tip() {
        let full = progress_of(1_100, 1_100);
        assert_eq!(full.synced_height(), Some(1_100));
        assert_eq!(full.tip_height(), Some(1_100));

        let partial = progress_of(600, 1_100);
        assert_eq!(partial.synced_height(), Some(600));
        assert_eq!(partial.tip_height(), Some(1_100));

        let ahead = progress_of(1_200, 1_100);
        assert_eq!(ahead.synced_height(), Some(1_100), "a node that's behind caps the claim");
        assert_eq!(ahead.tip_height(), Some(1_100));
    }

    /// A block whose header names this hardfork version.
    fn block_at_hardfork(version: u8) -> ScannableBlock {
        let header = BlockHeader {
            hardfork_version: version,
            hardfork_signal: version,
            timestamp: 1_700_000_000,
            previous: [0; 32],
            nonce: 0,
        };
        let miner = Transaction::V2 {
            prefix: TransactionPrefix {
                additional_timelock: Timelock::None,
                inputs: vec![Input::Gen(1)],
                outputs: Vec::new(),
                extra: Vec::new(),
            },
            proofs: None,
        };
        ScannableBlock {
            block: Block::new(header, miner, Vec::new()).expect("a well-formed block"),
            transactions: Vec::new(),
            output_index_for_first_ringct_output: Some(0),
        }
    }

    /// The ceiling the error names is monero-wallet's own, which nothing else
    /// would notice going stale.
    #[test]
    fn the_scanner_refuses_exactly_the_hardforks_past_the_ceiling() {
        let account = account();
        let newest = scan_blocks(
            scanner_for(&account, (1, 1)),
            &account,
            vec![block_at_hardfork(MAX_HARDFORK_VERSION)],
        );
        assert!(newest.is_ok(), "the newest hardfork this build reads scans");

        let next = MAX_HARDFORK_VERSION.saturating_add(1);
        let refused =
            scan_blocks(scanner_for(&account, (1, 1)), &account, vec![block_at_hardfork(next)]);
        assert!(
            matches!(refused, Err(MoneroError::ProtocolTooNew { version, height: 1 }) if version == next),
            "the next one is refused by name"
        );
    }
}
