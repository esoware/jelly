use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainId, Clock as _, CoreError, CursorBytes, CursorScope,
    CursorStore, FeeSelection, HeldReservation, ReservationSlot, ScanAdvance, ScanStream, SignedTx,
    TxDirection, TxOutcome, TxRecord, WalletEvent,
    test_util::{
        ControlledCursorStore, FakeClock, FakeEventSink, MemoryCursorStore, TxRequestBuilder,
        records,
    },
};
use jelly_monero::{
    MoneroAdapter, MoneroConfig,
    test_util::{account_balance, in_flight, replace_scan, stored_scan, stored_sends},
};
use monero_address::SubaddressIndex;
use tokio::sync::Notify;

use crate::common::{
    ChainBuilder, ENDPOINT, FakeNode, Fallible, Harness, PRIMARY_ID, base_chain, bytes32, config,
    fixture_adapter, images_in, owned_in, payment, reopen, stranger, watched,
};

#[derive(Debug)]
struct PausedScans {
    inner: Arc<MemoryCursorStore>,
    is_paused: AtomicBool,
    entered: Notify,
    resume: Notify,
}

#[async_trait]
impl CursorStore for PausedScans {
    async fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, CoreError> {
        let rows = self.inner.load_scans(scopes, streams).await?;
        if self.is_paused.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Ok(rows)
    }

    async fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, CoreError> {
        self.inner.load_reservations(scopes, slots).await
    }

    async fn advance(
        &self,
        scope: CursorScope,
        stream: ScanStream,
        cursor: CursorBytes,
    ) -> Result<(), CoreError> {
        self.inner.advance(scope, stream, cursor).await
    }

    async fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: CursorBytes,
    ) -> Result<(), CoreError> {
        self.inner.reserve(scope, slot, payload).await
    }
}

#[tokio::test]
async fn a_pass_reloading_the_committed_scan_keeps_a_reservation_broadcast_meanwhile() {
    let mut harness = Harness::new().await.unwrap();
    let cursors = Arc::new(PausedScans {
        inner: Arc::clone(&harness.cursors),
        is_paused: AtomicBool::new(false),
        entered: Notify::new(),
        resume: Notify::new(),
    });
    harness.adapter = fixture_adapter(
        Arc::<FakeNode>::clone(&harness.node),
        Arc::<PausedScans>::clone(&cursors),
        Arc::<FakeClock>::clone(&harness.clock),
        config(),
    )
    .await
    .unwrap();
    let unsigned = harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    cursors.is_paused.store(true, Ordering::SeqCst);
    let broadcasting = async {
        cursors.entered.notified().await;
        harness.adapter.broadcast(&signed).await.unwrap();
        let durable = stored_sends(cursors.as_ref()).await.unwrap().unwrap();
        assert!(durable.with_bytes(|bytes| {
            bytes.windows(signed.raw.as_bytes().len()).any(|window| window == signed.raw.as_bytes())
        }));
        cursors.resume.notify_one();
        durable
    };
    let (synced, durable) = tokio::join!(harness.sync(), broadcasting);
    synced.unwrap();
    assert_eq!(in_flight(&harness.adapter), 1);
    assert!(!account_balance(&harness.adapter, 0).reserved().is_zero());
    assert_eq!(stored_sends(cursors.as_ref()).await.unwrap(), Some(durable));
}

#[derive(Clone, Copy)]
enum TerminalCase {
    Landed,
    Displaced,
    Abandoned,
}

impl TerminalCase {
    const fn outcome(self) -> TxOutcome {
        match self {
            Self::Landed => TxOutcome::Success,
            Self::Displaced | Self::Abandoned => TxOutcome::Failed,
        }
    }
}

/// What happens to the scan checkpoint the settling pass wrote before the replay.
#[derive(Clone, Copy)]
enum Checkpoint {
    Uncommitted,
    Discarded,
}

/// Whether the wallet locks and unlocks between the settling pass and the replay.
#[derive(Clone, Copy)]
enum Session {
    Kept,
    Restarted,
}

