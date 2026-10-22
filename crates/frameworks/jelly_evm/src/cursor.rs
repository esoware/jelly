use jelly_core::ChainSyncProgress;
use serde::{Deserialize, Serialize};

use crate::config::EvmConfig;
use crate::error::EvmError;
use crate::params::EvmParams;

const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frontier {
    Forward,
    Backfill,
}

/// An inclusive block range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) frontier: Frontier,
}

impl Window {
    pub(crate) const fn blocks(self) -> u64 {
        if self.to < self.from {
            return 0;
        }
        self.to.saturating_sub(self.from).saturating_add(1)
    }
}

/// The `eth_getLogs` window width, adapted to what endpoints will serve.
///
/// Provider limits differ by orders of magnitude and are unpublished, so the
/// width halves on a refusal and doubles back toward the ceiling after a clean
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowSize {
    blocks: u64,
    floor: u64,
    ceiling: u64,
}

impl WindowSize {
    /// A floor above the ceiling or a zero width is clamped rather than refused,
    /// because a window size only shapes requests and never affects correctness.
    pub(crate) const fn new(params: &EvmParams, config: &EvmConfig) -> Self {
        let ceiling = if params.log_window_blocks == 0 { 1 } else { params.log_window_blocks };
        let floor =
            if config.min_log_window_blocks == 0 { 1 } else { config.min_log_window_blocks };
        let floor = if floor > ceiling { ceiling } else { floor };
        Self { blocks: ceiling, floor, ceiling }
    }

    pub(crate) const fn blocks(self) -> u64 {
        self.blocks
    }

    pub(crate) const fn is_at_floor(self) -> bool {
        self.blocks <= self.floor
    }

    #[must_use]
    pub(crate) const fn shrink(self) -> Self {
        let halved = self.blocks.saturating_div(2);
        Self { blocks: if halved < self.floor { self.floor } else { halved }, ..self }
    }

    #[must_use]
    pub(crate) const fn grow(self) -> Self {
        let doubled = self.blocks.saturating_mul(2);
        Self { blocks: if doubled > self.ceiling { self.ceiling } else { doubled }, ..self }
    }
}

/// Versioned JSON scan state. `forward_to` advances toward the tip and
/// `backfill_from` toward the history floor.
///
/// A new cursor starts one window below the tip so recent activity appears
/// first. The last judged native balance and nonce are kept, so a later pass can
/// tell an unexplained native change from a cold start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScanCursor {
    /// Highest block scanned, inclusive.
    pub(crate) forward_to: Option<u64>,
    /// Lowest block scanned, inclusive.
    pub(crate) backfill_from: Option<u64>,
    pub(crate) native_balance: Option<u128>,
    /// The account nonce read beside `native_balance`, which says which of the
    /// address's own sends that balance already holds.
    pub(crate) native_nonce: Option<u64>,
    pub(crate) settled_through: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    version: u8,
    forward_to: Option<u64>,
    backfill_from: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    native_balance: Option<u128>,
    #[serde(deserialize_with = "Option::deserialize")]
    native_nonce: Option<u64>,
    settled_through: u64,
}

impl ScanCursor {
    pub(crate) fn encode(self) -> Result<Vec<u8>, EvmError> {
        let wire = Wire {
            version: VERSION,
            forward_to: self.forward_to,
            backfill_from: self.backfill_from,
            native_balance: self.native_balance,
            native_nonce: self.native_nonce,
            settled_through: self.settled_through,
        };
        serde_json::to_vec(&wire).map_err(|source| EvmError::Encode { what: "scan cursor", source })
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let wire: Wire = serde_json::from_slice(bytes).ok()?;
        if wire.version != VERSION {
            return None;
        }
        Some(Self {
            forward_to: wire.forward_to,
            backfill_from: wire.backfill_from,
            native_balance: wire.native_balance,
            native_nonce: wire.native_nonce,
            settled_through: wire.settled_through,
        })
    }

    /// Re-reads `overlap` blocks for reorgs without moving the upper edge, so the
    /// overlap adds no request. `None` when already at the tip.
    pub(crate) fn next_forward(self, tip: u64, window: u64, overlap: u64) -> Option<Window> {
        let window = window.max(1);
        match self.forward_to {
            None => Some(Window {
                from: tip.saturating_sub(window.saturating_sub(1)),
                to: tip,
                frontier: Frontier::Forward,
            }),
            Some(done) if done >= tip => None,
            Some(done) => Some(Window {
                from: done.saturating_add(1).saturating_sub(overlap),
                to: tip.min(done.saturating_add(window)),
                frontier: Frontier::Forward,
            }),
        }
    }

