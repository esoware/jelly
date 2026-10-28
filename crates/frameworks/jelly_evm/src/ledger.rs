use std::mem;

use alloy::primitives::{Address, B256, Bytes};
use jelly_core::{Amount, AssetId, RetryPolicy};

use crate::error::EvmError;
use crate::fee::FeePair;
use crate::payload::Intent;
use crate::rpc::{Landing, Receipt};

const TARGET: &str = "jelly::evm::pending";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// Built, and holding its nonce slot so a second build can't claim it.
    /// Released by an explicit discard or by the timeout. Never stored, because a
    /// held slot means nothing after a restart.
    Held { since_millis: u64 },
    /// Signed bytes never expire, and an unsent signature is released only by an
    /// explicit discard. Once sent, it keeps its slot until a receipt, another
    /// transaction taking the nonce, or a definite refusal settles it, because a
    /// transport failure leaves the outcome unknown.
    Signed { envelope: Envelope, is_sent: bool },
}

/// A signed transaction's id and the exact bytes it was broadcast as, which are
/// resent while no node knows the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub(crate) txid: B256,
    pub(crate) raw: Bytes,
}

/// One nonce slot and the transaction that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tracked {
    pub(crate) from: Address,
    pub(crate) nonce: u64,
    pub(crate) asset: AssetId,
    pub(crate) amount: Amount,
    /// The recipient the user named, not a token contract. A record shows it as
    /// the counterparty.
    pub(crate) to: Address,
    /// The pair this transaction pays, from which a replacement's floor is
    /// derived.
    pub(crate) fee: FeePair,
    /// Calldata the user supplied in place of the transfer, which a replacement
    /// carries again so a speed-up stays the same call. Known from signing onward.
    pub(crate) calldata: Option<Bytes>,
    pub(crate) state: State,
    /// Set once a record has been emitted for the inclusion, and cleared when the
    /// inclusion is reorganized away, so the record is emitted again for the block
    /// it lands in next.
    pub(crate) landing: Option<Landing>,
}

impl Tracked {
    pub(crate) const fn held(intent: Intent, nonce: u64, fee: FeePair, now_millis: u64) -> Self {
        let Intent { from, asset, to, amount } = intent;
        Self {
            from,
            nonce,
            asset,
            amount,
            to,
            fee,
            calldata: None,
            state: State::Held { since_millis: now_millis },
            landing: None,
        }
    }

    /// Known from signing onward.
    pub(crate) const fn txid(&self) -> Option<B256> {
        match self.state {
            State::Held { .. } => None,
            State::Signed { ref envelope, .. } => Some(envelope.txid),
        }
    }

    pub(crate) const fn sent(&self) -> Option<&Envelope> {
        match self.state {
            State::Signed { ref envelope, is_sent: true } => Some(envelope),
            State::Held { .. } | State::Signed { is_sent: false, .. } => None,
        }
    }

    pub(crate) fn sent_bytes(&self) -> Option<&Bytes> {
        self.sent().map(|envelope| &envelope.raw)
    }

    pub(crate) const fn is_sent(&self) -> bool {
        matches!(self.state, State::Signed { is_sent: true, .. })
    }

    fn is_same_build(&self, other: &Self) -> bool {
        self.from == other.from && self.nonce == other.nonce && self.fee == other.fee
    }