async fn cross_funded() -> Fallible<(Harness, SignedTx)> {
    let harness = Harness::new().await?;
    let funding = harness.account.address(0, 1)?;
    harness
        .adapter
        .set_watch_set(&[watched(PRIMARY_ID, 0, 0, &harness.primary), watched(8, 0, 1, &funding)])
        .await?;
    let sweep = TxRequestBuilder::new(PRIMARY_ID, &funding, MONERO, Amount::ZERO)
        .fee(FeeSelection::NORMAL)
        .send_max()
        .build();
    let unsigned = harness.adapter.build_tx(&sweep).await?;
    let swept = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()])?;
    harness.adapter.broadcast(&swept).await?;
    let moved = harness.tip.saturating_add(1);
    let blocks = base_chain(harness.height)?
        .landed(moved, 1, &swept)
        .run(moved.saturating_add(1), 10, 1)
        .finish()?;
    let moved_block = blocks
        .iter()
        .find(|block| u64::try_from(block.block.number()).ok() == Some(moved))
        .ok_or("sweep block")?;
    let funding_index = SubaddressIndex::new(0, 1).ok_or("(0, 1) is a subaddress")?;
    for output in owned_in(&harness.recorded, moved_block.clone(), &[funding_index])? {
        harness.node.register_real(&output);
    }
    harness.node.serve(blocks);
    harness.node.set_tip(moved.saturating_add(10));
    harness.sync().await?;
    harness.sync().await?;
    harness.clock.advance(Duration::from_secs(601));
    Ok((harness, swept))
}

fn terminal_record(events: &[WalletEvent], signed: &SignedTx) -> Fallible<TxRecord> {
    let mut found: Vec<_> =
        records(events).into_iter().filter(|row| row.txid == signed.txid).collect();
    if found.len() != 1 {
        return Err(format!("expected exactly one record for the send, got {found:?}").into());
    }
    found.pop().ok_or_else(|| "terminal record".into())
}

async fn terminal_replay(
    case: TerminalCase,
    session: Session,
    checkpoint: Checkpoint,
) -> Fallible<()> {
    let (mut harness, swept) = cross_funded().await?;
    let funding = harness.account.address(0, 1)?;
    let watches = [watched(PRIMARY_ID, 0, 0, &harness.primary), watched(8, 0, 1, &funding)];
    let moved = harness.tip.saturating_add(1);
    let unlocked = moved.saturating_add(10);
    let funded_chain = || -> Fallible<ChainBuilder> {
        Ok(base_chain(harness.height)?.landed(moved, 1, &swept).run(moved.saturating_add(1), 10, 1))
    };
    let cursors = Arc::new(ControlledCursorStore::default());
    replace_scan(
        cursors.committed(),
        stored_scan(harness.cursors.as_ref()).await?.ok_or("stored state")?,
    )
    .await?;
    let settings = MoneroConfig { abandon_after_blocks: 5, ..config() };
    let build = || {
        MoneroAdapter::builder(
            Arc::<FakeNode>::clone(&harness.node),
            harness.account.clone(),
            Arc::<ControlledCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&harness.clock),
            harness.height,
        )
        .config(settings.clone())
    };
    harness.adapter = build().build().await?;
    harness.adapter.set_watch_set(&watches).await?;
    let request = payment(&stranger()?.address(0, 0)?, 1_000);
    let unsigned = harness.adapter.build_tx(&request).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()])?;
    harness.adapter.broadcast(&signed).await?;
    let landed = unlocked.saturating_add(1);
    let chain = match case {
        TerminalCase::Landed => funded_chain()?.landed(landed, 1, &signed),
        TerminalCase::Displaced => {
            funded_chain()?.spending(landed, 1, [0xee; 32], &images_in(&signed)?, 900)
        }
        TerminalCase::Abandoned => {
            harness.node.forget(&bytes32(signed.txid.value())?);
            funded_chain()?.run(landed, 6, 1)
        }
    };
    harness.node.serve(chain.finish()?);
    harness.node.set_tip(if matches!(case, TerminalCase::Abandoned) {
        landed.saturating_add(5)
    } else {
        landed
    });
    let baseline = stored_scan(cursors.as_ref()).await?;
    let (_, emitted) = harness.sync().await?;
    let initial = terminal_record(&emitted, &signed)?;
    assert_eq!(initial.outcome, case.outcome(), "the settling pass reports the send's fate");
    assert_eq!(initial.address_id, PRIMARY_ID, "the first scan uses the request's address");
    assert_eq!(initial.amount, Amount::from_base_units(1_000), "the payment is net of change");
    assert_eq!(
        (in_flight(&harness.adapter), account_balance(&harness.adapter, 0).reserved()),
        (0, Amount::ZERO),
        "terminal metadata must not reserve inputs"
    );
    let saved = stored_sends(cursors.as_ref()).await?.ok_or("stored state")?;
    assert!(
        saved.with_bytes(|bytes| !bytes
            .windows(signed.raw.as_bytes().len())
            .any(|window| window == signed.raw.as_bytes())),
        "terminal attribution must not retain signed bytes"
    );
    if matches!(checkpoint, Checkpoint::Discarded) {
        cursors.discard();
    }
    assert_eq!(stored_scan(cursors.as_ref()).await?, baseline, "the stored scan still lags");
    if matches!(session, Session::Restarted) {
        harness.adapter = build().build().await?;
        harness.adapter.set_watch_set(&watches).await?;
        assert_eq!(
            (in_flight(&harness.adapter), account_balance(&harness.adapter, 0).reserved()),
            (0, Amount::ZERO),
            "terminal metadata must not reserve inputs"
        );
    }
    let (_, replay) = harness.sync().await?;
    let replayed = terminal_record(&replay, &signed)?;
    assert_eq!(
        replayed.address_id, PRIMARY_ID,
        "terminal replay must retain the request's address, not the funding address"
    );
    assert_eq!(replayed.amount, initial.amount, "the replay keeps the amount");
    assert_eq!(replayed.fee, initial.fee, "the replay keeps the fee");
    assert_eq!(replayed.outcome, initial.outcome, "the replay keeps the outcome");
    cursors.commit().await?;
    let (_, quiet) = harness.sync().await?;
    assert!(
        !records(&quiet).iter().any(|row| row.txid == signed.txid),
        "acknowledged settlements stay quiet"
    );
    let cleaned = stored_sends(cursors.as_ref()).await?.ok_or("stored state")?;
    assert!(cleaned.len() < saved.len(), "the pass after the commit drops the terminal metadata");
    Ok(())
}

