use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::mem;

use jelly_core::{Amount, Clock};
use monero_wallet::WalletOutput;
use monero_wallet::transaction::Timelock;

use crate::balance::MoneroBalance;
use crate::error::MoneroError;
use crate::params::{LOCK_WINDOW, REORG_WINDOW};
use crate::pending::{InFlight, TerminalSend};
use crate::subaddress::Subaddress;

/// Wall-clock seconds, which is what an output's own timelock is measured in.
///
/// Fails closed, because a clock that cannot be read reads as the epoch and an
/// output whose timelock has not been reached is not spendable. Every other lock
/// on this chain is counted in blocks, so this only concerns a transaction that
/// set a time.
pub(crate) fn timelock_now_secs(clock: &dyn Clock) -> u64 {
    clock.now_secs().ok().and_then(|secs| u64::try_from(secs).ok()).unwrap_or_default()
}

/// Where one of our outputs was consumed, as observed on chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Spend {
    pub txid: [u8; 32],
    pub height: u64,
}

/// A block this wallet has scanned, kept so a reorganisation can be located
/// exactly rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockRef {
    pub height: u64,
    pub hash: [u8; 32],
}

/// One block as the chain check sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockLink {
    pub height: u64,
    pub hash: [u8; 32],
    pub previous: [u8; 32],
}

/// What recording a scanned output did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recorded {
    /// An output this wallet had not seen. It counts as a receipt.
    Fresh,
    /// The same output again, from a block that was re-scanned. Re-scanning is
    /// idempotent, so this is silent.
    Again,
    /// A different output with an output key already seen, which is the burning
    /// bug. Only one of them is spendable, so the later one is refused rather
    /// than allowed to inflate the balance.
    Burnt,
}

/// An output belonging to this account, plus the bookkeeping the wallet keeps on
/// top of what the scanner recovers.
pub(crate) struct OwnedOutput {
    output: WalletOutput,
    height: u64,
    timestamp_secs: u64,
    key_image: [u8; 32],
    spend: Option<Spend>,
}

impl fmt::Debug for OwnedOutput {
    /// Where the output came from and whether it is spent, but neither its key
    /// offset nor its key image.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedOutput")
            .field("transaction", &hex::encode(self.output.transaction()))
            .field("index_in_transaction", &self.output.index_in_transaction())
            .field("height", &self.height)
            .field("is_spent", &self.spend.is_some())
            .finish_non_exhaustive()
    }
}

impl OwnedOutput {
    pub(crate) const fn new(
        output: WalletOutput,
        height: u64,
        timestamp_secs: u64,
        key_image: [u8; 32],
        spend: Option<Spend>,
    ) -> Self {
        Self { output, height, timestamp_secs, key_image, spend }
    }

    pub(crate) const fn output(&self) -> &WalletOutput {
        &self.output
    }

    pub(crate) const fn height(&self) -> u64 {
        self.height
    }

    pub(crate) const fn timestamp_secs(&self) -> u64 {
        self.timestamp_secs
    }

    pub(crate) const fn key_image(&self) -> [u8; 32] {
        self.key_image
    }

    pub(crate) const fn spend(&self) -> Option<Spend> {
        self.spend
    }

    pub(crate) fn amount(&self) -> u64 {
        self.output.commitment().amount
    }

    /// The transaction that made the output and its index there. Two scans of the
    /// same block yield the same identity, which makes re-scanning idempotent.
    fn id(&self) -> ([u8; 32], u64) {
        (self.output.transaction(), self.output.index_in_transaction())
    }

    fn key(&self) -> [u8; 32] {
        self.output.key().compress().to_bytes()
    }

    /// The scanner reports the account's own primary address as `None`.
    pub(crate) fn subaddress(&self) -> Subaddress {
        self.output
            .subaddress()
            .map_or(Subaddress::PRIMARY, |index| Subaddress::new(index.account(), index.address()))
    }

