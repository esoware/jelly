//! Decides when a pass reads an address's history from the indexer.
//!
//! Every pass reads balances anyway, so they're the main signal, and a pass that
//! finds them unchanged usually leaves the indexer alone. None of this is
//! persisted, so every address is read once after a restart.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use jelly_core::{AssetId, Clock, Deadline, TxDirection, TxRecord};

use crate::config::TRX;
use crate::cursor::{Stream, StreamCursor};

/// How long after a balance moves the address's history is read every pass.
///
/// The indexer can list a transfer a few blocks after the balance shows it. A
/// move still unexplained after this wasn't a transfer (staking or unstaking TRX,
/// say) and is dropped.
const NEWS_WINDOW: Duration = Duration::from_secs(60);

/// How long an address's history goes unread when nothing suggests news.
///
/// Apart from the sweep, a stream is read only while its walk is unfinished,
/// within [`NEWS_WINDOW`] of a balance move the reported records don't explain, or
/// while the address's own send lands. The sweep catches what no balance showed,
/// such as a payment in and out between two passes. At a 12-second poll, reading
/// all three streams every pass would cost 21,600 requests per address a day, and
/// a 10-minute sweep costs 432.
const HISTORY_SWEEP: Duration = Duration::from_secs(600);

/// Decides when an address's history streams are next read.
///
/// A stream is due while its walk is unfinished, when its sweep comes round (at
/// once on the first pass that sees the address), and while there's news.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// The last balances read, in base units. A failed read keeps the previous
    /// one, so a move still shows once a read succeeds.
    balances: BTreeMap<AssetId, u128>,
    /// How far each balance has moved since the news began, net of what the
    /// records reported since then explain.
    unexplained: BTreeMap<AssetId, i128>,
    /// When the current news expires, if there is any.
    news_until: Option<Deadline>,
    /// When each stream is next read regardless of news. No entry means it hasn't
    /// been read since the adapter was built.
    sweeps: HashMap<Stream, Deadline>,
}

impl Gate {
    /// Takes the balances this pass read. A move starts news, which lasts until
    /// the walks report records that explain every move since, or until its window
    /// runs out.
    pub(crate) fn observe(&mut self, balances: &[(AssetId, u128)], clock: &dyn Clock) {
        if self.news_until.is_some_and(|until| until.is_expired(clock)) {
            self.news_until = None;
            self.unexplained.clear();
        }
        for &(asset, balance) in balances {
            let Some(before) = self.balances.insert(asset, balance) else {
                continue;
            };
            if before != balance {
                let unexplained = self.unexplained.entry(asset).or_default();
                *unexplained = unexplained.saturating_add(
                    to_i128_saturating(balance).saturating_sub(to_i128_saturating(before)),
                );
                self.news_until = Some(clock.deadline_in(NEWS_WINDOW));
            }
        }
    }

    /// The native and TRC-20 streams are read on news and while the address's own
    /// send lands or settles. The internal stream is read on news only while the
    /// other two leave a TRX move unexplained, such as a payment from a contract.
    pub(crate) fn is_due(
        &self,
        stream: Stream,
        cursor: &StreamCursor,
        walked: &Explained,
        has_own_send: bool,
        clock: &dyn Clock,
    ) -> bool {
        let has_news = self.news_until.is_some_and(|until| !until.is_expired(clock));
        let is_news = match stream {
            Stream::Native | Stream::Token => has_news || has_own_send,
            Stream::Internal => {
                let unexplained_sun = self.unexplained.get(&TRX).copied().unwrap_or_default();
                has_news && unexplained_sun != walked.moved.get(&TRX).copied().unwrap_or_default()
            }
        };
        is_news
            || !cursor.is_complete
            || cursor.resume.is_some()
            || self.sweeps.get(&stream).is_none_or(|at| at.is_expired(clock))
    }

    /// After a read, clean or not, a stream waits for news or the next sweep, so a
    /// transaction no endpoint can place isn't asked about every pass.
    pub(crate) fn schedule_sweep(&mut self, stream: Stream, clock: &dyn Clock) {
        self.sweeps.insert(stream, clock.deadline_in(HISTORY_SWEEP));
    }

    /// Nets what the walks reported against the news, and ends the news once
    /// they've reported records and nothing is left unexplained. A walk that
    /// reported nothing hasn't seen the news yet, even if the moves since cancel
    /// out.
    pub(crate) fn reconcile(&mut self, walked: &Explained) {
        if self.news_until.is_none() {
            return;
        }
        for (asset, moved) in &walked.moved {
            let unexplained = self.unexplained.entry(*asset).or_default();
            *unexplained = unexplained.saturating_sub(*moved);
        }
        if walked.has_records && self.unexplained.values().all(|unexplained| *unexplained == 0) {
            self.news_until = None;
            self.unexplained.clear();
        }
    }
}

/// What the records from this pass's forward walks moved. The backfill doesn't
/// count, since it reads history older than the balances the last pass saw.
#[derive(Debug, Default)]
pub(crate) struct Explained {
    /// By asset, in base units.
    moved: BTreeMap<AssetId, i128>,
    has_records: bool,
}