#[tokio::test]
async fn a_landed_send_replays_under_its_sender_after_a_discarded_checkpoint() {
    terminal_replay(TerminalCase::Landed, Session::Kept, Checkpoint::Discarded).await.unwrap();
}

#[tokio::test]
async fn a_landed_send_replays_under_its_sender_after_a_discarded_checkpoint_and_a_restart() {
    terminal_replay(TerminalCase::Landed, Session::Restarted, Checkpoint::Discarded).await.unwrap();
}

#[tokio::test]
async fn a_landed_send_replays_under_its_sender_before_its_checkpoint_commits() {
    terminal_replay(TerminalCase::Landed, Session::Kept, Checkpoint::Uncommitted).await.unwrap();
}

#[tokio::test]
async fn a_landed_send_replays_under_its_sender_across_a_restart_before_its_checkpoint_commits() {
    terminal_replay(TerminalCase::Landed, Session::Restarted, Checkpoint::Uncommitted)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_displaced_send_replays_as_failed_after_a_discarded_checkpoint_and_a_restart() {
    terminal_replay(TerminalCase::Displaced, Session::Restarted, Checkpoint::Discarded)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_abandoned_send_replays_as_failed_after_a_discarded_checkpoint_and_a_restart() {
    terminal_replay(TerminalCase::Abandoned, Session::Restarted, Checkpoint::Discarded)
        .await
        .unwrap();
}

fn failed(records: &[TxRecord]) -> Option<&TxRecord> {
    records.iter().find(|record| record.outcome == TxOutcome::Failed)
}

#[tokio::test]
async fn a_send_that_lands_stops_being_in_flight_and_is_reported_once() {
    let harness = Harness::new().await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let images = images_in(&signed).unwrap();
    let mut txid = [0u8; 32];
    hex::decode_to_slice(signed.txid.value(), &mut txid).unwrap();

    // The chain now contains the transaction at the next height, with three
    // blocks on top.
    let landed = harness.tip.saturating_add(1);
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .spending(landed, 1, txid, &images, 6_000)
            .run(landed.saturating_add(1), 3, 1)
            .finish()
            .unwrap(),
    );
    harness.node.set_tip(landed.saturating_add(3));

    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(in_flight(&harness.adapter), 0, "the chain settled it");
    let reported = records(&events);
    let spends: Vec<&TxRecord> =
        reported.iter().filter(|record| record.txid == signed.txid).collect();
    assert_eq!(spends.len(), 1, "the spend is reported exactly once: {spends:?}");
    let spend = spends.first().expect("the spend was reported");
    assert_eq!(spend.outcome, TxOutcome::Success);
    assert_eq!(spend.fee, Some(Amount::from_base_units(6_000)));
    assert!(failed(&reported).is_none(), "a landed send is not a failure");
    assert_eq!(spend.confirmations, 4);

    // The reservation is gone on disk too, so an unlock doesn't bring it back and
    // hold coins the chain already moved.
    let raw = signed.raw.as_bytes();
    if let Some(blob) = stored_sends(&*harness.cursors).await.unwrap() {
        assert!(
            blob.with_bytes(|bytes| !bytes.windows(raw.len()).any(|window| window == raw)),
            "a settled send is still reserved on disk"
        );
    }
}

/// The change comes back to the paying address in the same transaction. The outgoing
/// amount is net of it, so it's never a payment of its own.
#[tokio::test]
async fn the_change_of_a_landed_send_is_not_reported_as_a_payment_in() {
    let harness = Harness::new().await.unwrap();
    let request = payment(&stranger().unwrap().address(0, 0).unwrap(), 1_000);
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();

    let landed = harness.tip.saturating_add(1);
    harness
        .node
        .serve(base_chain(harness.height).unwrap().landed(landed, 1, &signed).finish().unwrap());
    harness.node.set_tip(landed);

    let (_, events) = harness.sync().await.unwrap();
    let reported: Vec<TxRecord> =
        records(&events).into_iter().filter(|record| record.txid == signed.txid).collect();
    assert_eq!(reported.len(), 1, "one record for the send and none for its change: {reported:?}");
    let spend = reported.first().expect("the send was reported");
    assert_eq!(spend.direction, TxDirection::Outgoing);
    assert_eq!(spend.address_id, PRIMARY_ID);
    assert_eq!(spend.amount, Amount::from_base_units(1_000), "what left, net of change and fee");
}

#[tokio::test]
async fn a_send_displaced_by_a_conflicting_transaction_is_reported_failed() {
    let harness = Harness::new().await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let images = images_in(&signed).unwrap();

    // Something else spent the same key image, which only one transaction can.
    let landed = harness.tip.saturating_add(1);
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .spending(landed, 1, [0xee; 32], &images, 900)
            .finish()
            .unwrap(),
    );
    harness.node.set_tip(landed);

    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(in_flight(&harness.adapter), 0);
    let reported = records(&events);
    let failure = failed(&reported).expect("the send was reported failed");
    assert_eq!(failure.txid, signed.txid);
    assert_eq!(failure.fee, Some(Amount::ZERO), "a transaction that never landed cost nothing");
}