    /// `None` when history is complete or the forward walk has not set a starting
    /// point yet.
    pub(crate) fn next_backfill(self, floor: u64, window: u64) -> Option<Window> {
        let window = window.max(1);
        let start = self.backfill_from?;
        if start <= floor {
            return None;
        }
        let to = start.saturating_sub(1);
        let from = to.saturating_sub(window.saturating_sub(1)).max(floor);
        Some(Window { from, to, frontier: Frontier::Backfill })
    }

    /// Records a scanned range from a log window or an indexer.
    ///
    /// The first forward range sets where the backfill starts. An indexed read is
    /// recorded as a forward range ending as far as it got, so a truncated one
    /// resumes instead of repeating from the floor.
    pub(crate) fn apply(&mut self, window: Window) {
        if window.frontier == Frontier::Forward {
            self.forward_to = Some(window.to);
        }
        self.backfill_from =
            Some(self.backfill_from.map_or(window.from, |low| low.min(window.from)));
    }

    /// Copies only the frontiers. A group cursor carries no balance, so copying it
    /// whole would erase the native balance this address remembers.
    pub(crate) const fn take_frontiers(&mut self, other: Self) {
        self.forward_to = other.forward_to;
        self.backfill_from = other.backfill_from;
    }

    pub(crate) const fn is_backfill_complete(self, floor: u64) -> bool {
        match self.backfill_from {
            Some(from) => from <= floor,
            None => false,
        }
    }

    /// Indexed history starts at the floor until backfill completes, then
    /// `overlap` blocks before the forward frontier.
    ///
    /// Reading from the floor finds native transfers that earlier log-only scans
    /// missed, and the overlap re-reads possible reorgs.
    pub(crate) fn history_from(self, floor: u64, overlap: u64) -> u64 {
        if self.is_backfill_complete(floor) {
            self.forward_to.map_or(floor, |to| to.saturating_sub(overlap).max(floor))
        } else {
            floor
        }
    }