    /// Works out what a receipt means for publishing, including a reorged
    /// inclusion. It changes nothing: [`Ledger::land`] records the result, and
    /// only once the records this produced are delivered.
    pub(crate) fn observe<'r>(&self, receipt: Option<&'r Receipt>) -> Observation<'r> {
        let found = receipt.and_then(|receipt| Some((receipt, receipt.landing()?)));
        match (self.landing, found) {
            (None, None) => Observation::StillPending,
            // A receipt that vanished means the inclusion was reorged away. The
            // transaction may still land, so the slot stays held and the record
            // is emitted again when it does.
            (Some(_), None) => Observation::Unlanded,
            (None, Some((receipt, landing))) => {
                Observation::Landed { receipt, landing, should_report: true }
            }
            (Some(previous), Some((receipt, landing))) => {
                Observation::Landed { receipt, landing, should_report: !previous.is_same(landing) }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub(crate) txid: B256,
    pub(crate) from: Address,
}

/// The nonce slots this adapter has committed to and the sends it's watching.
///
/// Only one transaction per nonce can confirm, so the nonce guards against
/// double sends. A sent slot stays held until a receipt or a definite refusal
/// settles it, because a transport failure proves nothing and a retry at a new
/// nonce could pay twice.
#[derive(Debug)]
pub(crate) struct Ledger {
    entries: Vec<Tracked>,
    settled: Vec<Settled>,
    /// Released slots below one the same sender still holds. A send above a
    /// missing nonce never gets mined, so the next build fills the lowest of them.
    given_back: Vec<(Address, u64)>,
}

impl Ledger {
    pub(crate) const fn new() -> Self {
        Self { entries: Vec::new(), settled: Vec::new(), given_back: Vec::new() }
    }

    pub(crate) const fn len(&self) -> usize {
        self.entries.len()
    }

    /// The node's pending count covers its executable pending transactions but
    /// misses slots this adapter holds without sending, so the first free slot is
    /// past both. A released slot below a held one comes first, because the
    /// node's count stops below that gap.
    ///
    /// Only a slot this ledger released counts as a gap. A node that trails the
    /// chain reports a count below nonces already mined, and those aren't gaps.
    pub(crate) fn next_nonce(&self, from: Address, node_pending: u64) -> u64 {
        let holds = |nonce: u64| {
            self.entries.iter().any(|entry| entry.from == from && entry.nonce == nonce)
        };
        let past_held = self
            .entries
            .iter()
            .filter(|entry| entry.from == from)
            .map(|entry| entry.nonce.saturating_add(1))
            .max()
            .unwrap_or(0);
        self.given_back
            .iter()
            .filter(|&&(sender, gap)| sender == from && gap >= node_pending && !holds(gap))
            .map(|&(_, gap)| gap)
            .fold(node_pending.max(past_held), u64::min)
    }

    /// The floor a build at an occupied `nonce` must clear.
    ///
    /// Each half is the highest among everything holding the slot, taken
    /// separately, because a replacement has to outbid every transaction it might
    /// be racing on both the cap and the tip.
    pub(crate) fn replacement_floor(
        &self,
        from: Address,
        nonce: u64,
    ) -> Result<Option<FeePair>, EvmError> {
        let mut floor: Option<FeePair> = None;
        for entry in
            self.entries.iter().filter(|pending| pending.from == from && pending.nonce == nonce)
        {
            let candidate = entry.fee.replacement_floor()?;
            floor = Some(match floor {
                None => candidate,
                Some(current) => current.raised_to(candidate)?,
            });
        }
        Ok(floor)
    }

    pub(crate) fn hold(&mut self, entry: Tracked) {
        self.given_back.retain(|&slot| slot != (entry.from, entry.nonce));
        self.entries.push(entry);
    }

    /// Fills in the txid and calldata of the matching held slot, or takes a fresh
    /// slot when the reservation expired, because a signed transaction is about
    /// to be sent and must be watched.
    pub(crate) fn record_signed(&mut self, signed: Tracked) {
        if let Some(existing) =
            self.entries.iter_mut().find(|entry| !entry.is_sent() && entry.is_same_build(&signed))
        {
            existing.state = signed.state;
            existing.calldata = signed.calldata;
            return;
        }
        self.hold(signed);
    }

    /// Called before the bytes go out, so a crash mid-send still leaves a
    /// transaction this wallet knows to ask about.
    pub(crate) fn mark_sent(&mut self, txid: B256) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.txid() == Some(txid))
            && let State::Signed { ref mut is_sent, .. } = entry.state
        {
            *is_sent = true;
        }
    }

    /// A sent transaction keeps its slot, because it may already be in a mempool.
    pub(crate) fn release_held(&mut self, from: Address, nonce: u64) {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.is_sent() || entry.from != from || entry.nonce != nonce);
        if self.entries.len() < before {
            self.give_back(from, nonce);
        }
    }

    /// Only a refusal proving the transaction is in no mempool releases the slot,
    /// which frees its nonce for the rebuild. A refusal another endpoint might not
    /// repeat, or one saying the network already has the transaction, keeps it.
    pub(crate) fn release_refused(&mut self, txid: B256, retry: RetryPolicy) -> bool {
        matches!(retry, RetryPolicy::DidNotLand | RetryPolicy::FinalEverywhere)
            && self.release(txid)
    }

    /// Releases the slot of a transaction no node holds, which frees its nonce for
    /// the next build.
    pub(crate) fn release(&mut self, txid: B256) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.txid() == Some(txid)) else {
            return false;
        };
        let released = self.entries.remove(index);
        self.give_back(released.from, released.nonce);
        true
    }

    /// Stops watching a send whose nonce is taken for good, by itself when
    /// `receipt` says so and by another transaction otherwise. The nonce is spent
    /// either way, so the slot isn't given back.
    pub(crate) fn retire(
        &mut self,
        txid: B256,
        receipt: Option<Receipt>,
        committed: u64,
    ) -> Result<(), EvmError> {
        let Some((index, tracked)) =
            self.entries.iter().enumerate().find(|(_, entry)| entry.txid() == Some(txid))
        else {
            return Ok(());
        };
        let from = tracked.from;
        let checkpoint = self
            .settled
            .iter()
            .filter(|entry| entry.tracked.from == from)
            .map(|entry| entry.checkpoint)
            .fold(committed, u64::max)
            .checked_add(1)
            .ok_or(EvmError::UnreadableState)?;
        let tracked = self.entries.remove(index);
        self.settled.push(Settled { tracked, txid, receipt, checkpoint });
        Ok(())
    }

    pub(crate) fn settlements(&self) -> &[Settled] {
        &self.settled
    }

    /// Drops `from`'s settlements once a loaded checkpoint covers their records.
    /// One with a receipt also waits until the stored native nonce is past it,
    /// because until then it still explains the balance.
    pub(crate) fn acknowledge(
        &mut self,
        from: Address,
        committed: u64,
        native_nonce: Option<u64>,
    ) -> bool {
        let before = self.settled.len();
        self.settled.retain(|entry| {
            entry.tracked.from != from
                || entry.checkpoint > committed
                || (entry.receipt.is_some()
                    && native_nonce.is_none_or(|nonce| nonce <= entry.tracked.nonce))
        });
        self.settled.len() < before
    }

    /// Remembers a released slot while one above it is still held, and forgets
    /// slots with nothing held above them any more, since they're no longer gaps.
    fn give_back(&mut self, from: Address, nonce: u64) {
        let slot = (from, nonce);
        let is_still_held = self.entries.iter().any(|entry| (entry.from, entry.nonce) == slot);
        if !is_still_held && !self.given_back.contains(&slot) {
            self.given_back.push(slot);
        }
        let entries = &self.entries;
        self.given_back.retain(|&(sender, gap)| {
            entries.iter().any(|entry| entry.from == sender && entry.nonce > gap)
        });
    }

    pub(crate) fn expire_held(&mut self, now_millis: u64, ttl_millis: u64) {
        let (kept, expired): (Vec<Tracked>, Vec<Tracked>) =
            mem::take(&mut self.entries).into_iter().partition(|entry| match entry.state {
                State::Held { since_millis } => {
                    now_millis.saturating_sub(since_millis) < ttl_millis
                }
                State::Signed { .. } => true,
            });
        self.entries = kept;
        for entry in &expired {
            self.give_back(entry.from, entry.nonce);
        }
        if !expired.is_empty() {
            tracing::debug!(
                target: TARGET,
                dropped = expired.len(),
                "released nonce slots no send claimed"
            );
        }
    }

    pub(crate) fn retain_senders(&mut self, senders: &[Address]) {
        self.entries.retain(|entry| senders.contains(&entry.from));
        self.settled.retain(|entry| senders.contains(&entry.tracked.from));
        self.given_back.retain(|(sender, _)| senders.contains(sender));
    }

    pub(crate) fn watching(&self) -> Vec<InFlight> {
        self.entries
            .iter()
            .filter_map(|entry| {
                entry.sent().map(|envelope| InFlight { txid: envelope.txid, from: entry.from })
            })
            .collect()
    }

    pub(crate) fn entry(&self, txid: B256) -> Option<&Tracked> {
        self.entries.iter().find(|entry| entry.txid() == Some(txid))
    }

    /// Records the inclusion an observation found, or its absence.
    pub(crate) fn land(&mut self, txid: B256, landing: Option<Landing>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.txid() == Some(txid)) {
            entry.landing = landing;
        }
    }

    pub(crate) fn sent_from(&self, from: Address) -> impl Iterator<Item = (&Tracked, &Envelope)> {
        self.entries
            .iter()
            .filter(move |entry| entry.from == from)
            .filter_map(|entry| Some((entry, entry.sent()?)))
    }

    pub(crate) fn restore(&mut self, (entries, settled): (Vec<Tracked>, Vec<Settled>)) {
        for entry in settled {
            if !self.settled.iter().any(|known| known.txid == entry.txid) {
                self.settled.push(entry);
            }
        }
        for entry in entries {
            if self.entries.iter().any(|known| known.txid() == entry.txid()) {
                continue;
            }
            self.entries.push(entry);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation<'r> {
    /// No receipt yet, and none before.
    StillPending,
    /// A receipt that was there is gone, so the inclusion was reorged away.
    Unlanded,
    /// On chain, by the receipt that says so. `should_report` is true when no
    /// record has been emitted for this inclusion yet.
    Landed { receipt: &'r Receipt, landing: Landing, should_report: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settled {
    pub(crate) tracked: Tracked,
    pub(crate) txid: B256,
    pub(crate) receipt: Option<Receipt>,
    pub(crate) checkpoint: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{address, held, payment, sent_ledger, signed, txid_of};

    fn receipt_at(landing: Landing) -> Receipt {
        Receipt {
            transaction_hash: B256::ZERO,
            status: Some(1),
            gas_used: None,
            effective_gas_price: None,
            l1_fee: None,
            block_number: Some(landing.height),
            block_hash: landing.block_hash,
        }
    }

    /// Observes `receipt` and records its landing, as a pass does once its records
    /// are out.
    fn observe_and_land<'r>(
        ledger: &mut Ledger,
        txid: B256,
        receipt: Option<&'r Receipt>,
    ) -> Observation<'r> {
        let observed = ledger.entry(txid).expect("a tracked send").observe(receipt);
        ledger.land(txid, receipt.and_then(Receipt::landing));
        observed
    }

    #[test]
    fn the_next_nonce_is_the_first_slot_neither_the_node_nor_this_ledger_claims() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        assert_eq!(ledger.next_nonce(owner, 5), 5, "with nothing held the node decides");

        ledger.hold(held(owner, 5, 1_000, 0));
        assert_eq!(ledger.next_nonce(owner, 5), 6, "a held slot the node cannot know about");
        ledger.hold(held(owner, 6, 1_000, 0));
        assert_eq!(ledger.next_nonce(owner, 5), 7);

        assert_eq!(ledger.next_nonce(address(2), 5), 5, "another sender has its own slots");
        assert_eq!(ledger.next_nonce(owner, 20), 20, "a node that moved on wins");
    }

    #[test]
    fn a_slot_given_back_below_a_held_one_is_filled_first() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        for nonce in [5, 6, 7] {
            ledger.hold(held(owner, nonce, 1_000, 0));
        }
        assert_eq!(ledger.next_nonce(owner, 2), 8, "a node that trails the chain leaves no gap");

        ledger.release_held(owner, 6);
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.next_nonce(owner, 5), 6, "the node's count stops below the gap");
        assert_eq!(ledger.next_nonce(owner, 7), 8, "a count past the gap says it was filled");
        ledger.hold(held(owner, 6, 1_000, 0));
        assert_eq!(ledger.next_nonce(owner, 5), 8, "a filled gap is no gap");

        ledger.release_held(owner, 7);
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.next_nonce(owner, 5), 7, "a slot with nothing above it was never a gap");
    }

    #[test]
    fn a_build_at_an_occupied_slot_has_to_outbid_everything_holding_it() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        assert_eq!(ledger.replacement_floor(owner, 5).expect("no floor"), None);

        ledger.hold(held(owner, 5, 1_000, 0));
        ledger.hold(held(owner, 5, 2_000, 0));
        let floor = ledger.replacement_floor(owner, 5).expect("a floor").expect("occupied");
        assert_eq!(floor.max_fee_per_gas(), 2_200, "the highest of the two, a tenth higher");
        assert_eq!(ledger.replacement_floor(owner, 6).expect("no floor"), None);
    }

    #[test]
    fn a_replacement_floor_takes_the_highest_cap_and_the_highest_tip_separately() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        for (cap, tip) in [(2_000, 10), (1_000, 500)] {
            let fee = FeePair::new(cap, tip).expect("a pair");
            ledger.hold(Tracked::held(payment(owner, owner), 5, fee, 0));
        }

        let floor = ledger.replacement_floor(owner, 5).expect("a floor").expect("occupied");
        assert_eq!(floor.max_fee_per_gas(), 2_200);
        assert_eq!(floor.max_priority_fee_per_gas(), 550);
    }

    #[test]
    fn a_held_slot_is_released_by_a_discard_and_a_sent_one_never_is() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        ledger.hold(held(owner, 5, 1_000, 0));
        ledger.release_held(owner, 5);
        assert_eq!(ledger.len(), 0);

        let txid = txid_of(7);
        ledger.record_signed(signed(owner, 6, 1_000, 7));
        ledger.mark_sent(txid);
        assert!(ledger.entry(txid).is_some_and(Tracked::is_sent));
        ledger.release_held(owner, 6);
        assert_eq!(ledger.len(), 1, "a sent transaction keeps its slot");
    }

    #[test]
    fn signing_fills_in_the_transaction_id_of_the_slot_the_build_took() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        let txid = txid_of(3);
        ledger.hold(held(owner, 5, 1_000, 0));

        ledger.record_signed(signed(owner, 5, 1_000, 3));
        assert_eq!(ledger.len(), 1, "the slot was filled in, not doubled");
        assert_eq!(ledger.watching().len(), 0, "nothing is watched before it is sent");
        ledger.mark_sent(txid);
        assert_eq!(ledger.watching(), vec![InFlight { txid, from: owner }]);
    }

    #[test]
    fn signing_a_build_whose_reservation_expired_still_watches_the_send() {
        let mut ledger = Ledger::new();
        let txid = txid_of(3);
        ledger.record_signed(signed(address(1), 5, 1_000, 3));
        assert_eq!(ledger.len(), 1);
        ledger.mark_sent(txid);
        assert_eq!(ledger.watching().len(), 1);
    }

    #[test]
    fn only_a_refusal_proving_the_send_is_in_no_mempool_releases_a_sent_slot() {
        let txid = txid_of(4);
        let mut ledger = sent_ledger(4);
        assert_eq!(ledger.watching().len(), 1);
        for retry in [RetryPolicy::AlreadyLanded, RetryPolicy::TryAnotherEndpoint] {
            assert!(!ledger.release_refused(txid, retry), "{retry:?}");
            assert_eq!(ledger.len(), 1);
        }
        for retry in [RetryPolicy::DidNotLand, RetryPolicy::FinalEverywhere] {
            let mut ledger = sent_ledger(4);
            assert!(ledger.release_refused(txid, retry), "{retry:?}");
            assert_eq!(ledger.len(), 0, "{retry:?}");
        }
    }

    #[test]
    fn a_signed_unsent_slot_does_not_expire_or_become_available_to_another_build() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        let txid = txid_of(3);
        ledger.record_signed(signed(owner, 5, 1_000, 3));

        ledger.expire_held(2_000, 1_000);
        assert_eq!(ledger.len(), 1, "a signed slot outlives the timeout");
        assert_eq!(ledger.next_nonce(owner, 5), 6);
        ledger.mark_sent(txid);
        assert_eq!(ledger.watching(), vec![InFlight { txid, from: owner }]);
    }

    #[test]
    fn a_held_slot_expires_and_a_sent_one_does_not() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        ledger.hold(held(owner, 5, 1_000, 0));
        let txid = txid_of(1);
        ledger.record_signed(signed(owner, 6, 1_000, 1));
        ledger.mark_sent(txid);

        ledger.expire_held(500, 1_000);
        assert_eq!(ledger.len(), 2, "inside the window nothing goes");
        ledger.expire_held(2_000, 1_000);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.next_nonce(owner, 0), 5, "the expired slot is a gap below the sent one");
        ledger.hold(held(owner, 5, 1_000, 0));
        assert_eq!(ledger.next_nonce(owner, 0), 7, "the sent slot still holds its nonce");
    }

    #[test]
    fn an_inclusion_is_reported_once_and_a_reorganized_one_is_reported_again() {
        let txid = txid_of(8);
        let mut ledger = sent_ledger(8);

        assert_eq!(observe_and_land(&mut ledger, txid, None), Observation::StillPending);

        let first = Landing { height: 20, block_hash: Some(B256::repeat_byte(1)) };
        let first_receipt = receipt_at(first);
        assert_eq!(
            observe_and_land(&mut ledger, txid, Some(&first_receipt)),
            Observation::Landed { receipt: &first_receipt, landing: first, should_report: true }
        );
        assert_eq!(
            observe_and_land(&mut ledger, txid, Some(&first_receipt)),
            Observation::Landed { receipt: &first_receipt, landing: first, should_report: false },
            "the same inclusion is not reported twice"
        );

        assert_eq!(observe_and_land(&mut ledger, txid, None), Observation::Unlanded);
        let second = Landing { height: 21, block_hash: Some(B256::repeat_byte(2)) };
        let second_receipt = receipt_at(second);
        assert_eq!(
            observe_and_land(&mut ledger, txid, Some(&second_receipt)),
            Observation::Landed { receipt: &second_receipt, landing: second, should_report: true },
            "a new inclusion after a reorg is reported"
        );

        let moved = Landing { height: 21, block_hash: Some(B256::repeat_byte(3)) };
        let moved_receipt = receipt_at(moved);
        assert_eq!(
            observe_and_land(&mut ledger, txid, Some(&moved_receipt)),
            Observation::Landed { receipt: &moved_receipt, landing: moved, should_report: true },
            "the same height in another block is another inclusion"
        );
    }

    #[test]
    fn an_inclusion_nobody_recorded_is_still_to_be_reported() {
        let txid = txid_of(8);
        let ledger = sent_ledger(8);

        let landing = Landing { height: 20, block_hash: Some(B256::repeat_byte(1)) };
        let receipt = receipt_at(landing);
        let reported = Observation::Landed { receipt: &receipt, landing, should_report: true };
        let entry = ledger.entry(txid).expect("a tracked send");
        assert_eq!(entry.observe(Some(&receipt)), reported);
        assert_eq!(
            entry.observe(Some(&receipt)),
            reported,
            "an observation whose record was never delivered changes nothing"
        );
    }

    #[test]
    fn a_receipt_without_a_block_is_no_inclusion() {
        let txid = txid_of(8);
        let ledger = sent_ledger(8);
        let unmined =
            Receipt { block_number: None, ..receipt_at(Landing { height: 0, block_hash: None }) };
        let entry = ledger.entry(txid).expect("a tracked send");
        assert_eq!(entry.observe(Some(&unmined)), Observation::StillPending);
    }

    #[test]
    fn a_send_retired_without_a_receipt_leaves_its_nonce_spent() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        for (seed, nonce) in [(1u8, 5), (2, 6)] {
            ledger.record_signed(signed(owner, nonce, 1_000, seed));
            ledger.mark_sent(txid_of(seed));
        }
        ledger.retire(txid_of(1), None, 0).unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.settled.len(), 1);
        assert_eq!(ledger.next_nonce(owner, 5), 7, "another transaction took nonce five");
    }

    #[test]
    fn a_receipt_is_kept_until_its_record_and_its_spend_are_both_acknowledged() {
        let owner = address(1);
        let mut ledger = sent_ledger(8);
        let receipt = receipt_at(Landing { height: 20, block_hash: None });
        ledger.retire(txid_of(8), Some(receipt), 4).unwrap();
        assert_eq!(ledger.settlements()[0].checkpoint, 5);
        assert!(!ledger.acknowledge(owner, 4, Some(6)));
        assert!(!ledger.acknowledge(address(2), 5, Some(6)));
        assert!(!ledger.acknowledge(owner, 5, None));
        assert!(!ledger.acknowledge(owner, 5, Some(5)));
        assert!(ledger.acknowledge(owner, 5, Some(6)));
        assert!(ledger.settlements().is_empty());

        ledger.record_signed(signed(owner, 6, 1_000, 9));
        ledger.mark_sent(txid_of(9));
        ledger.retire(txid_of(9), None, 5).unwrap();
        assert!(ledger.acknowledge(owner, 6, None), "a replaced send spent nothing");
    }

    #[test]
    fn a_checkpoint_that_cannot_advance_leaves_the_send_watched() {
        let mut ledger = sent_ledger(8);
        ledger.retire(txid_of(8), None, u64::MAX - 1).unwrap();
        ledger.record_signed(signed(address(1), 6, 1_000, 9));
        ledger.mark_sent(txid_of(9));
        assert!(matches!(ledger.retire(txid_of(9), None, 0), Err(EvmError::UnreadableState)));
        assert_eq!(ledger.watching(), vec![InFlight { txid: txid_of(9), from: address(1) }]);
        assert_eq!(ledger.settlements()[0].checkpoint, u64::MAX);
    }

    #[test]
    fn senders_that_left_the_watch_set_are_forgotten() {
        let mut ledger = Ledger::new();
        ledger.hold(held(address(1), 1, 1_000, 0));
        ledger.hold(held(address(2), 1, 1_000, 0));
        ledger.retain_senders(&[address(1)]);
        assert_eq!(ledger.len(), 1);
        ledger.retain_senders(&[]);
        assert_eq!(ledger.len(), 0);
    }
}
