//! The watch set and the sync pass over it, through the adapter.

use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bitcoin::block::{Header, Version};
use bitcoin::consensus::serialize;
use bitcoin::hashes::Hash as _;
use bitcoin::{CompactTarget, OutPoint, TxMerkleNode};
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainSyncProgress, Clock as _, CursorBytes,
    CursorScope, CursorStore as _, EventSink, FeeSelection, ScriptKind, TxDetection, TxDirection,
    TxId, TxOutcome, WalletEvent, confirmations,
    test_util::{ControlledCursorStore, FakeEventSink, MemoryCursorStore, records},
};
use jelly_electrum::{Method, test_util::Answer};
use jelly_utxo::test_util::{claimed, cursor_summary, tx_spending};
use serde_json::Value;

use crate::common::{
    BITCOIN, BLOCK_UNIX_SECONDS, CHAINS, Fallible, Harness, LITECOIN, Ledger, MINED_AT,
    MINED_LATER, Setup, TIP, Wallet, genesis_header, kind_of, payload, sats, synced, watched,
};

const FUNDING_SAT: u64 = 190_000;

#[tokio::test]
async fn a_replacement_a_discarded_pass_reported_is_reported_again_by_the_next_pass() {
    replays_a_discarded_replacement(false).await.unwrap();
}

#[tokio::test]
async fn a_replacement_a_discarded_pass_reported_is_reported_again_once_it_confirms() {
    replays_a_discarded_replacement(true).await.unwrap();
}

/// A pass sees a replacement that leaves the wallet no change and settles the
/// original it displaced, and the engine then discards that pass's cursor writes.
///
/// The next pass has to report both again, the replacement at its height when
/// `should_confirm` mines it first.
async fn replays_a_discarded_replacement(should_confirm: bool) -> Fallible<()> {
    let cursors = Arc::new(ControlledCursorStore::default());
    let harness = Harness::with_store(
        Setup::on(&BITCOIN),
        Arc::new(MemoryCursorStore::new()),
        Arc::<ControlledCursorStore>::clone(&cursors),
    )?;
    let wallet = Wallet::new(&BITCOIN, kind_of(&BITCOIN), &[FUNDING_SAT])?;
    let original = tx_spending(&wallet.payment, 0, &[(wallet.theirs.script_pubkey(), 170_000)]);
    let replacement = tx_spending(&wallet.payment, 0, &[(wallet.theirs.script_pubkey(), 160_000)]);
    Ledger::new()
        .known(&wallet.funding)
        .confirmed(&wallet.payment, MINED_AT)
        .mempool(&original)
        .serve(&harness);
    harness.watch(&[wallet.address()]).await?;
    harness.sync().await?;
    assert!(
        harness
            .events
            .records()
            .iter()
            .any(|row| row.txid.value() == original.compute_txid().to_string()),
        "initial pending row is reported"
    );
    cursors.commit().await?;
    let baseline = cursors.load_scan(CursorScope::Address(7), BITCOIN.scan_stream).await?;

    Ledger::new()
        .known(&wallet.funding)
        .confirmed(&wallet.payment, MINED_AT)
        .mempool(&replacement)
        .serve(&harness);
    harness.sync().await?;
    let lost = harness.drain();
    assert!(
        records(&lost).iter().any(|row| row.txid.value() == replacement.compute_txid().to_string()),
        "replacement is reported"
    );
    assert!(
        records(&lost).iter().any(|row| row.txid.value() == original.compute_txid().to_string()
            && row.outcome == TxOutcome::Replaced),
        "original is settled in the lost window"
    );
    cursors.discard();
    assert_eq!(
        cursors.load_scan(CursorScope::Address(7), BITCOIN.scan_stream).await?,
        baseline,
        "discard leaves coverage unchanged"
    );
    if should_confirm {
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .confirmed(&replacement, MINED_LATER)
            .serve(&harness);
    }

    harness.sync().await?;
    let replay = harness.drain();
    let rows = records(&replay);
    assert!(
        rows.iter().any(|row| row.txid.value() == replacement.compute_txid().to_string()
            && row.height == should_confirm.then_some(u64::from(MINED_LATER))
            && row.amount == sats(160_000)
            && row.fee == Some(sats(30_000))),
        "uncommitted replacement must replay even without wallet change: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.txid.value() == original.compute_txid().to_string()
            && row.outcome == TxOutcome::Replaced),
        "lost conflict settlement must replay: {rows:?}"
    );
    cursors.commit().await?;
    harness.sync().await?;
    assert!(harness.events.records().is_empty(), "committed confirmation stays quiet");
    Ok(())
}

