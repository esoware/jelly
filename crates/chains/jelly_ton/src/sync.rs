use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, WalletEvent,
};

use crate::client::{AccountState, Position, TonClient};
use crate::config::CHAIN;
use crate::cursor::{Cursor, JettonCursor, Stream};
use crate::error::TonError;
use crate::history::{Held, Reader};
use crate::jetton::{JETTONS, master};
use crate::pending;
use crate::state::States;
use crate::watched::Watched;

/// How many pages of transactions one walk reads in a pass.
const PAGES_PER_PASS: usize = 3;
const PARTIAL_PROGRESS_PERMILLE: u16 = 500;

/// The context every walk of one address shares in a pass.
struct Pass<'a> {
    client: &'a TonClient,
    states: &'a States,
    entry: &'a Watched,
    clock: &'a dyn Clock,
    deadline: Deadline,
    events: &'a dyn EventSink,
}

pub(crate) async fn run(
    client: &TonClient,
    states: &States,
    watched: &[Watched],
    clock: &dyn Clock,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<ChainSyncProgress, TonError> {
    states.ensure_readable()?;
    let tip = client.tip().await?;
    let mut is_complete = true;
    for entry in watched {
        if deadline.is_expired(clock) {
            is_complete = false;
            break;
        }
        let pass = Pass { client, states, entry, clock, deadline, events };
        if !pass.sync_address(tip).await? {
            is_complete = false;
        }
    }
    let progress_permille =
        if is_complete { ChainSyncProgress::MAX_PERMILLE } else { PARTIAL_PROGRESS_PERMILLE };
    Ok(ChainSyncProgress::new(CHAIN)
        .with_progress(progress_permille)
        .with_heights(is_complete.then_some(tip), Some(tip)))
}

impl Pass<'_> {
    /// Syncs one address and reports whether every history of it now stands at its head.
    async fn sync_address(&self, tip: u64) -> Result<bool, TonError> {
        let entry = self.entry;
        let account = self.client.account(&entry.address, tip).await?;
        self.events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: entry.address_id,
            asset: AssetId::Native(CHAIN),
            balance: Amount::from(account.balance_nanogram()?),
        }));
        let mut cursor = self.states.cursor(entry.address_id).await?;
        for jetton in JETTONS {
            if cursor.jettons.iter().all(|known| known.token != jetton.token) {
                let wallet = self
                    .client
                    .jetton_wallet(&master(jetton.token)?, &entry.address, Some(tip))
                    .await?;
                cursor.jettons.push(JettonCursor::named(jetton.token, &wallet));
                self.states.advance(entry.address_id, &cursor).await?;
            }
        }
        let held = cursor.held()?;
        let reader = Reader {
            address_id: entry.address_id,
            owner: &entry.address,
            held: &held,
            tip,
            sync_utime: account.sync_utime,
        };
        let floor = self.states.sent_floor(entry.address_id);
        if !self
            .walk(&reader, &mut cursor, Stream::Wallet, &account.last_transaction_id, floor)
            .await?
        {
            return Ok(false);
        }
        for record in reader.settle(&mut cursor.returns)? {
            self.events.emit(WalletEvent::TxDetected(record)).await;
        }
        self.states.advance(entry.address_id, &cursor).await?;
        pending::follow(self.client, self.states, entry, account.sync_utime, self.events).await?;
        let mut is_read = true;
        for index in 0..cursor.jettons.len() {
            if self.deadline.is_expired(self.clock) {
                return Ok(false);
            }
            if !self.sync_jetton(&reader, &mut cursor, index, tip).await? {
                is_read = false;
            }
        }
        Ok(is_read)
    }

    /// Reads one jetton wallet's balance and history. A jetton wallet that is not
    /// deployed holds nothing.
    async fn sync_jetton(
        &self,
        reader: &Reader<'_>,
        cursor: &mut Cursor,
        index: usize,
        tip: u64,
    ) -> Result<bool, TonError> {
        let jetton = cursor
            .jettons
            .get_mut(index)
            .ok_or(TonError::Internal("a jetton cursor went missing"))?;
        let wallet = jetton.address()?;
        let account = self.client.account(&wallet, tip).await?;
        let head_lt = account.last_transaction_id.lt;
        match account.state {
            AccountState::Active if jetton.is_confirmed_at(head_lt) => {}
            AccountState::Active => {
                let balance = self
                    .client
                    .jetton_balance(&wallet, &self.entry.address, &master(jetton.token)?, tip)
                    .await?;
                jetton.confirm(balance, head_lt);
            }
            AccountState::Uninitialized => jetton.empty(head_lt),
            AccountState::Frozen => {}
        }
        self.events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: self.entry.address_id,
            asset: AssetId::Token(jetton.token),
            balance: jetton.balance(),
        }));
        self.states.advance(self.entry.address_id, cursor).await?;
        self.walk(reader, cursor, Stream::Jetton(index), &account.last_transaction_id, None).await
    }

    /// Reads up to [`PAGES_PER_PASS`] pages of one walk and reports whether it now
    /// stands at `head`.
    async fn walk(
        &self,
        reader: &Reader<'_>,
        cursor: &mut Cursor,
        stream: Stream,
        head: &Position,
        floor: Option<u64>,
    ) -> Result<bool, TonError> {
        let held = match stream {
            Stream::Wallet => None,
            Stream::Jetton(index) => {
                let jetton = cursor
                    .jettons
                    .get(index)
                    .ok_or(TonError::Internal("a jetton cursor went missing"))?;
                Some(Held { token: jetton.token, wallet: jetton.address()? })
            }
        };
        let address = held.as_ref().map_or(&self.entry.address, |held| &held.wallet);
        cursor.walk_mut(stream)?.begin(head, floor);
        for _ in 0..PAGES_PER_PASS {
            if self.deadline.is_expired(self.clock) {
                break;
            }
            let Some(position) = cursor.walk_mut(stream)?.take_next() else {
                break;
            };
            let rows = self.client.transactions(address, &position).await?;
            let stop_lt = cursor.walk_mut(stream)?.stop_lt();
            let mut expected = position;
            for row in rows {
                if row.transaction_id != expected {
                    return Err(TonError::Protocol);
                }
                let tx = row.read(address)?;
                let records = match held {
                    None => reader.wallet_tx(&tx, &mut cursor.returns)?,
                    Some(ref held) => reader.jetton_tx(&tx, held, &mut cursor.returns)?,
                };
                for record in records {
                    let should_settle = held.is_none()
                        && self.states.is_settled_by(self.entry.address_id, record.txid.value());
                    self.events.emit(WalletEvent::TxDetected(record)).await;
                    if should_settle {
                        self.states.settle(self.entry.address_id).await?;
                    }
                }
                expected = tx.previous;
                if expected.lt <= stop_lt {
                    break;
                }
            }
            cursor.walk_mut(stream)?.mark_read(expected);
            self.states.advance(self.entry.address_id, cursor).await?;
        }
        Ok(cursor.walk_mut(stream)?.is_at(head.lt))
    }
}
