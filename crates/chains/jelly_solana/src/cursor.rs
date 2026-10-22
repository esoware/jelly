//! Signature history resumes with separate forward and backfill walks. Each gets
//! the full page budget, so a one-page forward check cannot starve the backfill.
//!
//! Anchors advance only over rows turned into records, and the forward frontier
//! moves only once catch-up finishes, so no gap opens across passes.

use serde::{Deserialize, Serialize};

use crate::config::SolanaConfig;
use crate::rpc::SignatureRow;

/// The anchors one address's scan resumes from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignatureCursor {
    /// The newest signature already turned into records. The next forward walk
    /// stops locally when it reaches this signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) newest: Option<String>,
    /// Where a forward walk that ran out of budget picks up, as `before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) forward_before: Option<String>,
    /// The newest signature that interrupted walk saw. It becomes `newest` when
    /// the walk finally terminates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) forward_newest: Option<String>,
    /// Where the historical walk continues, as `before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) backfill_before: Option<String>,
    /// True once the historical walk has reached the end of the address's
    /// history.
    #[serde(rename = "backfillComplete")]
    pub(crate) is_backfill_complete: bool,
}

impl SignatureCursor {
    /// True while a forward walk is part way through a multi-pass run, so the
    /// frontier has not moved yet.
    const fn is_forward_in_progress(&self) -> bool {
        self.forward_before.is_some()
    }

    /// True when this address's whole history has been read at least once.
    pub(crate) const fn is_complete(&self) -> bool {
        self.is_backfill_complete && !self.is_forward_in_progress()
    }
}

/// One page request with a local stop signature, exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageRequest {
    pub(crate) limit: u32,
    pub(crate) before: Option<String>,
    pub(crate) until: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Forward,
    Backfill,
    Done,
}

/// Where one walk stands, apart from the rows it has found.
#[derive(Debug, Clone)]
struct Position {
    cursor: SignatureCursor,
    phase: Phase,
    before: Option<String>,
    pending_newest: Option<String>,
}

impl Position {
    /// An address whose first walk found no history has no frontier, so its
    /// forward walk runs with no `until` and covers whatever has arrived since.
    fn new(cursor: SignatureCursor) -> Self {
        let phase = if cursor.newest.is_some() || cursor.is_backfill_complete {
            Phase::Forward
        } else {
            Phase::Backfill
        };
        let before = match phase {
            Phase::Forward => cursor.forward_before.clone(),
            Phase::Backfill => cursor.backfill_before.clone(),
            Phase::Done => None,
        };
        let pending_newest = cursor.forward_newest.clone();
        Self { cursor, phase, before, pending_newest }
    }

    fn request(&self, limit: u32) -> Option<PageRequest> {
        match self.phase {
            Phase::Done => None,
            Phase::Forward => Some(PageRequest {
                limit,
                before: self.before.clone(),
                until: self.cursor.newest.clone(),
            }),
            Phase::Backfill => {
                Some(PageRequest { limit, before: self.before.clone(), until: None })
            }
        }
    }

    /// Moves the anchors past `rows`, newest first.
    fn advance(&mut self, rows: &[SignatureRow]) {
        let newest = rows.first().map(|row| row.signature.clone());
        match self.phase {
            Phase::Done => return,
            Phase::Forward => {
                if self.pending_newest.is_none() {
                    self.pending_newest = newest;
                }
            }
            Phase::Backfill => {
                // The first backfill page of a wallet's first ever pass starts at
                // the tip, so it is the only page that may seed the frontier.
                if self.before.is_none() && self.cursor.newest.is_none() {
                    self.cursor.newest = newest;
                }
            }
        }
        if let Some(oldest) = rows.last() {
            self.before = Some(oldest.signature.clone());
        }
    }

    /// Ends the phase when a whole page came back with fewer than `limit` rows,
    /// and answers whether it did.
    ///
    /// The caller passes only whole pages, because a page it cut short still has
    /// rows below it.
    fn end_phase_if_short(&mut self, row_count: usize, limit: u32) -> bool {
        if u32::try_from(row_count).unwrap_or(u32::MAX) >= limit {
            return false;
        }
        match self.phase {
            Phase::Forward => {
                // The walk reached the frontier, so everything between it and the
                // newest signature seen is now accounted for.
                if let Some(found) = self.pending_newest.take() {
                    self.cursor.newest = Some(found);
                }
                self.cursor.forward_before = None;
                self.cursor.forward_newest = None;
                self.phase =
                    if self.cursor.is_backfill_complete { Phase::Done } else { Phase::Backfill };
                self.before = self.cursor.backfill_before.clone();
                true
            }
            Phase::Backfill => {
                self.cursor.is_backfill_complete = true;
                self.phase = Phase::Done;
                true
            }
            Phase::Done => false,
        }
    }