#[tokio::test]
async fn a_watch_set_reads_the_cursors_of_only_the_addresses_it_admits() {
    let cursors = Arc::new(ControlledCursorStore::default());
    let harness = Harness::with_store(
        Setup::on(&BITCOIN),
        Arc::new(MemoryCursorStore::new()),
        Arc::<ControlledCursorStore>::clone(&cursors),
    )
    .unwrap();
    let wallet = Wallet::new(&BITCOIN, kind_of(&BITCOIN), &[FUNDING_SAT]).unwrap();
    wallet.ledger().serve(&harness);
    let addresses = [wallet.address()];
    harness.watch(&addresses).await.unwrap();
    assert_eq!(cursors.scan_read_count(), 1, "initial admission validates coverage");
    harness.watch(&addresses).await.unwrap();
    let (first, second) = tokio::join!(harness.watch(&addresses), harness.watch(&addresses));
    first.unwrap();
    second.unwrap();
    assert_eq!(
        cursors.scan_read_count(),
        1,
        "unchanged concurrent refreshes must not reload admitted cursors"
    );
    harness.sync().await.unwrap();
    assert_eq!(cursors.scan_read_count(), 2, "sync reloads committed coverage");
    cursors.commit().await.unwrap();
    harness.watch(&[]).await.unwrap();
    assert_eq!(cursors.scan_read_count(), 2);
    harness.watch(&addresses).await.unwrap();
    assert_eq!(cursors.scan_read_count(), 3, "re-admission validates again");
    harness.sync().await.unwrap();
    assert_eq!(cursors.scan_read_count(), 4);
}

#[tokio::test]
async fn a_failed_cursor_read_admits_nothing_and_an_unreadable_cursor_blocks_the_chain() {
    let cursors = Arc::new(ControlledCursorStore::default());
    let harness = Harness::with_store(
        Setup::on(&BITCOIN),
        Arc::new(MemoryCursorStore::new()),
        Arc::<ControlledCursorStore>::clone(&cursors),
    )
    .unwrap();
    let wallet = Wallet::new(&BITCOIN, kind_of(&BITCOIN), &[FUNDING_SAT]).unwrap();
    wallet.ledger().serve(&harness);
    cursors.scan_failure().store(true, Ordering::SeqCst);
    harness.watch(&[wallet.address()]).await.unwrap_err();
    cursors.scan_failure().store(false, Ordering::SeqCst);
    harness.watch(&[wallet.address()]).await.unwrap();
    assert_eq!(cursors.scan_read_count(), 2, "failed admission did not publish the watch");
    harness.sync().await.unwrap();
    cursors.commit().await.unwrap();
    cursors
        .committed()
        .advance(CursorScope::Address(7), BITCOIN.scan_stream, CursorBytes::copied(b"malformed"))
        .await
        .unwrap();
    harness.watch(&[wallet.address()]).await.unwrap();
    harness.sync().await.unwrap_err();
    harness.watch(&[]).await.unwrap_err();
    assert!(
        cursors
            .load_scan(CursorScope::Address(7), BITCOIN.scan_stream)
            .await
            .unwrap()
            .unwrap()
            .with_bytes(|bytes| bytes == b"malformed")
    );
}