    /// wallet2 spends an output once it has [`LOCK_WINDOW`] confirmations, plus
    /// any additional timelock the transaction carried. A miner's output carries
    /// its sixty-block lock as exactly such a timelock, so this covers it.
    ///
    /// Whether the output is spent is a separate question this does not ask.
    fn is_unlocked(&self, tip: u64, now_secs: u64) -> bool {
        if !is_unlocked_at(self.height, tip) {
            return false;
        }
        match self.output.additional_timelock() {
            Timelock::None => true,
            Timelock::Block(block) => {
                u64::try_from(block).is_ok_and(|block| block <= tip.saturating_add(1))
            }
            Timelock::Time(time) => time <= now_secs,
        }
    }
}

/// The ten-block lock in isolation.
const fn is_unlocked_at(output_height: u64, tip: u64) -> bool {
    tip.saturating_add(1) >= output_height.saturating_add(LOCK_WINDOW)
}

/// A window wide enough to cover `wanted`, and at least `step` wider than it was.
const fn widened(current: u32, wanted: u32, step: u32) -> u32 {
    let needed = wanted.saturating_add(1);
    let stepped = current.saturating_add(step);
    if needed > stepped { needed } else { stepped }
}

/// Everything this adapter knows after scanning.
///
/// Two derived indexes and two dedup tables sit beside the output vector, so a
/// spend lookup and a balance both touch only what they need. Every table is
/// rebuilt from the vector by [`ScanState::reindex`], so the vector is the only
/// thing a restore has to be sure of, and [`ScanState::with_scanned`] checks that
/// it holds no output twice.
pub(crate) struct ScanState {
    public_spend: [u8; 32],
    restore_height: u64,
    next_height: u64,
    tip_height: u64,
    lookahead_major: u32,
    lookahead_minor: u32,
    outputs: Vec<OwnedOutput>,
    /// Key image to a position in `outputs`, so spend detection is a lookup.
    by_key_image: HashMap<[u8; 32], usize>,
    /// Subaddress to the positions it owns, so a balance touches only those.
    by_subaddress: BTreeMap<Subaddress, Vec<usize>>,
    /// Output identity to a position, which is what makes a re-scan idempotent.
    by_id: HashMap<([u8; 32], u64), usize>,
    /// Output key to a position. A second output under a key already seen is the
    /// burning bug.
    by_key: HashMap<[u8; 32], usize>,
    /// The most recent block ids, oldest first, at most [`REORG_WINDOW`] of them.
    recent_blocks: VecDeque<BlockRef>,
    /// Broadcast transactions whose fate is unknown, by transaction id.
    in_flight_sends: BTreeMap<[u8; 32], InFlight>,
    pub(crate) terminal_sends: Vec<TerminalSend>,
    /// The ring each output was offered with, as the positions of its members on the
    /// blockchain in ascending order, by the output's key image.
    ///
    /// Input selection is deterministic, so a rebuild after a refusal or an abandoned
    /// build spends the same outputs, and two different rings around one output give
    /// the real member away to anyone who sees both. Like the sends in flight, these
    /// outlive a rescan.
    pub(crate) rings: BTreeMap<[u8; 32], Vec<u64>>,
    pub(crate) settled_through: u64,
    /// Bumped by every reset and never persisted, so a pass that read the
    /// frontier before a reset can tell its batch was scanned for a frontier and a
    /// window that no longer exist.
    generation: u64,
}