#[tokio::test]
async fn a_send_no_node_holds_is_resent_until_its_budget_runs_out() {
    let harness =
        Harness::with(MoneroConfig { abandon_after_blocks: 5, ..config() }).await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let mut txid = [0u8; 32];
    hex::decode_to_slice(signed.txid.value(), &mut txid).unwrap();

    // Every mempool forgot it, and the chain has barely moved.
    harness.node.forget(&txid);
    let sent = harness.node.published().len();
    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(in_flight(&harness.adapter), 1, "still inside its budget");
    assert!(harness.node.published().len() > sent, "the identical bytes went out again");
    assert!(failed(&records(&events)).is_none());
    let ring_node = Some(ENDPOINT.to_owned());
    assert!(
        harness.node.broadcast_endpoints().iter().all(|endpoint| *endpoint == ring_node),
        "they went back only to the node that served the ring"
    );
    assert!(harness.node.known_endpoints().iter().all(|endpoint| *endpoint == ring_node));

    // Now the chain has moved past the budget with the transaction still nowhere.
    harness.node.set_tip(harness.tip.saturating_add(30));
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .run(harness.tip.saturating_add(1), 30, 1)
            .finish()
            .unwrap(),
    );
    harness.node.forget(&txid);
    let (_, events) = harness.sync().await.unwrap();
    assert_eq!(in_flight(&harness.adapter), 0, "its budget ran out");
    let reported = records(&events);
    let failure = failed(&reported).expect("it was reported failed");
    assert_eq!(failure.txid, signed.txid);
    assert!(
        account_balance(&harness.adapter, 0).spendable() > Amount::ZERO,
        "its inputs went back"
    );

    // The follow-up ended the reservation on disk too, so an unlock doesn't hold
    // those coins again.
    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    assert_eq!(in_flight(&reopened), 0, "a dead send does not come back at the next unlock");
    assert!(account_balance(&reopened, 0).spendable() > Amount::ZERO);
}