    fn into_cursor(mut self) -> SignatureCursor {
        match self.phase {
            Phase::Forward => {
                self.cursor.forward_before = self.before;
                self.cursor.forward_newest = self.pending_newest;
            }
            Phase::Backfill => {
                self.cursor.backfill_before = self.before;
            }
            Phase::Done => {}
        }
        self.cursor
    }
}

/// One account's walk in progress.
///
/// A state machine rather than a loop, so a caller can advance every walk by one
/// page per batched round trip. A pass then costs as many round trips as the
/// deepest walk rather than the sum of them.
#[derive(Debug, Clone)]
pub(crate) struct Walk {
    /// Where this pass started, which [`Walk::finish`] replays the pages from.
    start: SignatureCursor,
    position: Position,
    /// Pages the current phase has used this pass.
    pages: u32,
    limit: u32,
    budget: u32,
    found: Vec<SignatureRow>,
    page_lengths: Vec<usize>,
}

impl Walk {
    pub(crate) fn new(cursor: SignatureCursor, config: &SolanaConfig) -> Self {
        Self {
            start: cursor.clone(),
            position: Position::new(cursor),
            pages: 0,
            limit: config.page_size(),
            budget: config.pages_per_pass(),
            found: Vec::new(),
            page_lengths: Vec::new(),
        }
    }

    /// The next page to ask for, or `None` when this walk is finished or out of
    /// budget for this pass.
    pub(crate) fn next_page(&self) -> Option<PageRequest> {
        if self.pages >= self.budget {
            return None;
        }
        self.position.request(self.limit)
    }

    /// Takes the answer to the page [`Walk::next_page`] asked for.
    pub(crate) fn accept(&mut self, rows: Vec<SignatureRow>) {
        if self.position.phase == Phase::Done {
            return;
        }
        self.pages = self.pages.saturating_add(1);
        self.position.advance(&rows);
        if self.position.end_phase_if_short(rows.len(), self.limit) {
            self.pages = 0;
        }
        self.page_lengths.push(rows.len());
        self.found.extend(rows);
    }

    /// The signatures found this pass, newest first.
    ///
    /// One signature can appear in two walks, so the caller deduplicates across
    /// them. Pages within one walk never overlap, because each starts below the
    /// last one's oldest row and the two phases cover disjoint halves of the
    /// history.
    pub(crate) fn rows(&self) -> &[SignatureRow] {
        &self.found
    }

