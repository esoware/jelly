use std::collections::BTreeMap;

use alloy::primitives::{Address, B256};
use jelly_core::WalletEvent;

use super::{PassInputs, TARGET};
use crate::abi;
use crate::cursor::{Frontier, ScanCursor, Window, WindowSize};
use crate::error::EvmError;
use crate::params::EvmParams;
use crate::record;
use crate::rpc::{LogEntry, LogFilter};
use crate::token::TokenTable;
use crate::watched::Watched;

/// How many times a window may halve inside one pass before the group is left
/// for the next one. Five halvings take a 1,000-block window to the default
/// 32-block floor.
const MAX_SHRINKS: u8 = 5;

/// Addresses that share a frontier, so one window covers them all. The cursor
/// carries only the frontiers.
#[derive(Debug, Clone)]
struct Group {
    cursor: ScanCursor,
    members: Vec<Watched>,
}

/// Caught-up addresses share one frontier, so usually there is one group. A new
/// address forms its own group until it catches up.
fn groups(watched: &[Watched], cursors: &BTreeMap<i64, ScanCursor>) -> Vec<Group> {
    let mut by_frontier: BTreeMap<(Option<u64>, Option<u64>), Vec<Watched>> = BTreeMap::new();
    for entry in watched {
        let cursor = cursors.get(&entry.address_id).copied().unwrap_or_default();
        by_frontier.entry((cursor.forward_to, cursor.backfill_from)).or_default().push(*entry);
    }
    by_frontier
        .into_iter()
        .map(|((forward_to, backfill_from), members)| Group {
            cursor: ScanCursor { forward_to, backfill_from, ..ScanCursor::default() },
            members,
        })
        .collect()
}

/// The watched addresses as senders, then as recipients, of every supported
/// token and, where the node logs them, of the native coin.
///
/// Each filter names every watched address, so a window costs two requests, or
/// four with native logs, whatever the wallet holds.
fn filters(
    params: &EvmParams,
    tokens: &TokenTable,
    owners: &[Watched],
    window: Window,
) -> Vec<LogFilter> {
    let owner_topics: Vec<B256> = owners.iter().map(|entry| entry.owner.into_word()).collect();
    let filter = |address: Vec<Address>, event: B256, owner_slot: usize| {
        let mut topics = vec![Some(vec![event])];
        topics.resize(owner_slot, None);
        topics.push(Some(owner_topics.clone()));
        LogFilter { from_block: window.from, to_block: window.to, address, topics }
    };
    let contracts = tokens.contracts();
    let mut filters = vec![
        filter(contracts.clone(), abi::TRANSFER_TOPIC, 1),
        filter(contracts, abi::TRANSFER_TOPIC, 2),
    ];
    if params.has_native_transfer_logs {
        filters.push(filter(vec![abi::BOR_NATIVE_TOKEN], abi::NATIVE_TRANSFER_TOPIC, 2));
        filters.push(filter(vec![abi::BOR_NATIVE_TOKEN], abi::NATIVE_TRANSFER_TOPIC, 3));
    }
    filters
}

/// Returns the window width the next pass should start at.
///
/// Where the node logs only token transfers, native activity without an indexer
/// comes from the balance. A group no endpoint serves even at the narrowest window
/// is left for the next pass, and its balances are still read.
pub(super) async fn log_history(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    cursors: &mut BTreeMap<i64, ScanCursor>,
    tip: u64,
    floor: u64,
    mut size: WindowSize,
) -> WindowSize {
    let params = inputs.params;
    let native = params.has_native_transfer_logs.then(|| params.native());
    for group in groups(watched, cursors) {
        let mut cursor = group.cursor;
        let mut forward_left = inputs.config.max_forward_windows;
        let mut backfill_left = inputs.config.max_backfill_windows;
        let mut shrinks_left = MAX_SHRINKS;
        let mut is_clean = true;
        // Only the first forward window overlaps, because a reorg can only have
        // moved the blocks the previous pass ended on.
        let mut overlap = inputs.params.rescan_overlap_blocks();

        loop {
            if inputs.is_out_of_time() {
                is_clean = false;
                break;
            }
            let forward = (forward_left > 0)
                .then(|| cursor.next_forward(tip, size.blocks(), overlap))
                .flatten();
            let Some(window) = forward.or_else(|| {
                (backfill_left > 0).then(|| cursor.next_backfill(floor, size.blocks())).flatten()
            }) else {
                break;
            };

            match read_window(inputs, &group.members, window).await {
                Ok(logs) => {
                    tracing::debug!(
                        target: TARGET,
                        from = window.from,
                        to = window.to,
                        blocks = window.blocks(),
                        found = logs.len(),
                        addresses = group.members.len(),
                        "scanned a log window"
                    );
                    for record in record::of_logs(inputs.tokens, native, &group.members, &logs, tip)
                    {
                        inputs.events.emit(WalletEvent::TxDetected(record)).await;
                    }
                    cursor.apply(window);
                    match window.frontier {
                        Frontier::Forward => {
                            overlap = 0;
                            forward_left = forward_left.saturating_sub(1);
                        }
                        Frontier::Backfill => backfill_left = backfill_left.saturating_sub(1),
                    }
                }
                Err(error) if error.is_window_too_wide() => {
                    is_clean = false;
                    if size.is_at_floor() || shrinks_left == 0 {
                        // Endpoint messages don't reliably say what width they
                        // refused, so the window is logged here.
                        tracing::warn!(
                            target: TARGET,
                            %error,
                            blocks = size.blocks(),
                            from = window.from,
                            to = window.to,
                            "no endpoint will serve a log window this narrow either"
                        );
                        break;
                    }
                    size = size.shrink();
                    shrinks_left = shrinks_left.saturating_sub(1);
                }
                Err(error) => {
                    tracing::warn!(
                        target: TARGET,
                        %error,
                        "a log window failed; the balances of this group still stand"
                    );
                    is_clean = false;
                    break;
                }
            }
        }

        for entry in &group.members {
            cursors.entry(entry.address_id).or_default().take_frontiers(cursor);
        }
        if is_clean {
            size = size.grow();
        }
    }
    size
}