#[tokio::test]
async fn a_send_its_ring_node_still_holds_is_never_resent() {
    let harness =
        Harness::with(MoneroConfig { abandon_after_blocks: 5, ..config() }).await.unwrap();
    harness.send(Some(1_000)).await.unwrap();
    let sent = harness.node.published().len();

    harness.node.set_tip(harness.tip.saturating_add(30));
    for _ in 0..3 {
        let (_, events) = harness.sync().await.unwrap();
        assert!(failed(&records(&events)).is_none());
    }
    assert_eq!(in_flight(&harness.adapter), 1, "the node still holds it, so it is still alive");
    assert_eq!(harness.node.published().len(), sent, "bytes the node holds are not sent again");
    assert!(harness.node.known_calls() >= 3, "the node was asked every pass");
    assert!(
        harness.node.known_endpoints().iter().all(|endpoint| endpoint.as_deref() == Some(ENDPOINT)),
        "and only the node that served the ring was asked"
    );
}

#[tokio::test]
async fn an_unreachable_ring_node_hands_the_follow_up_to_another_node() {
    let harness =
        Harness::with(MoneroConfig { abandon_after_blocks: 5, ..config() }).await.unwrap();
    let (_, signed) = harness.send(Some(1_000)).await.unwrap();
    let mut txid = [0u8; 32];
    hex::decode_to_slice(signed.txid.value(), &mut txid).unwrap();
    harness.node.forget(&txid);
    harness.node.set_named_unreachable(true);
    let sent = harness.node.published().len();

    harness.sync().await.unwrap();
    let asked = harness.node.known_endpoints();
    assert_eq!(
        asked.get(asked.len().saturating_sub(2)..),
        Some([Some(ENDPOINT.to_owned()), None].as_slice()),
        "the ring node first, and the pool once it could not be reached"
    );
    assert_eq!(harness.node.broadcast_endpoints().last(), Some(&None));
    assert!(harness.node.published().len() > sent, "the bytes reached another node");
    assert_eq!(in_flight(&harness.adapter), 1);
}

#[tokio::test]
async fn an_in_flight_send_survives_a_restart() {
    let harness = Harness::new().await.unwrap();
    harness.send(Some(1_000)).await.unwrap();
    let reserved = account_balance(&harness.adapter, 0).reserved();
    assert!(reserved > Amount::ZERO);

    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    assert_eq!(in_flight(&reopened), 1, "a payment in the air survives a lock and unlock");
    assert_eq!(account_balance(&reopened, 0).reserved(), reserved);
}

const MONERO: AssetId = AssetId::Native(ChainId::Monero);

/// The scanner watches account 1 too, but paying it from account 0 is still a payment
/// out. wallet2 nets an outgoing amount only against what came back to the spending
/// account.
#[tokio::test]
async fn a_payment_to_another_of_its_own_accounts_leaves_the_account_it_spent_from() {
    let harness = Harness::new().await.unwrap();
    let request = payment(&harness.account.address(1, 0).unwrap(), 1_000);
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    harness.adapter.broadcast(&signed).await.unwrap();

    let landed = harness.tip.saturating_add(1);
    harness
        .node
        .serve(base_chain(harness.height).unwrap().landed(landed, 1, &signed).finish().unwrap());
    harness.node.set_tip(landed);
    let (_, events) = harness.sync().await.unwrap();

    let spend = records(&events)
        .into_iter()
        .find(|record| record.txid == signed.txid && record.address_id == PRIMARY_ID)
        .expect("the send was reported against account 0");
    assert_eq!(
        (spend.direction, spend.amount),
        (TxDirection::Outgoing, Amount::from_base_units(1_000)),
        "expected account 0 to record 1000 piconero leaving it, got {:?} of {:?}",
        spend.direction,
        spend.amount
    );
}