#[tokio::test]
async fn one_pass_reports_the_balance_the_history_and_the_cursor() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        wallet.ledger().serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();

        let progress = harness.sync().await.unwrap();
        assert_eq!(progress.chain(), params.chain);
        assert_eq!(progress.tip_height(), Some(u64::from(TIP)));
        assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);

        let emitted = harness.drain();
        let records = records(&emitted);
        assert_eq!(records.len(), 1);
        let record = records.first().expect("one detected record");
        assert_eq!(record.txid.value(), wallet.payment.compute_txid().to_string());
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.asset, AssetId::Native(params.chain));
        assert_eq!(record.amount, sats(FUNDING_SAT));
        assert_eq!(
            record.fee,
            Some(sats(10_000)),
            "the funding transaction makes the fee knowable"
        );
        assert_eq!(record.detection, TxDetection::Full);
        assert_eq!(record.counterparty.as_deref(), Some(wallet.theirs.address()));
        assert_eq!(record.confirmations, confirmations(Some(u64::from(MINED_AT)), u64::from(TIP)));
        assert_eq!(record.height, Some(u64::from(MINED_AT)));
        assert_eq!(record.timestamp, Some(i64::from(BLOCK_UNIX_SECONDS)));

        assert!(emitted.iter().any(|event| matches!(
            *event,
            WalletEvent::BalanceChanged(ref update)
                if update.address_id == 7 && update.balance == sats(FUNDING_SAT)
        )));
        assert!(
            !emitted.iter().any(|event| matches!(*event, WalletEvent::TxConfirmed(_))),
            "a record a hundred blocks down is not one anything is waiting on"
        );

        let stored = harness
            .cursors
            .load_scan(CursorScope::Address(7), params.scan_stream)
            .await
            .unwrap()
            .expect("a cursor was written for the address");
        let (balance, coins) = stored.with_bytes(cursor_summary).expect("the cursor decodes");
        assert_eq!(balance, sats(FUNDING_SAT));
        assert_eq!(coins, 1);
    }
}

#[tokio::test]
async fn a_confirmation_is_announced_only_while_the_record_is_still_shallow() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        let shallow = TIP.saturating_sub(2);
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, shallow)
            .unspent(&wallet.payment, 0, FUNDING_SAT, shallow)
            .serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();
        harness.sync().await.unwrap();

        let emitted = harness.drain();
        let confirmed: Vec<(u32, bool)> = emitted
            .iter()
            .filter_map(|event| match *event {
                WalletEvent::TxConfirmed(ref confirmation) => {
                    Some((confirmation.confirmations, confirmation.txid.chain() == params.chain))
                }
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxUpdated(_)
                | WalletEvent::TxDetected(_)
                | WalletEvent::SyncProgress(_) => None,
            })
            .collect();
        assert_eq!(confirmed, vec![(3, true)], "the tip is two blocks above the record");
    }
}

#[tokio::test]
async fn an_unchanged_history_restores_the_unspent_set_without_refetching() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        harness.forget_calls();

        harness.sync().await.unwrap();
        assert_eq!(harness.calls(Method::GetTransaction), 0, "nothing was refetched");
        assert_eq!(harness.calls(Method::ListUnspent), 0);
        assert!(
            harness.events.records().is_empty(),
            "nothing was refetched, so nothing was reclassified"
        );

        // The restored coins are still spendable.
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .unwrap();
        assert_eq!(unsigned.chain(), params.chain);
    }
}

/// The txids every pass since the last `forget_calls` downloaded, in request
/// order.
fn downloaded(harness: &Harness) -> Vec<String> {
    harness
        .chain
        .server
        .calls_to(Method::GetTransaction)
        .iter()
        .filter_map(|call| {
            serde_json::from_str::<Value>(&call.params).ok()?.get(0)?.as_str().map(str::to_owned)
        })
        .collect()
}