    /// The cursor to persist once the first `recorded` of [`Walk::rows`] have
    /// been turned into records.
    ///
    /// A pass that ran out of time records only some of what it found, so the
    /// pages are replayed over those rows alone and the walk resumes below the
    /// last one recorded.
    pub(crate) fn finish(self, recorded: usize) -> SignatureCursor {
        let mut position = Position::new(self.start);
        let mut rest = self.found.as_slice();
        let mut left = recorded;
        for length in self.page_lengths {
            let Some((page, after)) = rest.split_at_checked(length) else { break };
            if let Some(remaining) = left.checked_sub(length) {
                position.advance(page);
                position.end_phase_if_short(page.len(), self.limit);
                left = remaining;
                rest = after;
            } else {
                position.advance(page.get(..left).unwrap_or_default());
                break;
            }
        }
        position.into_cursor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ledger of signatures, newest first, that answers pages the way the
    /// protocol does.
    struct Ledger {
        rows: Vec<SignatureRow>,
    }

    impl Ledger {
        fn new(count: usize) -> Self {
            let rows = (0..count)
                .map(|i| SignatureRow { signature: format!("sig{i:05}"), slot: 0 })
                .collect();
            Self { rows }
        }

        fn prepend(&mut self, count: usize) {
            let mut fresh: Vec<SignatureRow> = (0..count)
                .map(|i| SignatureRow { signature: format!("new{i:05}"), slot: 0 })
                .collect();
            fresh.append(&mut self.rows);
            self.rows = fresh;
        }

        fn page(&self, request: &PageRequest) -> Vec<SignatureRow> {
            let start = match request.before.as_deref() {
                None => 0,
                Some(marker) => self
                    .rows
                    .iter()
                    .position(|row| row.signature == marker)
                    .map_or(0, |at| at.saturating_add(1)),
            };
            let mut rows = Vec::new();
            for row in self.rows.iter().skip(start) {
                if Some(row.signature.as_str()) == request.until.as_deref() {
                    break;
                }
                if u32::try_from(rows.len()).unwrap_or(u32::MAX) >= request.limit {
                    break;
                }
                rows.push(row.clone());
            }
            rows
        }
    }

    fn config() -> SolanaConfig {
        SolanaConfig { signature_page_size: 4, page_budget: 2, ..SolanaConfig::default() }
    }

    fn pass(
        ledger: &Ledger,
        cursor: SignatureCursor,
        config: &SolanaConfig,
    ) -> (SignatureCursor, Vec<String>, u32) {
        let mut walk = Walk::new(cursor, config);
        let mut pages = 0u32;
        while let Some(request) = walk.next_page() {
            walk.accept(ledger.page(&request));
            pages = pages.saturating_add(1);
        }
        let found: Vec<String> = walk.rows().iter().map(|row| row.signature.clone()).collect();
        (walk.finish(found.len()), found, pages)
    }

    /// A pass that records only the first `recorded` rows it found.
    fn cut_short(
        ledger: &Ledger,
        cursor: SignatureCursor,
        config: &SolanaConfig,
        recorded: usize,
    ) -> (SignatureCursor, Vec<String>) {
        let mut walk = Walk::new(cursor, config);
        while let Some(request) = walk.next_page() {
            walk.accept(ledger.page(&request));
        }
        let found = walk.rows().iter().take(recorded).map(|row| row.signature.clone()).collect();
        (walk.finish(recorded), found)
    }

    #[test]
    fn a_first_pass_over_a_short_history_finishes_and_seeds_the_frontier() {
        let ledger = Ledger::new(4);
        let (cursor, found, pages) = pass(&ledger, SignatureCursor::default(), &config());
        assert_eq!(pages, 2, "one full page, then the short page that ends the walk");
        assert!(cursor.is_backfill_complete);
        assert!(cursor.is_complete());
        assert_eq!(cursor.newest.as_deref(), Some("sig00000"));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn repeated_passes_cover_the_whole_history_with_no_gap_and_no_repeat() {
        let config = config();
        let ledger = Ledger::new(30);
        let mut cursor = SignatureCursor::default();
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..40u8 {
            let (next, found, _) = pass(&ledger, cursor, &config);
            seen.extend(found);
            cursor = next;
            if cursor.is_complete() {
                break;
            }
        }
        assert!(cursor.is_backfill_complete, "the backfill must finish");
        assert_eq!(seen.len(), 30, "no signature is walked twice: {seen:?}");
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 30, "every signature exactly once: {seen:?}");
        for i in 0..30usize {
            assert!(unique.contains(&format!("sig{i:05}")), "{i}");
        }
    }

    #[test]
    fn a_forward_walk_that_runs_out_of_budget_keeps_the_frontier_until_it_finishes() {
        let config = config();
        let mut ledger = Ledger::new(4);
        let (settled, _, _) = pass(&ledger, SignatureCursor::default(), &config);
        assert_eq!(settled.newest.as_deref(), Some("sig00000"));

        // Twenty new signatures, more than one pass of two pages of four.
        ledger.prepend(20);
        let mut cursor = settled;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..20u8 {
            let (next, found, _) = pass(&ledger, cursor, &config);
            seen.extend(found);
            cursor = next;
            if !cursor.is_forward_in_progress() {
                break;
            }
            assert_eq!(
                cursor.newest.as_deref(),
                Some("sig00000"),
                "the frontier must not move while the walk is unfinished"
            );
            assert!(cursor.forward_newest.is_some(), "the newest seen is remembered");
        }
        assert_eq!(cursor.newest.as_deref(), Some("new00000"));
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 20, "{seen:?}");
    }

    #[test]
    fn a_quiet_address_asks_one_page_and_changes_nothing() {
        let config = config();
        let ledger = Ledger::new(4);
        let (settled, _, _) = pass(&ledger, SignatureCursor::default(), &config);
        let before = settled.clone();
        let (after, found, pages) = pass(&ledger, settled, &config);
        assert_eq!(pages, 1, "the forward walk terminates on its first short page");
        assert!(found.is_empty());
        assert_eq!(after, before);
    }