impl Explained {
    /// A record moves its own asset, and its fee burns TRX. A TRC-20 movement
    /// counts only from the TRC-20 stream, because the native stream reports the
    /// same transfer again to carry its fee.
    pub(crate) fn add(&mut self, stream: Stream, record: &TxRecord) {
        let amount = to_i128_saturating(record.amount.base_units());
        let moved = match record.direction {
            TxDirection::Incoming => amount,
            TxDirection::Outgoing => amount.saturating_neg(),
            TxDirection::SelfTransfer => 0,
        };
        let is_counted = match record.asset {
            AssetId::Native(_) => true,
            AssetId::Token(_) => stream == Stream::Token,
        };
        if is_counted {
            let total = self.moved.entry(record.asset).or_default();
            *total = total.saturating_add(moved);
        }
        if let Some(fee) = record.fee {
            let total = self.moved.entry(TRX).or_default();
            *total = total.saturating_sub(to_i128_saturating(fee.base_units()));
        }
        self.has_records = true;
    }
}

fn to_i128_saturating(units: u128) -> i128 {
    i128::try_from(units).unwrap_or(i128::MAX)
}

#[cfg(test)]
mod tests {
    use jelly_core::{Amount, TokenId, test_util::FakeClock};

    use super::*;

    /// A gate whose every stream is walked and swept, holding `sun` TRX.
    fn quiet(sun: u128, clock: &FakeClock) -> Gate {
        let mut gate = Gate::default();
        gate.observe(&[(TRX, sun)], clock);
        for stream in Stream::ALL {
            gate.schedule_sweep(stream, clock);
        }
        gate
    }

    fn complete_cursor() -> StreamCursor {
        StreamCursor { is_complete: true, ..StreamCursor::default() }
    }

    fn received(sun: u128) -> TxRecord {
        TxRecord::builder(
            TRX,
            1,
            jelly_core::TxId::new(TRX.chain(), "aa"),
            TxDirection::Incoming,
            Amount::from_base_units(sun),
        )
        .build()
    }

    #[test]
    fn a_first_balance_is_no_news_and_a_moved_one_is() {
        let clock = FakeClock::default();
        let mut gate = quiet(5, &clock);
        let nothing = Explained::default();
        assert!(!gate.is_due(Stream::Native, &complete_cursor(), &nothing, false, &clock));
        assert!(
            gate.is_due(Stream::Native, &complete_cursor(), &nothing, true, &clock),
            "an own send"
        );
        assert!(gate.is_due(Stream::Native, &StreamCursor::default(), &nothing, false, &clock));

        gate.observe(&[(TRX, 7)], &clock);
        for stream in Stream::ALL {
            assert!(gate.is_due(stream, &complete_cursor(), &nothing, false, &clock), "{stream:?}");
        }
        clock.advance(NEWS_WINDOW);
        assert!(
            !gate.is_due(Stream::Native, &complete_cursor(), &nothing, false, &clock),
            "stale news"
        );
    }

    #[test]
    fn the_internal_stream_is_read_only_while_trx_moved_unexplained() {
        let clock = FakeClock::default();
        let mut gate = quiet(5, &clock);
        gate.observe(&[(TRX, 7)], &clock);

        let mut explained = Explained::default();
        explained.add(Stream::Native, &received(2));
        assert!(!gate.is_due(Stream::Internal, &complete_cursor(), &explained, false, &clock));
        assert!(gate.is_due(Stream::Native, &complete_cursor(), &explained, false, &clock));

        gate.reconcile(&explained);
        assert!(!gate.is_due(
            Stream::Native,
            &complete_cursor(),
            &Explained::default(),
            false,
            &clock
        ));
    }

    #[test]
    fn a_walk_that_reported_nothing_leaves_the_news_open() {
        let clock = FakeClock::default();
        let mut gate = quiet(5, &clock);
        gate.observe(&[(TRX, 7)], &clock);
        gate.observe(&[(TRX, 5)], &clock);
        gate.reconcile(&Explained::default());
        assert!(gate.is_due(
            Stream::Native,
            &complete_cursor(),
            &Explained::default(),
            false,
            &clock
        ));
    }

    #[test]
    fn a_token_transfer_counts_only_from_the_token_stream() {
        let usdt = AssetId::Token(TokenId::UsdtTrc20);
        let record = TxRecord::builder(
            usdt,
            1,
            jelly_core::TxId::new(usdt.chain(), "aa"),
            TxDirection::Outgoing,
            Amount::from_base_units(3),
        )
        .fee(Amount::from_base_units(9))
        .build();
        let mut native = Explained::default();
        native.add(Stream::Native, &record);
        assert_eq!(native.moved.get(&usdt), None);
        assert_eq!(native.moved.get(&TRX), Some(&-9), "the fee still burns TRX");

        let mut token = Explained::default();
        token.add(Stream::Token, &record);
        assert_eq!(token.moved.get(&usdt), Some(&-3));
    }
}