/// A transaction never changes, so a pass downloads what is new to the history
/// and the parents that price it, and reports nothing it reported before.
#[tokio::test]
async fn a_moved_history_downloads_and_reports_only_what_is_new() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let spend = tx_spending(
            &wallet.payment,
            0,
            &[(wallet.theirs.script_pubkey(), 50_000), (wallet.ours.script_pubkey(), 130_000)],
        );
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .confirmed(&spend, MINED_LATER)
            .unspent(&spend, 1, 130_000, MINED_LATER)
            .serve(&harness);
        harness.forget_calls();
        harness.sync().await.unwrap();

        assert_eq!(
            downloaded(&harness),
            vec![spend.compute_txid().to_string(), wallet.payment.compute_txid().to_string()],
            "{}: the new transaction, then the one parent that prices it",
            params.chain
        );
        let emitted = harness.drain();
        let records = records(&emitted);
        assert_eq!(records.len(), 1, "{}: {records:?}", params.chain);
        let record = records.first().expect("the spend");
        assert_eq!(record.txid.value(), spend.compute_txid().to_string());
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.amount, sats(50_000));
        assert_eq!(record.fee, Some(sats(FUNDING_SAT - 50_000 - 130_000)));
    }
}

/// A mempool transaction this session decoded is reported at its height once it
/// is mined, from what was decoded, and its change is the coin already verified.
#[tokio::test]
async fn a_send_this_session_decoded_confirms_without_a_download() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let spend = tx_spending(
            &wallet.payment,
            0,
            &[(wallet.theirs.script_pubkey(), 50_000), (wallet.ours.script_pubkey(), 130_000)],
        );
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .mempool(&spend)
            .unconfirmed(&spend, 1, 130_000)
            .serve(&harness);
        harness.sync().await.unwrap();
        harness.drain();

        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .confirmed(&spend, MINED_LATER)
            .unspent(&spend, 1, 130_000, MINED_LATER)
            .serve(&harness);
        harness.forget_calls();
        harness.sync().await.unwrap();

        assert!(downloaded(&harness).is_empty(), "{}: {:?}", params.chain, downloaded(&harness));
        let emitted = harness.drain();
        let records = records(&emitted);
        assert_eq!(records.len(), 1, "{}: {records:?}", params.chain);
        let record = records.first().expect("the spend");
        assert_eq!(record.txid.value(), spend.compute_txid().to_string());
        assert_eq!(record.height, Some(u64::from(MINED_LATER)));
        assert_eq!(record.timestamp, Some(i64::from(BLOCK_UNIX_SECONDS)));
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.amount, sats(50_000));
        assert_eq!(record.fee, Some(sats(FUNDING_SAT - 50_000 - 130_000)));
        assert!(emitted.iter().any(|event| matches!(
            *event,
            WalletEvent::BalanceChanged(ref update) if update.balance == sats(130_000)
        )));
    }
}

#[tokio::test]
async fn an_address_with_no_history_syncs_to_an_empty_set() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        Ledger::new().serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();
        harness.sync().await.unwrap();

        assert!(harness.drain().iter().any(|event| matches!(
            *event,
            WalletEvent::BalanceChanged(ref update) if update.balance == Amount::ZERO
        )));
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .expect_err("there is nothing to spend");
        assert!(
            matches!(error, ChainError::InsufficientFunds { available, .. } if available == Amount::ZERO),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_short_transaction_batch_fails_the_pass_and_persists_nothing() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        Ledger::new()
            .unfetchable(&wallet.payment, MINED_AT)
            .unspent(&wallet.payment, 0, FUNDING_SAT, MINED_AT)
            .serve(&harness);
        harness.chain.server.always(Method::GetTransaction, Answer::Each(Vec::new()));
        harness.watch(&[wallet.address()]).await.unwrap();

        let outcome = harness.sync().await;
        assert!(
            matches!(outcome, Err(ChainError::EndpointsDown { .. })),
            "{}: a short answer must not become an empty history: {outcome:?}",
            params.chain
        );
        assert!(
            harness
                .cursors
                .load_scan(CursorScope::Address(7), params.scan_stream)
                .await
                .unwrap()
                .is_none(),
            "no cursor may advance past data that never arrived"
        );
        assert!(harness.cursors.is_empty());
    }
}