async fn read_window(
    inputs: &PassInputs<'_>,
    owners: &[Watched],
    window: Window,
) -> Result<Vec<LogEntry>, EvmError> {
    let mut logs = Vec::new();
    for filter in filters(inputs.params, inputs.tokens, owners, window) {
        logs.extend(inputs.client.logs(&filter).await?);
    }
    Ok(logs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{token_table, watched};
    use crate::test_util::{ETHEREUM, POLYGON};

    #[test]
    fn one_window_asks_both_sides_over_every_contract_and_every_address() {
        let members = vec![watched(1, 1), watched(2, 2)];
        let window = Window { from: 10, to: 20, frontier: Frontier::Forward };
        let built = filters(&ETHEREUM, &token_table(), &members, window);
        let [sending, receiving] = built.as_slice() else {
            panic!("a network without native logs asks two filters: {built:?}")
        };
        assert_eq!(
            sending.address.len(),
            ETHEREUM.tokens.len(),
            "every token contract in one filter"
        );
        assert_eq!(sending.topics.len(), 2, "the sender side needs no hole");
        assert_eq!(receiving.topics.len(), 3, "the recipient side skips the sender slot");
        assert_eq!(receiving.topics.get(1), Some(&None));
        assert_eq!(
            sending.topics.get(1).and_then(Option::as_ref).map(Vec::len),
            Some(2),
            "every watched address rides in one topic array"
        );
        assert_eq!((sending.from_block, sending.to_block), (10, 20));
    }

    /// Bor's log names the token contract first, so the sender sits one slot later
    /// than in an erc-20 transfer and the recipient one after that.
    #[test]
    fn a_network_that_logs_native_transfers_asks_both_sides_of_those_too() {
        let members = vec![watched(1, 1), watched(2, 2)];
        let window = Window { from: 10, to: 20, frontier: Frontier::Forward };
        let tokens = TokenTable::for_params(&POLYGON).expect("the registry parses");
        let built = filters(&POLYGON, &tokens, &members, window);
        let [tokens_sent, tokens_received, coin_sent, coin_received] = built.as_slice() else {
            panic!("four filters: {built:?}")
        };
        assert_eq!(tokens_sent.address, tokens.contracts());
        assert_eq!(tokens_received.address, tokens.contracts());
        let owners: Vec<B256> = members.iter().map(|entry| entry.owner.into_word()).collect();
        let event = Some(vec![abi::NATIVE_TRANSFER_TOPIC]);
        for (filter, expected) in [
            (coin_sent, vec![event.clone(), None, Some(owners.clone())]),
            (coin_received, vec![event, None, None, Some(owners)]),
        ] {
            assert_eq!(filter.address, vec![abi::BOR_NATIVE_TOKEN]);
            assert_eq!(filter.topics, expected);
            assert_eq!((filter.from_block, filter.to_block), (10, 20));
        }
    }

    #[test]
    fn addresses_that_share_a_frontier_share_a_window() {
        let members = vec![watched(1, 1), watched(2, 2), watched(3, 3)];
        let mut cursors = BTreeMap::new();
        let caught_up =
            ScanCursor { forward_to: Some(100), backfill_from: Some(50), ..ScanCursor::default() };
        cursors.insert(1, caught_up);
        cursors.insert(2, caught_up);
        let grouped = groups(&members, &cursors);
        assert_eq!(grouped.len(), 2, "the fresh address forms its own group");
        let shared = grouped
            .iter()
            .find(|group| group.cursor.forward_to == Some(100))
            .expect("the caught-up group");
        assert_eq!(shared.members.len(), 2);
        let fresh = grouped
            .iter()
            .find(|group| group.cursor.forward_to.is_none())
            .expect("the fresh group");
        assert_eq!(fresh.members.len(), 1);
        assert_eq!(fresh.members.first().map(|entry| entry.address_id), Some(3));
    }
}