/// The engine files a send under the address the request named.
///
/// If the scan filed it under the watched subaddress that held its largest input instead,
/// the engine's row would never resolve and the history would show one payment on two
/// addresses.
#[tokio::test]
async fn a_landed_send_is_filed_under_the_address_it_was_sent_from_not_the_one_that_funded_it() {
    let (harness, swept) = cross_funded().await.unwrap();
    let moved = harness.tip.saturating_add(1);
    let unlocked = moved.saturating_add(10);
    let request = payment(&stranger().unwrap().address(0, 0).unwrap(), 1_000);
    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let paid = harness.adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    harness.adapter.broadcast(&paid).await.unwrap();
    let landed = unlocked.saturating_add(1);
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .landed(moved, 1, &swept)
            .run(moved.saturating_add(1), 10, 1)
            .landed(landed, 1, &paid)
            .finish()
            .unwrap(),
    );
    harness.node.set_tip(landed);
    let (_, events) = harness.sync().await.unwrap();

    let filed: Vec<i64> = records(&events)
        .into_iter()
        .filter(|record| record.txid == paid.txid)
        .map(|record| record.address_id)
        .collect();
    assert_eq!(
        filed,
        vec![PRIMARY_ID],
        "expected the send to be filed under the primary address it was sent from, as the \
         engine files it, but it was filed under {filed:?}"
    );
}

/// The abandon budget counts from the height the ring was chosen at, not the older tip in
/// the saved scan state.
///
/// Otherwise the first pass after an unlock would abandon a fresh send just because no node
/// has it yet.
#[tokio::test]
async fn a_send_made_right_after_a_long_lock_stays_in_flight_inside_its_budget() {
    let harness = Harness::new().await.unwrap();
    let budget = config().abandon_after_blocks;
    let gap = budget.saturating_add(10);
    let grown = harness.tip.saturating_add(gap);
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .run(harness.tip.saturating_add(1), gap, 1)
            .finish()
            .unwrap(),
    );
    harness.node.set_tip(grown);

    // The wallet unlocks with the scan state it locked with, and the payment goes out
    // before its first pass.
    let adapter =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    let unsigned = adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    adapter.broadcast(&signed).await.unwrap();
    let mut txid = [0u8; 32];
    hex::decode_to_slice(signed.txid.value(), &mut txid).unwrap();

    // The first pass asks a node that hasn't seen the transaction yet.
    harness.node.forget(&txid);
    let events = FakeEventSink::new();
    adapter.sync(harness.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    let reported = records(&events.drain());
    assert!(
        failed(&reported).is_none() && in_flight(&adapter) == 1,
        "expected a send broadcast zero blocks ago to stay in flight inside its {budget}-block \
         budget, but it was reported {:?} with {} sends left in flight",
        failed(&reported).map(|record| record.outcome),
        in_flight(&adapter)
    );
}

#[tokio::test]
async fn an_unreadable_sends_blob_refuses_every_unlock_and_is_left_as_it_was() {
    let harness = Harness::new().await.unwrap();
    harness.send(Some(1_000)).await.unwrap();
    let saved = stored_sends(&*harness.cursors).await.unwrap().unwrap();
    let mut future = saved.with_bytes(<[u8]>::to_vec);
    future[8] = 99;
    let truncated = saved.with_bytes(|bytes| bytes[..bytes.len().saturating_sub(1)].to_vec());
    let broadcasts = harness.node.broadcast_endpoints().len();
    for bytes in [future, truncated, b"unreadable".to_vec()] {
        harness
            .cursors
            .reserve(CursorScope::Wallet, ReservationSlot::MoneroSends, CursorBytes::copied(&bytes))
            .await
            .unwrap();
        for _ in 0..2 {
            reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap_err();
        }
        assert!(
            stored_sends(&*harness.cursors)
                .await
                .unwrap()
                .unwrap()
                .with_bytes(|stored| stored == bytes)
        );
        assert_eq!(harness.node.broadcast_endpoints().len(), broadcasts);
    }
}

#[tokio::test]
async fn an_unreadable_scan_blob_is_rescanned_with_every_send_in_flight_still_reserved() {
    let harness = Harness::new().await.unwrap();
    harness.send(Some(1_000)).await.unwrap();
    let saved = stored_sends(&*harness.cursors).await.unwrap().unwrap();
    let reserved = account_balance(&harness.adapter, 0).reserved();
    replace_scan(&*harness.cursors, CursorBytes::copied(b"unreadable scan")).await.unwrap();
    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    assert_eq!(in_flight(&reopened), 1);
    reopened
        .sync(harness.clock.deadline_in(Duration::from_secs(30)), &FakeEventSink::new())
        .await
        .unwrap();
    assert_eq!(account_balance(&reopened, 0).reserved(), reserved);
    assert_eq!(stored_sends(&*harness.cursors).await.unwrap(), Some(saved));
}
