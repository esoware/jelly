use std::collections::BTreeMap;

use jelly_core::{ChainId, ChainSyncProgress};
use parking_lot::Mutex;

/// What the board holds for one chain. The heights stay inside the engine, where history
/// counts confirmations from them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChainHeights {
    pub(crate) synced_height: Option<u64>,
    pub(crate) tip_height: Option<u64>,
}

/// The one place a chain's heights live.
#[derive(Debug, Default)]
pub(crate) struct StatusBoard {
    chains: Mutex<BTreeMap<ChainId, ChainHeights>>,
}

impl StatusBoard {
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn heights(&self, chain: ChainId) -> Option<ChainHeights> {
        self.chains.lock().get(&chain).copied()
    }

    /// Every chain's cached tip in one lock, for a page of history that needs a
    /// confirmation count per row.
    ///
    /// The seeded synced height stands in until the first pass reports a real tip.
    /// Without it, a freshly unlocked wallet's history would read every confirmed
    /// transaction as one block deep, however old it is.
    pub(crate) fn tips(&self) -> BTreeMap<ChainId, u64> {
        self.chains
            .lock()
            .iter()
            .filter_map(|(&chain, heights)| {
                heights.tip_height.or(heights.synced_height).map(|tip| (chain, tip))
            })
            .collect()
    }

    pub(crate) fn clear(&self) {
        self.chains.lock().clear();
    }

    /// Applies a persisted height so a freshly unlocked wallet shows where the
    /// last session got to, before the first pass of this one finishes.
    pub(crate) fn seed_height(&self, chain: ChainId, synced: u64) {
        self.update(chain, |heights| {
            if heights.synced_height.is_none() {
                heights.synced_height = Some(synced);
            }
        });
    }

    pub(crate) fn record_success(&self, chain: ChainId, progress: ChainSyncProgress) {
        self.update(chain, |heights| {
            heights.synced_height = progress.synced_height();
            heights.tip_height = progress.tip_height();
        });
    }

    /// A missing height is unobserved rather than cleared, so the previous one stays
    /// until the adapter supplies a new one.
    pub(crate) fn record_progress(&self, progress: ChainSyncProgress) {
        self.update(progress.chain(), |heights| {
            if progress.synced_height().is_some() {
                heights.synced_height = progress.synced_height();
            }
            if progress.tip_height().is_some() {
                heights.tip_height = progress.tip_height();
            }
        });
    }

    fn update(&self, chain: ChainId, change: impl FnOnce(&mut ChainHeights)) {
        change(self.chains.lock().entry(chain).or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_success_sets_the_tip_history_counts_from() {
        let board = StatusBoard::default();
        let progress = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(Some(10), Some(12));
        board.record_success(ChainId::Bitcoin, progress);
        assert_eq!(board.tips().get(&ChainId::Bitcoin).copied(), Some(12));
    }

    #[test]
    fn a_seeded_height_stands_in_for_the_tip_until_a_pass_reports_one() {
        let board = StatusBoard::default();
        board.seed_height(ChainId::Bitcoin, 90);
        assert_eq!(board.tips().get(&ChainId::Bitcoin).copied(), Some(90));

        let progress = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(Some(95), Some(96));
        board.record_success(ChainId::Bitcoin, progress);
        assert_eq!(board.tips().get(&ChainId::Bitcoin).copied(), Some(96));
    }

    /// An Electrum pass reports no synced height until it finishes, so clearing it
    /// would leave the board without one for most of the session and erase the seeded
    /// one as soon as the first pass starts.
    #[test]
    fn progress_that_states_no_height_leaves_the_last_one_standing() {
        let board = StatusBoard::default();
        board.seed_height(ChainId::Bitcoin, 800_000);

        let partway = ChainSyncProgress::new(ChainId::Bitcoin)
            .with_progress(500)
            .with_heights(None, Some(800_010));
        board.record_progress(partway);

        let found = board.heights(ChainId::Bitcoin).expect("the heights");
        assert_eq!(found.synced_height, Some(800_000), "the seeded height survives");
        assert_eq!(found.tip_height, Some(800_010));

        // A pass that states a height still decides it, even downwards, because a
        // rescan from a birthday is lower than the height before it.
        board.record_progress(
            ChainSyncProgress::new(ChainId::Bitcoin).with_heights(Some(700_000), Some(800_011)),
        );
        let found = board.heights(ChainId::Bitcoin).expect("the heights");
        assert_eq!(found.synced_height, Some(700_000));
        assert_eq!(found.tip_height, Some(800_011));
    }

    #[test]
    fn clearing_forgets_every_height() {
        let board = StatusBoard::default();
        board.seed_height(ChainId::Bitcoin, 90);
        board.clear();
        assert!(board.tips().is_empty());
    }
}