impl fmt::Debug for ScanState {
    /// Heights and counts, and the sends in flight by transaction id, but no
    /// output, key image, or signed byte.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanState")
            .field("restore_height", &self.restore_height)
            .field("next_height", &self.next_height)
            .field("tip_height", &self.tip_height)
            .field("lookahead", &self.lookahead())
            .field("outputs", &self.outputs.len())
            .field("recent_blocks", &self.recent_blocks.len())
            .field("in_flight_sends", &self.in_flight_sends.values())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl ScanState {
    pub(crate) fn new(public_spend: [u8; 32], restore_height: u64, lookahead: (u32, u32)) -> Self {
        Self {
            public_spend,
            restore_height,
            next_height: restore_height,
            tip_height: 0,
            lookahead_major: lookahead.0,
            lookahead_minor: lookahead.1,
            outputs: Vec::new(),
            by_key_image: HashMap::new(),
            by_subaddress: BTreeMap::new(),
            by_id: HashMap::new(),
            by_key: HashMap::new(),
            recent_blocks: VecDeque::new(),
            in_flight_sends: BTreeMap::new(),
            terminal_sends: Vec::new(),
            rings: BTreeMap::new(),
            settled_through: 0,
            generation: 0,
        }
    }

    pub(crate) const fn public_spend(&self) -> [u8; 32] {
        self.public_spend
    }

    pub(crate) const fn restore_height(&self) -> u64 {
        self.restore_height
    }

    pub(crate) const fn next_height(&self) -> u64 {
        self.next_height
    }

    /// Moves the frontier forward only. A batch that re-scanned blocks below the
    /// frontier must not drag it backwards.
    pub(crate) const fn advance_to(&mut self, height: u64) {
        if height > self.next_height {
            self.next_height = height;
        }
    }

    pub(crate) const fn tip_height(&self) -> u64 {
        self.tip_height
    }

    pub(crate) const fn set_tip_height(&mut self, height: u64) {
        self.tip_height = height;
    }

    pub(crate) const fn lookahead(&self) -> (u32, u32) {
        (self.lookahead_major, self.lookahead_minor)
    }

    pub(crate) fn outputs(&self) -> &[OwnedOutput] {
        &self.outputs
    }

    pub(crate) fn recent_blocks(&self) -> impl Iterator<Item = &BlockRef> {
        self.recent_blocks.iter()
    }

    pub(crate) fn in_flight_sends(&self) -> impl Iterator<Item = &InFlight> {
        self.in_flight_sends.values()
    }

    pub(crate) fn in_flight_count(&self) -> usize {
        self.in_flight_sends.len()
    }

    /// Takes `committed` as this state's scan, keeping the sends and rings, which
    /// outlive any rescan, and says whether it acknowledged a terminal send. That
    /// takes a settlement checkpoint at or past the send's and, for a send that
    /// landed, the spend itself.
    pub(crate) fn replace_scan(&mut self, mut committed: Self) -> bool {
        let before = self.terminal_sends.len();
        self.terminal_sends.retain(|send| {
            send.checkpoint > committed.settled_through
                || (!send.is_failed && !committed.has_spend(send.txid))
        });
        let has_acknowledged = before != self.terminal_sends.len();
        committed.in_flight_sends = mem::take(&mut self.in_flight_sends);
        committed.terminal_sends = mem::take(&mut self.terminal_sends);
        committed.rings = mem::take(&mut self.rings);
        committed.generation = self.generation;
        *self = committed;
        has_acknowledged
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// The height already scanned, never below the wallet birthday.
    pub(crate) const fn synced_height(&self) -> u64 {
        if self.next_height > self.restore_height {
            self.next_height.saturating_sub(1)
        } else {
            self.restore_height
        }
    }

    /// Widens the registered window to cover `wanted`, by at least `step` so a
    /// run of new subaddresses costs one rescan rather than one each.
    ///
    /// Returns true when the window grew, which is the only thing that forces a
    /// rescan, because an index already inside the window was visible to every
    /// block already scanned.
    pub(crate) const fn widen(&mut self, wanted: Subaddress, step: u32) -> bool {
        let has_major_grown = wanted.major >= self.lookahead_major;
        if has_major_grown {
            self.lookahead_major = widened(self.lookahead_major, wanted.major, step);
        }
        let has_minor_grown = wanted.minor >= self.lookahead_minor;
        if has_minor_grown {
            self.lookahead_minor = widened(self.lookahead_minor, wanted.minor, step);
        }
        has_major_grown || has_minor_grown
    }

    /// Discards scan progress but keeps the birthday, the window, and anything in
    /// flight, because a rescan does not undo a broadcast transaction.
    pub(crate) fn reset_scan(&mut self) {
        self.next_height = self.restore_height;
        self.outputs.clear();
        self.recent_blocks.clear();
        self.reindex();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Sets the birthday. A lower birthday uncovers blocks that were never scanned,
    /// and an output found there may be spent in a block already scanned, so the
    /// scan starts again from it. A higher one keeps everything already scanned and
    /// only moves a frontier that is still below it.
    pub(crate) fn set_restore_height(&mut self, height: u64) {
        if height < self.restore_height {
            self.restore_height = height;
            self.reset_scan();
        } else if height > self.restore_height {
            self.restore_height = height;
            self.advance_to(height);
        }
    }

    /// The first height at which these blocks disagree with what was scanned, if
    /// any. That height is the fork point.
    pub(crate) fn fork_in(&self, links: &[BlockLink]) -> Option<u64> {
        for link in links {
            if let Some(known) = self.hash_at(link.height) {
                if known == link.hash {
                    continue;
                }
                return Some(link.height);
            }
            if let Some(parent_height) = link.height.checked_sub(1)
                && let Some(parent) = self.hash_at(parent_height)
                && parent != link.previous
            {
                return Some(parent_height);
            }
        }
        None
    }

    fn hash_at(&self, height: u64) -> Option<[u8; 32]> {
        self.recent_blocks.iter().find(|block| block.height == height).map(|block| block.hash)
    }

    /// Whether a block below `height` is remembered. A fork found at `height`
    /// with nothing remembered under it may reach any depth, so it cannot be
    /// located.
    pub(crate) fn has_block_below(&self, height: u64) -> bool {
        self.recent_blocks.front().is_some_and(|oldest| oldest.height < height)
    }

    /// Drops everything at or above `height` and resumes the scan there.
    ///
    /// In-flight sends survive, because their transactions are still valid and
    /// their inputs must stay reserved until a spend is observed. The frontier
    /// never moves forward here, so a reset that landed between locating a fork
    /// and rewinding to it is kept.
    pub(crate) fn rewind_to(&mut self, height: u64) {
        let floor = height.max(self.restore_height);
        self.outputs.retain(|output| output.height < floor);
        for output in &mut self.outputs {
            if output.spend.is_some_and(|spend| spend.height >= floor) {
                output.spend = None;
            }
        }
        self.recent_blocks.retain(|block| block.height < floor);
        self.next_height = self.next_height.min(floor);
        self.reindex();
    }

    /// Records a scanned block id, keeping only the window a reorganisation can
    /// be located within.
    pub(crate) fn note_block(&mut self, block: BlockRef) {
        if self.recent_blocks.back().is_some_and(|last| last.height >= block.height) {
            self.recent_blocks.retain(|known| known.height < block.height);
        }
        self.recent_blocks.push_back(block);
        while u64::try_from(self.recent_blocks.len()).unwrap_or(u64::MAX) > REORG_WINDOW {
            self.recent_blocks.pop_front();
        }
    }

    pub(crate) fn record_output(&mut self, output: OwnedOutput) -> Recorded {
        if self.by_id.contains_key(&output.id()) {
            return Recorded::Again;
        }
        if self.by_key.contains_key(&output.key()) {
            return Recorded::Burnt;
        }
        let position = self.outputs.len();
        self.outputs.push(output);
        self.index_at(position);
        Recorded::Fresh
    }

    /// Marks the output with this key image spent and returns its position, or
    /// `None` when this wallet does not own it or has already seen the spend.
    pub(crate) fn mark_spent(
        &mut self,
        key_image: &[u8; 32],
        txid: [u8; 32],
        height: u64,
    ) -> Option<usize> {
        let index = *self.by_key_image.get(key_image)?;
        let output = self.outputs.get_mut(index)?;
        if output.spend.is_some() {
            return None;
        }
        output.spend = Some(Spend { txid, height });
        Some(index)
    }

    /// Records a broadcast transaction. Its inputs are reserved from here until
    /// its fate is known.
    pub(crate) fn track(&mut self, send: InFlight) {
        self.in_flight_sends.insert(send.txid, send);
    }

    pub(crate) fn forget(&mut self, txid: &[u8; 32]) {
        self.in_flight_sends.remove(txid);
    }

    pub(crate) fn retire_landed(&mut self, txid: &[u8; 32]) -> Result<(), MoneroError> {
        self.retire(txid, false)
    }

    pub(crate) fn retire_failed(&mut self, txid: &[u8; 32]) -> Result<(), MoneroError> {
        self.retire(txid, true)
    }

    /// Moves a send out of flight into the terminal sends, which replay its record
    /// until a committed scan covers its checkpoint.
    fn retire(&mut self, txid: &[u8; 32], is_failed: bool) -> Result<(), MoneroError> {
        let checkpoint = self
            .terminal_sends
            .iter()
            .map(|send| send.checkpoint)
            .fold(self.settled_through, u64::max)
            .checked_add(1)
            .ok_or(MoneroError::Internal { context: "monero settlement checkpoint exhausted" })?;
        if let Some(send) = self.in_flight_sends.remove(txid) {
            self.terminal_sends.push(TerminalSend {
                txid: send.txid,
                subaddress: send.subaddress,
                amount_piconero: send.amount_piconero,
                recipient: send.recipient.clone(),
                checkpoint,
                is_failed,
            });
        }
        Ok(())
    }

    /// Keeps the ring each of `chosen` was offered with, and says whether any is new.
    pub(crate) fn remember_rings(&mut self, chosen: BTreeMap<[u8; 32], Vec<u64>>) -> bool {
        let mut has_changed = false;
        for (key_image, positions) in chosen {
            if self.rings.get(&key_image) != Some(&positions) {
                self.rings.insert(key_image, positions);
                has_changed = true;
            }
        }
        has_changed
    }

    /// Forgets the ring of every output spent deeper than a reorganisation reaches, which
    /// nothing can spend again, and says whether any went.
    pub(crate) fn prune_rings(&mut self) -> bool {
        let before = self.rings.len();
        let tip = self.tip_height;
        let by_key_image = &self.by_key_image;
        let outputs = &self.outputs;
        self.rings.retain(|key_image, _| {
            let spend = by_key_image.get(key_image).and_then(|index| outputs.get(*index)?.spend);
            spend.is_none_or(|spend| tip.saturating_sub(spend.height) < REORG_WINDOW)
        });
        self.rings.len() != before
    }

    pub(crate) fn has_spend(&self, txid: [u8; 32]) -> bool {
        self.outputs.iter().any(|output| output.spend.is_some_and(|spend| spend.txid == txid))
    }

    pub(crate) fn sent_from(&self, txid: &[u8; 32]) -> Option<Subaddress> {
        self.in_flight_sends.get(txid).map(|send| send.subaddress).or_else(|| {
            self.terminal_sends.iter().find(|send| send.txid == *txid).map(|send| send.subaddress)
        })
    }

    /// Key images committed to a transaction in flight.
    fn reserved_key_images(&self) -> HashSet<[u8; 32]> {
        self.in_flight_sends.values().flat_map(|send| send.key_images.iter().copied()).collect()
    }

    /// Where the output behind one key image was spent, when a scan has seen it.
    pub(crate) fn spend_of(&self, key_image: &[u8; 32]) -> Option<Spend> {
        let index = *self.by_key_image.get(key_image)?;
        self.outputs.get(index).and_then(OwnedOutput::spend)
    }

    /// Per-subaddress balances. Subaddresses with no outputs are absent.
    pub(crate) fn balances(&self, tip: u64, now_secs: u64) -> BTreeMap<Subaddress, MoneroBalance> {
        let reserved = self.reserved_key_images();
        self.by_subaddress
            .iter()
            .map(|(index, positions)| {
                (*index, self.balance_of(positions.iter().copied(), &reserved, tip, now_secs))
            })
            .collect()
    }

    /// The balance of every subaddress in one account. Monero spends within an
    /// account and returns change to it.
    pub(crate) fn account_balance(&self, major: u32, tip: u64, now_secs: u64) -> MoneroBalance {
        let positions = self
            .by_subaddress
            .iter()
            .filter(|(index, _)| index.major == major)
            .flat_map(|(_, positions)| positions.iter().copied());
        self.balance_of(positions, &self.reserved_key_images(), tip, now_secs)
    }

    fn balance_of(
        &self,
        positions: impl Iterator<Item = usize>,
        reserved_images: &HashSet<[u8; 32]>,
        tip: u64,
        now_secs: u64,
    ) -> MoneroBalance {
        let mut total: u128 = 0;
        let mut spendable: u128 = 0;
        let mut reserved: u128 = 0;
        for position in positions {
            let Some(output) = self.outputs.get(position) else { continue };
            let amount = u128::from(output.amount());
            if output.spend.is_some() {
                continue;
            }
            total = total.saturating_add(amount);
            if reserved_images.contains(&output.key_image) {
                reserved = reserved.saturating_add(amount);
            } else if output.is_unlocked(tip, now_secs) {
                spendable = spendable.saturating_add(amount);
            }
        }
        MoneroBalance {
            total: Amount::from_base_units(total),
            spendable: Amount::from_base_units(spendable),
            reserved: Amount::from_base_units(reserved),
        }
    }

    /// Unspent, unlocked, unreserved outputs of one account, largest first.
    ///
    /// Largest first minimises the input count, hence the weight and the fee. It
    /// is a weaker privacy heuristic than wallet2's randomised selection and is a
    /// deliberate trade for predictable fees. The tie-break on the output's index
    /// makes it deterministic, so a rebuild selects the same inputs and therefore
    /// conflicts with, rather than adds to, the transaction it replaces.
    pub(crate) fn spendable_outputs(
        &self,
        major: u32,
        tip: u64,
        now_secs: u64,
    ) -> Vec<&OwnedOutput> {
        let reserved = self.reserved_key_images();
        let mut chosen: Vec<&OwnedOutput> = self
            .by_subaddress
            .iter()
            .filter(|(index, _)| index.major == major)
            .flat_map(|(_, positions)| positions.iter().copied())
            .filter_map(|position| self.outputs.get(position))
            .filter(|output| output.spend.is_none())
            .filter(|output| output.is_unlocked(tip, now_secs))
            .filter(|output| !reserved.contains(&output.key_image))
            .collect();
        chosen.sort_unstable_by(|a, b| {
            b.amount()
                .cmp(&a.amount())
                .then_with(|| a.output.index_on_blockchain().cmp(&b.output.index_on_blockchain()))
        });
        chosen
    }

    /// Whether an output of account `major` that a send may draw on at the scan's
    /// tip was still locked at `height`.
    pub(crate) fn has_unlocked_since(&self, major: u32, height: u64, now_secs: u64) -> bool {
        height < self.tip_height
            && self
                .spendable_outputs(major, self.tip_height, now_secs)
                .iter()
                .any(|output| !output.is_unlocked(height, now_secs))
    }

    /// This state holding the blocks and outputs a restore read back, indexed once,
    /// or `None` when two of the outputs share an identity or an output key.
    ///
    /// [`ScanState::record_output`] refuses both while scanning, so only a restore
    /// can meet them, and a blob naming one output twice would count its amount
    /// twice.
    pub(crate) fn with_scanned(
        mut self,
        recent_blocks: VecDeque<BlockRef>,
        outputs: Vec<OwnedOutput>,
    ) -> Option<Self> {
        self.recent_blocks = recent_blocks;
        self.outputs = outputs;
        self.reindex();
        let is_distinct =
            self.by_id.len() == self.outputs.len() && self.by_key.len() == self.outputs.len();
        is_distinct.then_some(self)
    }

    /// Rebuilds every derived table from the output vector.
    fn reindex(&mut self) {
        self.by_key_image.clear();
        self.by_subaddress.clear();
        self.by_id.clear();
        self.by_key.clear();
        for position in 0..self.outputs.len() {
            self.index_at(position);
        }
    }

    /// Enters the output at `position` in every derived table.
    fn index_at(&mut self, position: usize) {
        let Some(output) = self.outputs.get(position) else { return };
        self.by_key_image.insert(output.key_image, position);
        self.by_subaddress.entry(output.subaddress()).or_default().push(position);
        self.by_id.insert(output.id(), position);
        self.by_key.insert(output.key(), position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{PUBLIC_SPEND, recorded_outputs};

    fn state() -> ScanState {
        ScanState::new(PUBLIC_SPEND, 100, (4, 4))
    }

    #[test]
    fn terminal_checkpoints_are_distinct_and_acknowledged_only_by_a_committed_scan() {
        let mut state = state();
        for marker in [1, 2, 3] {
            state.track(InFlight {
                txid: [marker; 32],
                key_images: Vec::new(),
                raw: vec![marker; 10],
                sent_at_height: 100,
                amount_piconero: 500,
                recipient: "4AAA".to_owned(),
                subaddress: Subaddress::new(0, u32::from(marker)),
                endpoint: "https://node.invalid:18081".to_owned(),
            });
        }
        state.retire_failed(&[1; 32]).unwrap();
        let mut earlier = ScanState::new(PUBLIC_SPEND, 100, (4, 4));
        earlier.settled_through = 1;
        state.retire_failed(&[2; 32]).unwrap();
        assert_eq!(
            state.terminal_sends.iter().map(|send| send.checkpoint).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(state.replace_scan(earlier));
        assert_eq!(state.terminal_sends.len(), 1);
        assert_eq!(state.sent_from(&[2; 32]), Some(Subaddress::new(0, 2)));
        assert_eq!(state.in_flight_count(), 1);
        state.reset_scan();
        assert_eq!(state.terminal_sends.len(), 1);
        assert_eq!(state.in_flight_count(), 1);
        state.retire_landed(&[3; 32]).unwrap();
        let mut reset = ScanState::new(PUBLIC_SPEND, 100, (4, 4));
        reset.settled_through = 3;
        assert!(state.replace_scan(reset));
        assert_eq!(
            state.terminal_sends.len(),
            1,
            "a reset watermark alone cannot acknowledge a spend"
        );
        assert_eq!(state.sent_from(&[3; 32]), Some(Subaddress::new(0, 3)));
    }

    #[test]
    fn a_ring_outlives_a_rescan_and_goes_once_its_spend_is_past_any_reorganisation() {
        let mut state = state();
        let outputs = recorded_outputs();
        let image = outputs.first().map(OwnedOutput::key_image).expect("a key image");
        for owned in outputs {
            state.record_output(owned);
        }
        let unknown = [0xee; 32];
        assert!(state.remember_rings(BTreeMap::from([(image, vec![1, 2, 3]), (unknown, vec![4])])));
        assert!(!state.remember_rings(BTreeMap::from([(image, vec![1, 2, 3])])), "nothing new");

        state.reset_scan();
        let committed = ScanState::new(PUBLIC_SPEND, 100, (4, 4));
        state.replace_scan(committed);
        assert_eq!(state.rings.len(), 2, "a rescan does not unsee a ring");
        for owned in recorded_outputs() {
            state.record_output(owned);
        }

        let tip = 1_000;
        state.set_tip_height(tip);
        assert!(!state.prune_rings(), "an unspent output keeps its ring");
        state.mark_spent(&image, [7; 32], tip - REORG_WINDOW + 1);
        assert!(!state.prune_rings(), "a reorganisation could still undo the spend");
        state.set_tip_height(tip + 1);
        assert!(state.prune_rings());
        assert_eq!(
            state.rings.keys().collect::<Vec<_>>(),
            [&unknown],
            "an unknown output keeps its ring"
        );
    }

    #[test]
    fn the_lock_window_boundary_is_ten_confirmations() {
        assert!(!is_unlocked_at(100, 100));
        assert!(!is_unlocked_at(100, 108));
        assert!(is_unlocked_at(100, 109));
        assert!(is_unlocked_at(100, 110));
    }

    #[test]
    fn widening_grows_by_at_least_the_step_so_a_run_costs_one_rescan() {
        let mut state = state();
        assert!(!state.widen(Subaddress::PRIMARY, 10), "already inside the window");
        assert!(state.widen(Subaddress::new(4, 0), 10));
        assert_eq!(state.lookahead(), (14, 4));
        assert!(!state.widen(Subaddress::new(13, 3), 10), "the step covered a whole run at once");
        assert!(!state.widen(Subaddress::new(2, 2), 10), "already covered");
    }

    #[test]
    fn a_lower_birthday_restarts_the_scan_and_a_higher_one_keeps_it() {
        let mut state = state();
        state.advance_to(5_000);
        state.set_restore_height(1_000);
        assert_eq!(state.restore_height(), 1_000);
        assert_eq!(state.next_height(), 5_000, "a higher birthday loses nothing");

        state.set_restore_height(50);
        assert_eq!(state.next_height(), 50, "a lower birthday uncovers a gap, so rescan");

        state.set_restore_height(2_000);
        assert_eq!(state.next_height(), 2_000, "the frontier skips what the birthday excludes");
    }

    #[test]
    fn the_chain_check_finds_the_exact_fork_height() {
        let mut state = state();
        for height in 100..110 {
            state.note_block(BlockRef { height, hash: [u8::try_from(height).unwrap_or(0); 32] });
        }
        let same = BlockLink { height: 105, hash: [105; 32], previous: [104; 32] };
        assert_eq!(state.fork_in(&[same]), None);

        let changed = BlockLink { height: 105, hash: [0xee; 32], previous: [104; 32] };
        assert_eq!(state.fork_in(&[changed]), Some(105));

        let orphaned = BlockLink { height: 110, hash: [110; 32], previous: [0xee; 32] };
        assert_eq!(state.fork_in(&[orphaned]), Some(109), "a different parent forks below");

        let unknown = BlockLink { height: 400, hash: [1; 32], previous: [2; 32] };
        assert_eq!(state.fork_in(&[unknown]), None, "nothing to compare against");
    }

    #[test]
    fn a_fork_at_the_oldest_block_remembered_has_nothing_under_it() {
        let mut state = state();
        assert!(!state.has_block_below(105), "nothing is remembered yet");
        for height in 100..110 {
            state.note_block(BlockRef { height, hash: [1; 32] });
        }
        assert!(state.has_block_below(101));
        assert!(!state.has_block_below(100), "the oldest block has nothing remembered under it");
    }

    #[test]
    fn the_recent_window_is_bounded_and_replaces_a_reorganised_height() {
        let mut state = state();
        for height in 0..(REORG_WINDOW.saturating_mul(2)) {
            state.note_block(BlockRef { height, hash: [1; 32] });
        }
        assert_eq!(u64::try_from(state.recent_blocks().count()).unwrap_or(0), REORG_WINDOW);

        state.note_block(BlockRef { height: 200, hash: [9; 32] });
        assert_eq!(state.hash_at(200), Some([9; 32]));
        assert_eq!(state.hash_at(201), None, "everything above it went");
    }

    #[test]
    fn a_debug_line_carries_no_key_offset_key_image_or_signed_byte() {
        let outputs = recorded_outputs();
        assert!(!outputs.is_empty(), "the fixture pays its account");
        let mut secrets = Vec::new();
        let mut key_images = Vec::new();
        for owned in &outputs {
            let key_image = owned.key_image();
            let key_offset = <[u8; 32]>::from(owned.output().key_offset());
            secrets.extend([
                hex::encode(key_image),
                format!("{key_image:?}"),
                hex::encode(key_offset),
                format!("{key_offset:?}"),
            ]);
            key_images.push(key_image);
        }
        let raw = vec![0xabu8; 64];
        secrets.push(format!("{raw:?}"));

        let mut state = state();
        for owned in outputs {
            assert_eq!(state.record_output(owned), Recorded::Fresh);
        }
        state.track(InFlight {
            txid: [0xcd; 32],
            key_images,
            raw,
            sent_at_height: 100,
            amount_piconero: 1_000,
            recipient: "4AAA".to_owned(),
            subaddress: Subaddress::PRIMARY,
            endpoint: "https://node.invalid:18081".to_owned(),
        });

        let rendered = format!("{state:?} {:?}", state.outputs());
        for secret in &secrets {
            assert!(!rendered.contains(secret.as_str()), "{rendered}");
        }
        assert!(!rendered.contains("171, 171"), "{rendered}");
        assert!(rendered.contains(&hex::encode([0xcd; 32])), "a send is named by its txid");
    }
}