/// Bitcoin and Litecoin servers share the protocol and the ports, so block zero is
/// what tells them apart.
#[tokio::test]
async fn a_server_on_the_other_chain_is_refused_before_it_is_asked_anything_else() {
    for (params, other) in [(BITCOIN, LITECOIN), (LITECOIN, BITCOIN)] {
        let setup = Setup { genesis_hex: genesis_header(&other), ..Setup::on(&params) };
        let harness = Harness::new(setup).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        wallet.ledger().serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();

        let error = harness
            .sync()
            .await
            .expect_err("a server on the other chain is not a server on this one");
        assert!(matches!(error, ChainError::EndpointsDown { .. }), "{}: {error}", params.chain);
        assert_eq!(harness.calls(Method::GetHistory), 0, "nothing was asked of it");
    }
}

/// Feathercoin and Litecoin Cash share Litecoin's block zero.
///
/// Feathercoin's chainparams assert Litecoin's genesis hash, and `ElectrumX`'s coin table
/// records the same hash for both, whose servers listen on the same default ports.
///
/// Such a server answers height zero like a Litecoin server and every later height
/// with a block Litecoin never mined, here a block one on the shared block zero. The
/// checkpoint has to refuse it, as Bitcoin's refuses a Bitcoin Cash server, or it
/// reports an empty history for every Litecoin address.
#[tokio::test]
async fn a_server_on_a_chain_that_shares_litecoins_block_zero_is_refused_at_the_checkpoint() {
    let elsewhere = Header {
        version: Version::ONE,
        prev_blockhash: LITECOIN.electrum_chain().genesis,
        merkle_root: TxMerkleNode::from_byte_array([0x46; 32]),
        time: 1_366_156_800,
        bits: CompactTarget::from_consensus(0x1e0f_fff0),
        nonce: 0,
    };
    let setup =
        Setup { checkpoint_hex: Some(hex::encode(serialize(&elsewhere))), ..Setup::on(&LITECOIN) };
    assert_eq!(setup.genesis_hex, genesis_header(&LITECOIN), "block zero agrees");
    let harness = Harness::new(setup).unwrap();
    let wallet = Wallet::new(&LITECOIN, kind_of(&LITECOIN), &[FUNDING_SAT]).unwrap();
    Ledger::new().serve(&harness);
    harness.watch(&[wallet.address()]).await.unwrap();

    let outcome = harness.sync().await;
    assert!(
        matches!(outcome, Err(ChainError::EndpointsDown { .. })),
        "a server whose chain parts from Litecoin's after block zero should be refused, but the \
         pass ran against it and returned {outcome:?}"
    );
    assert_eq!(
        harness.calls(Method::GetHistory),
        0,
        "the other chain's server was asked for the wallet's history"
    );
}

#[tokio::test]
async fn an_unspent_entry_the_funding_transaction_does_not_bear_out_is_dropped() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, ScriptKind::P2pkh, &[FUNDING_SAT]).unwrap();
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .unspent(&wallet.payment, 0, FUNDING_SAT.saturating_mul(1000), MINED_AT)
            .serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();
        harness.sync().await.unwrap();

        assert!(
            harness.drain().iter().any(|event| matches!(
                *event,
                WalletEvent::BalanceChanged(ref update) if update.balance == Amount::ZERO
            )),
            "an inflated coin is dropped rather than spent"
        );
    }
}