    pub(crate) fn progress_permille(self, floor: u64, tip: u64) -> u16 {
        let (Some(from), Some(to)) = (self.backfill_from, self.forward_to) else {
            return 0;
        };
        ChainSyncProgress::permille_of(to.saturating_sub(from), tip.saturating_sub(floor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ETHEREUM;

    const FLOOR: u64 = 4_634_748;

    fn indexed(cursor: &mut ScanCursor, from: u64, to: u64) {
        cursor.apply(Window { from, to, frontier: Frontier::Forward });
    }

    #[test]
    fn a_cold_cursor_starts_one_window_below_the_tip() {
        let cursor = ScanCursor::default();
        let window = cursor.next_forward(1_000, 100, 0).expect("a first window");
        assert_eq!((window.from, window.to, window.blocks()), (901, 1_000, 100));
        assert_eq!(window.frontier, Frontier::Forward);
        assert_eq!(cursor.next_backfill(0, 100), None, "the backfill needs a starting point");
        assert_eq!(cursor.progress_permille(0, 1_000), 0);
    }

    #[test]
    fn forward_windows_advance_and_stop_at_the_tip_without_moving_the_floor() {
        let mut cursor = ScanCursor::default();
        let first = cursor.next_forward(1_000, 100, 0).expect("a window");
        cursor.apply(first);
        assert_eq!(cursor.forward_to, Some(1_000));
        assert_eq!(cursor.backfill_from, Some(901));
        assert_eq!(cursor.next_forward(1_000, 100, 0), None);

        let next = cursor.next_forward(1_150, 100, 0).expect("a window");
        assert_eq!((next.from, next.to), (1_001, 1_100));
        cursor.apply(next);
        let last = cursor.next_forward(1_150, 100, 0).expect("a window");
        assert_eq!((last.from, last.to), (1_101, 1_150));
        cursor.apply(last);
        assert_eq!(cursor.next_forward(1_150, 100, 0), None);
        assert_eq!(cursor.backfill_from, Some(901), "walking forward never moves the floor");
    }

    #[test]
    fn the_backfill_walks_down_and_clamps_at_the_floor() {
        let mut cursor = ScanCursor::default();
        cursor.apply(cursor.next_forward(1_000, 100, 0).expect("a window"));

        for expected in [(801, 900), (701, 800), (700, 700)] {
            let window = cursor.next_backfill(700, 100).expect("a window");
            assert_eq!((window.from, window.to), expected);
            assert_eq!(window.frontier, Frontier::Backfill);
            cursor.apply(window);
        }
        assert_eq!(cursor.next_backfill(700, 100), None);
        assert!(cursor.is_backfill_complete(700));
        assert_eq!(cursor.progress_permille(700, 1_000), 1_000);
        assert_eq!(cursor.forward_to, Some(1_000), "the backfill never moves the tip frontier");
    }

    #[test]
    fn a_tip_at_the_floor_leaves_nothing_to_scan() {
        let mut cursor = ScanCursor::default();
        cursor.apply(Window { from: 700, to: 700, frontier: Frontier::Forward });
        assert_eq!(cursor.progress_permille(700, 700), 1_000);
    }

    /// `eth_getLogs` answers about the chain as it is now, so the overlap re-reads
    /// the recent blocks a reorg could have changed, inside a window that was going
    /// out anyway.
    #[test]
    fn the_tip_ward_walk_re_reads_the_frontier_the_last_pass_left() {
        let mut cursor = ScanCursor::default();
        cursor.apply(cursor.next_forward(1_000, 100, 0).expect("a window"));
        assert_eq!(cursor.forward_to, Some(1_000));

        let next = cursor.next_forward(1_050, 100, 12).expect("a window");
        assert_eq!((next.from, next.to), (989, 1_050), "twelve blocks back, same upper edge");
        assert_eq!(cursor.next_forward(1_050, 100, 12).expect("a window").to, 1_050);

        // A frontier at the tip is finished, so the overlap cannot loop the walk
        // forever.
        cursor.apply(next);
        assert_eq!(cursor.next_forward(1_050, 100, 12), None);
        assert_eq!(cursor.backfill_from, Some(901), "walking forward never moves the floor");
    }

    /// An indexer answers about the chain as it is now too, so its resume point
    /// takes the same overlap and never drops below the floor.
    #[test]
    fn an_indexed_read_resumes_the_same_overlap_below_the_frontier() {
        let mut cursor = ScanCursor::default();
        indexed(&mut cursor, FLOOR, 18_000_010);
        assert_eq!(cursor.history_from(FLOOR, 12), 18_000_010 - 12);

        let mut shallow = ScanCursor::default();
        indexed(&mut shallow, FLOOR, FLOOR.saturating_add(3));
        assert_eq!(shallow.history_from(FLOOR, 12), FLOOR, "the overlap stops at the floor");
    }

    #[test]
    fn a_window_of_zero_blocks_still_makes_progress() {
        let cursor = ScanCursor::default();
        let window = cursor.next_forward(10, 0, 0).expect("a window");
        assert_eq!((window.from, window.to), (10, 10), "a zero width is read as one block");
        assert_eq!(Window { from: 10, to: 9, frontier: Frontier::Forward }.blocks(), 0);
    }

    #[test]
    fn a_truncated_indexer_read_resumes_where_it_stopped_rather_than_at_the_floor() {
        let mut cursor = ScanCursor::default();
        assert_eq!(cursor.history_from(FLOOR, 0), FLOOR, "a cold cursor reads the whole history");

        indexed(&mut cursor, FLOOR, 18_000_010);
        assert_eq!(cursor.forward_to, Some(18_000_010));
        assert_eq!(cursor.backfill_from, Some(FLOOR), "everything below what it read is covered");
        assert!(cursor.is_backfill_complete(FLOOR));
        assert_eq!(
            cursor.history_from(FLOOR, 0),
            18_000_010,
            "the next read continues instead of repeating ten thousand rows"
        );

        indexed(&mut cursor, 18_000_010, 18_000_100);
        assert_eq!(cursor.history_from(FLOOR, 0), 18_000_100);
        assert_eq!(cursor.backfill_from, Some(FLOOR), "reading forward never lifts the floor");
    }

    #[test]
    fn an_indexer_reads_from_the_floor_while_only_the_tip_has_been_scanned() {
        let mut cursor = ScanCursor::default();
        cursor.apply(Window { from: 18_000_000, to: 18_000_100, frontier: Frontier::Forward });
        assert_eq!(
            cursor.history_from(FLOOR, 0),
            FLOOR,
            "a log walk that only touched the tip has not covered the history"
        );
        assert_eq!(
            cursor.progress_permille(FLOOR, 18_000_100),
            0,
            "a hundred blocks of thirteen million is not one part in a thousand"
        );
    }

    #[test]
    fn a_cursor_round_trips_and_unreadable_formats_are_refused() {
        let cursor = ScanCursor {
            forward_to: Some(21_000_000),
            backfill_from: Some(20_999_000),
            native_balance: Some(u128::MAX),
            native_nonce: Some(41),
            settled_through: 7,
        };
        let bytes = cursor.encode().expect("it encodes");
        assert_eq!(ScanCursor::decode(&bytes), Some(cursor));
        assert_eq!(ScanCursor::decode(b"not json"), None);
        assert_eq!(ScanCursor::decode(br#"{"version":9}"#), None);
        assert_eq!(ScanCursor::decode(br#"{"version":0}"#), None, "an unstamped blob is refused");
    }

    #[test]
    fn required_cursor_fields_are_independently_validated() {
        let cursor =
            ScanCursor { native_balance: Some(5), native_nonce: Some(0), ..ScanCursor::default() };
        let document: serde_json::Value =
            serde_json::from_slice(&cursor.encode().unwrap()).unwrap();
        assert_eq!(ScanCursor::decode(&serde_json::to_vec(&document).unwrap()), Some(cursor));
        for field in ["nativeBalance", "nativeNonce", "settledThrough"] {
            let mut missing = document.clone();
            assert!(missing.as_object_mut().unwrap().remove(field).is_some());
            assert!(
                ScanCursor::decode(&serde_json::to_vec(&missing).unwrap()).is_none(),
                "missing {field}"
            );
        }
        for field in ["nativeBalance", "nativeNonce"] {
            let mut nullable = document.clone();
            nullable[field] = serde_json::Value::Null;
            let decoded = ScanCursor::decode(&serde_json::to_vec(&nullable).unwrap()).unwrap();
            assert_eq!(
                decoded.native_balance,
                if field == "nativeBalance" { None } else { Some(5) }
            );
            assert_eq!(decoded.native_nonce, if field == "nativeNonce" { None } else { Some(0) });
        }
        let mut invalid = document;
        invalid["settledThrough"] = serde_json::Value::Null;
        assert!(ScanCursor::decode(&serde_json::to_vec(&invalid).unwrap()).is_none());
        assert_eq!(
            ScanCursor::decode(&ScanCursor::default().encode().unwrap()),
            Some(ScanCursor::default())
        );
    }

    #[test]
    fn the_window_bounds_are_ordered_whatever_the_configuration_says() {
        let config = EvmConfig::default();
        let shipped = WindowSize::new(&ETHEREUM, &config);
        assert_eq!((shipped.floor, shipped.ceiling), (32, 1_000));

        let narrow = EvmParams { log_window_blocks: 10, ..ETHEREUM };
        let high_floor = EvmConfig { min_log_window_blocks: 500, ..EvmConfig::default() };
        let inverted = WindowSize::new(&narrow, &high_floor);
        assert_eq!(
            (inverted.floor, inverted.ceiling),
            (10, 10),
            "a floor above the ceiling is the ceiling"
        );

        let zero = EvmParams { log_window_blocks: 0, ..ETHEREUM };
        let no_floor = EvmConfig { min_log_window_blocks: 0, ..EvmConfig::default() };
        let zeroed = WindowSize::new(&zero, &no_floor);
        assert_eq!((zeroed.floor, zeroed.ceiling), (1, 1), "a window of no blocks is one block");
    }

    #[test]
    fn the_window_width_halves_on_a_refusal_and_grows_back_to_the_ceiling() {
        let start = WindowSize::new(&ETHEREUM, &EvmConfig::default());
        assert_eq!(start.blocks(), 1_000);
        assert!(!start.is_at_floor());

        let shrunk = start.shrink();
        assert_eq!(shrunk.blocks(), 500);
        assert_eq!(shrunk.grow().blocks(), 1_000);
        assert_eq!(start.grow().blocks(), 1_000, "the ceiling holds");

        let mut narrow = start;
        for _ in 0..10u8 {
            narrow = narrow.shrink();
        }
        assert_eq!(narrow.blocks(), 32, "the floor holds");
        assert!(narrow.is_at_floor());
        assert_eq!(narrow.shrink().blocks(), 32);
    }
}
