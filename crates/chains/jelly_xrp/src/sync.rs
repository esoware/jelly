use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, WalletEvent,
};

use crate::client::{AccountRoot, XrpClient};
use crate::config::CHAIN;
use crate::error::XrpError;
use crate::pending;
use crate::record::{ledger_index, record};
use crate::state::States;
use crate::watched::Watched;

const PAGES_PER_PASS: usize = 3;
const PARTIAL_PROGRESS_PERMILLE: u16 = 500;

pub(crate) async fn run(
    client: &XrpClient,
    states: &States,
    watched: &[Watched],
    clock: &dyn Clock,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<ChainSyncProgress, XrpError> {
    states.ensure_readable()?;
    let ledger = client.ledger().await?;
    let mut is_complete = true;
    for entry in watched {
        if deadline.is_expired(clock) {
            is_complete = false;
            break;
        }
        let account = client.account(&entry.address, &ledger).await?;
        let balance_drops =
            account.as_ref().map(AccountRoot::balance_drops).transpose()?.unwrap_or(0);
        events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: entry.address_id,
            asset: AssetId::Native(CHAIN),
            balance: Amount::from(balance_drops),
        }));
        // Read the cursor before following the held send, so an unreadable one blocks
        // spending before anything is resubmitted.
        let mut cursor = states.cursor(entry.address_id).await?;
        pending::follow(client, states, entry, ledger.seq, events).await?;
        for _ in 0..PAGES_PER_PASS {
            if deadline.is_expired(clock) {
                is_complete = false;
                break;
            }
            if cursor.is_past(ledger.seq) {
                break;
            }
            let (first_ledger, last_ledger) = cursor.range(ledger.seq);
            let page =
                client.history(&entry.address, first_ledger, last_ledger, cursor.marker()).await?;
            let reached_ledger = page.rows.last().map(ledger_index).transpose()?;
            cursor.mark_read(page.marker, reached_ledger, ledger.seq)?;
            for row in &page.rows {
                if let Some(record) = record(row, entry.address_id, &entry.address, ledger.seq)? {
                    events.emit(WalletEvent::TxDetected(record)).await;
                }
            }
            states.advance(entry.address_id, &cursor).await?;
            if cursor.has_read_through(ledger.seq) {
                break;
            }
        }
        if !cursor.has_read_through(ledger.seq) {
            is_complete = false;
        }
    }
    let progress_permille =
        if is_complete { ChainSyncProgress::MAX_PERMILLE } else { PARTIAL_PROGRESS_PERMILLE };
    Ok(ChainSyncProgress::new(CHAIN)
        .with_progress(progress_permille)
        .with_heights(is_complete.then_some(u64::from(ledger.seq)), Some(u64::from(ledger.seq))))
}