#[tokio::test]
async fn a_pass_releases_the_claims_whose_coins_have_been_spent() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .unwrap();
        assert_eq!(claimed(&harness.adapter).len(), 1);

        let spend = tx_spending(&wallet.payment, 0, &[(wallet.theirs.script_pubkey(), 100_000)]);
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .confirmed(&spend, MINED_LATER)
            .serve(&harness);

        harness.sync().await.unwrap();
        assert!(claimed(&harness.adapter).is_empty(), "the coin is gone, so the claim is too");
    }
}

#[tokio::test]
async fn a_built_transaction_keeps_its_claim_across_a_watch_set_replacement() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let send = harness.request(wallet.theirs.address(), 50_000, "2");
        harness.adapter.build_tx(&send).await.unwrap();
        assert_eq!(claimed(&harness.adapter).len(), 1);

        harness.watch(&[wallet.address()]).await.unwrap();
        assert_eq!(claimed(&harness.adapter).len(), 1, "the same watch set, the same claim");
        let error = harness
            .adapter
            .build_tx(&send)
            .await
            .expect_err("a coin committed to a pending transaction is not spendable twice");
        assert!(
            matches!(error, ChainError::InsufficientFunds { available, .. } if available == Amount::ZERO),
            "{error}"
        );
    }
}

#[tokio::test]
async fn an_address_the_watch_set_lost_can_no_longer_send() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .unwrap();

        harness.watch(&[]).await.unwrap();
        let error = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .expect_err("the address is no longer watched");
        assert!(
            matches!(error, ChainError::InvalidInput(ref message, _)
                if message == "This address is no longer available in this wallet."),
            "{}: {error}",
            params.chain
        );
    }
}

#[tokio::test]
async fn an_asset_from_another_chain_is_refused() {
    for (params, other) in [(BITCOIN, LITECOIN), (LITECOIN, BITCOIN)] {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let mut foreign = harness.request(wallet.theirs.address(), 50_000, "2");
        foreign.asset = AssetId::Native(other.chain);
        let error = harness.adapter.quote_fee(&foreign).await.expect_err("another chain's coin");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error}");
        assert!(error.to_string().contains(params.chain.display_name()), "{error}");
    }
}

/// A sink that applies backpressure forever to one address's records, the way a
/// full engine queue does while a deadline runs out.
#[derive(Debug)]
struct StallingSink {
    stalled_address_id: i64,
    inner: FakeEventSink,
}

#[async_trait]
impl EventSink for StallingSink {
    async fn emit(&self, event: WalletEvent) {
        if matches!(event, WalletEvent::TxDetected(ref record) if record.address_id == self.stalled_address_id)
        {
            pending::<()>().await;
        }
        self.inner.emit(event).await;
    }

    fn try_emit(&self, event: WalletEvent) {
        self.inner.try_emit(event);
    }
}

/// A pass cut after one address's cursor advanced leaves that cursor ahead of the
/// adapter's state, and the next pass reads the address as unchanged.
#[tokio::test(start_paused = true)]
async fn a_pass_cut_after_one_cursor_advanced_never_leaves_a_build_on_spent_coins() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        let other = harness.key(kind_of(&params), 3).unwrap();
        let addresses = [wallet.address(), watched(&params, 8, other.address())];
        wallet.ledger().serve(&harness);
        harness.watch(&addresses).await.unwrap();
        harness.sync().await.unwrap();

        // The coin is spent, with the change coming back to the same address.
        let spend = tx_spending(
            &wallet.payment,
            0,
            &[(wallet.theirs.script_pubkey(), 50_000), (wallet.ours.script_pubkey(), 130_000)],
        );
        let moved = Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .confirmed(&spend, MINED_LATER)
            .unspent(&spend, 1, 130_000, MINED_LATER);
        moved.serve(&harness);
        let stalling = StallingSink { stalled_address_id: 8, inner: FakeEventSink::new() };
        let cut = harness
            .adapter
            .sync(harness.clock.deadline_in(Duration::from_millis(300)), &stalling)
            .await;
        assert!(cut.is_err(), "{}: the pass was cut at the second address", params.chain);
        let stored = harness
            .cursors
            .load_scan(CursorScope::Address(7), params.scan_stream)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.with_bytes(cursor_summary).map(|(balance, _)| balance),
            Some(sats(130_000)),
            "the first address's cursor already holds the change"
        );

        moved.serve(&harness);
        harness.sync().await.unwrap();
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), 50_000, "2"))
            .await
            .unwrap();
        let spent: Vec<OutPoint> =
            payload(&unsigned).unwrap().tx.input.iter().map(|txin| txin.previous_output).collect();
        assert_eq!(
            spent,
            vec![OutPoint { txid: spend.compute_txid(), vout: 1 }],
            "{}: the build spends a coin the chain has already spent",
            params.chain
        );
    }
}