    #[test]
    fn an_empty_history_is_a_complete_walk() {
        let ledger = Ledger::new(0);
        let (cursor, found, pages) = pass(&ledger, SignatureCursor::default(), &config());
        assert_eq!(pages, 1);
        assert!(found.is_empty());
        assert!(cursor.is_complete());
        assert_eq!(cursor.newest, None, "there is no frontier to seed");
    }

    #[test]
    fn a_cursor_round_trips_through_its_stored_form() {
        let cursor = SignatureCursor {
            newest: Some(String::from("a")),
            forward_before: Some(String::from("b")),
            forward_newest: Some(String::from("c")),
            backfill_before: Some(String::from("d")),
            is_backfill_complete: true,
        };
        let json = serde_json::to_string(&cursor).expect("it encodes");
        assert_eq!(serde_json::from_str::<SignatureCursor>(&json).expect("it decodes"), cursor);
        let bare = serde_json::to_string(&SignatureCursor::default()).expect("it encodes");
        assert_eq!(bare, r#"{"backfillComplete":false}"#);
    }

    /// A shared budget would spend its only page on the forward walk of a quiet
    /// address and never reach the backfill, so the history would never fill in.
    #[test]
    fn a_budget_of_one_page_still_advances_the_backfill() {
        let config =
            SolanaConfig { signature_page_size: 2, page_budget: 1, ..SolanaConfig::default() };
        let ledger = Ledger::new(10);
        let mut cursor = SignatureCursor::default();
        let mut passes = 0u8;
        for _ in 0..20u8 {
            let (next, _, _) = pass(&ledger, cursor, &config);
            cursor = next;
            passes = passes.saturating_add(1);
            if cursor.is_complete() {
                break;
            }
        }
        assert!(cursor.is_backfill_complete, "after {passes} passes");
        assert_eq!(cursor.newest.as_deref(), Some("sig00000"));
    }

    /// A new address's first pass finds nothing, so it has no frontier, and its
    /// forward walk asks for everything that has arrived since.
    #[test]
    fn the_walk_of_a_completed_address_with_no_frontier_still_looks_forward() {
        let cursor = SignatureCursor { is_backfill_complete: true, ..SignatureCursor::default() };
        assert_eq!(
            Walk::new(cursor, &config()).next_page(),
            Some(PageRequest { limit: 4, before: None, until: None })
        );
    }

    #[test]
    fn history_that_arrives_after_an_empty_first_pass_is_walked_once() {
        let config = config();
        let mut ledger = Ledger::new(0);
        let (empty, _, _) = pass(&ledger, SignatureCursor::default(), &config);
        assert!(empty.is_complete());

        ledger.prepend(10);
        let mut cursor = empty;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..10u8 {
            let (next, found, _) = pass(&ledger, cursor, &config);
            seen.extend(found);
            cursor = next;
            if !cursor.is_forward_in_progress() {
                break;
            }
        }
        assert_eq!(cursor.newest.as_deref(), Some("new00000"));
        let (quiet, found, pages) = pass(&ledger, cursor.clone(), &config);
        assert_eq!((found.len(), pages), (0, 1));
        assert_eq!(quiet, cursor);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 10, "{seen:?}");
    }

    /// A pass that ran out of time recorded some of what it found, and the next
    /// one picks up below the last row it recorded.
    #[test]
    fn a_pass_cut_short_resumes_below_the_last_recorded_row() {
        let config = config();
        let ledger = Ledger::new(30);
        let mut cursor = SignatureCursor::default();
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..40u8 {
            let (next, found) = cut_short(&ledger, cursor, &config, 3);
            seen.extend(found);
            cursor = next;
            if cursor.is_complete() {
                break;
            }
        }
        assert!(cursor.is_complete());
        assert_eq!(seen.len(), 30, "no row twice: {seen:?}");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 30, "every row once: {seen:?}");
    }

    #[test]
    fn a_pass_that_recorded_nothing_keeps_its_cursor() {
        let config = config();
        let ledger = Ledger::new(30);
        let (fresh, _) = cut_short(&ledger, SignatureCursor::default(), &config, 0);
        assert_eq!(fresh, SignatureCursor::default());

        let (settled, _, _) = pass(&Ledger::new(4), SignatureCursor::default(), &config);
        let mut grown = Ledger::new(4);
        grown.prepend(20);
        let (unchanged, _) = cut_short(&grown, settled.clone(), &config, 0);
        assert_eq!(unchanged, settled);
    }
}
