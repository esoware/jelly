//! When a pass asks the indexer for an address's history.
//!
//! The balances every pass reads anyway are the signal, so a pass that finds
//! them where the last one left them asks the indexer nothing. None of this is
//! persisted, so after a restart every address is read once.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use jelly_core::{AssetId, Clock, Deadline, TxDirection, TxRecord};

use crate::config::TRX;
use crate::cursor::{Stream, StreamCursor};

/// How long news keeps an address's history read every pass.
///
/// The indexer can list a transfer a few blocks after the balance shows it. A
/// move still unexplained when the window runs out was not a transfer, such as
/// TRX staked or unstaked, and is let go.
const NEWS_WINDOW: Duration = Duration::from_secs(60);

/// How long an address's history streams go unread while nothing says it has
/// news.
///
/// A pass reads a stream only while its walk is unfinished, for up to
/// [`NEWS_WINDOW`] after a balance moved by more than the records already
/// reported explain, or while a send of the address's own lands, so this sweep is
/// what finds a transfer no balance showed, such as a payment in and out between
/// two passes. Every pass reads one balance per asset, which costs 7,200
/// requests an asset a day at a twelve-second interval. Reading the three
/// streams every pass would add 21,600 an address a day, and at ten minutes the
/// sweep adds 432.
const HISTORY_SWEEP: Duration = Duration::from_secs(600);

/// What decides when the indexer next reads one address's history streams.
///
/// A stream is due while its walk is unfinished, once its sweep comes round,
/// which on the first pass that sees the address is at once, and while there is
/// news.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// The balances the passes so far read, in base units. A failed read keeps
    /// the one before it, so a move is still seen once a read succeeds.
    balances: BTreeMap<AssetId, u128>,
    /// How far each balance moved since the news began beyond what the records
    /// reported since then account for.
    unexplained: BTreeMap<AssetId, i128>,
    /// Set while there is news.
    news_until: Option<Deadline>,
    /// When each stream is next read whatever the news. A stream with no entry
    /// has not been read since the adapter was built.
    sweeps: HashMap<Stream, Deadline>,
}

impl Gate {
    /// Takes the balances this pass read. A move is news, which lasts until the
    /// walks have reported records that account for every move since it began,
    /// or until its window runs out.
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

    /// The native and TRC-20 streams are read on news, and while a send of the
    /// address's own lands or settles. The internal stream is read on news only
    /// while what the other two reported leaves a TRX move unexplained, such as a
    /// payment a contract made.
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

    /// A stream that was read, cleanly or not, waits for the next news or for
    /// the next sweep, so a transaction no endpoint can place is not asked about
    /// every pass.
    pub(crate) fn schedule_sweep(&mut self, stream: Stream, clock: &dyn Clock) {
        self.sweeps.insert(stream, clock.deadline_in(HISTORY_SWEEP));
    }

    /// Nets what the walks reported against the news, and ends it once they
    /// reported records and nothing is left unexplained. A walk that reported
    /// nothing has not seen the news yet, even when the moves since it began
    /// cancel out.
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

/// What the records the forward walks reported this pass moved. The backfill is
/// left out, because it reads history older than the balances the last pass saw.
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