#[tokio::test]
async fn a_quiet_pass_keeps_the_depths_the_previous_one_established() {
    // Bitcoin stands in for every table that replaces.
    let (harness, wallet) =
        synced(Setup::on(&BITCOIN), kind_of(&BITCOIN), &[FUNDING_SAT]).await.unwrap();
    let known = TxId::new(BITCOIN.chain, wallet.payment.compute_txid().to_string());
    let bump = harness.adapter.fee_bump().expect("the adapter plans replacements");
    let error = bump.quote_bump(&known, &FeeSelection::NORMAL).await.expect_err("it is in a block");
    assert!(error.to_string().contains("already confirmed"), "{error}");

    harness.sync().await.unwrap();
    let error = bump
        .quote_bump(&known, &FeeSelection::NORMAL)
        .await
        .expect_err("a quiet pass does not forget that");
    assert!(error.to_string().contains("already confirmed"), "{error}");
}

/// A sink whose queue is full whenever `is_full` is set.
///
/// The engine's bounded queue is full this way right after a pass pushed a long run
/// of records with backpressure. A full sink drops what `try_emit` offers and still
/// waits to take what `emit` does.
#[derive(Debug, Default)]
struct FullSink {
    is_full: AtomicBool,
    inner: FakeEventSink,
}

#[async_trait]
impl EventSink for FullSink {
    async fn emit(&self, event: WalletEvent) {
        self.inner.emit(event).await;
    }

    fn try_emit(&self, event: WalletEvent) {
        if !self.is_full.load(Ordering::SeqCst) {
            self.inner.try_emit(event);
        }
    }
}

/// A balance update a full queue dropped has to reach the engine on a later pass.
///
/// The balance of an address only ever leaves through `try_emit`, which a full
/// queue drops. The cursor still advances, so every later pass reads the address as
/// unchanged and the engine keeps whatever balance it had before.
#[tokio::test]
async fn a_balance_update_a_full_queue_dropped_is_sent_again_by_a_later_pass() {
    for params in CHAINS {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        wallet.ledger().serve(&harness);
        harness.watch(&[wallet.address()]).await.unwrap();

        let sink = FullSink { is_full: AtomicBool::new(true), inner: FakeEventSink::new() };
        harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
        sink.is_full.store(false, Ordering::SeqCst);
        for _ in 0..3 {
            harness.adapter.sync(harness.deadline(), &sink).await.unwrap();
        }

        let balances: Vec<Amount> = sink
            .inner
            .drain()
            .iter()
            .filter_map(|event| match *event {
                WalletEvent::BalanceChanged(ref update) if update.address_id == 7 => {
                    Some(update.balance)
                }
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxDetected(_)
                | WalletEvent::TxUpdated(_)
                | WalletEvent::TxConfirmed(_)
                | WalletEvent::SyncProgress(_) => None,
            })
            .collect();
        assert!(
            balances.contains(&sats(FUNDING_SAT)),
            "{}: the address holds {FUNDING_SAT}, but after the first pass's balance update was \
             dropped no later pass reported it again, so the engine keeps a stale balance: \
             {balances:?}",
            params.chain
        );
    }
}
