use std::time::Duration;

use jelly_core::{Amount, ChainAdapter as _, Clock as _, CursorBytes, test_util::FakeEventSink};
use jelly_monero::{
    MoneroConfig,
    test_util::{account_balance, replace_scan, stored_scan},
};

use crate::common::{Fallible, Harness, config, reopen, stranger};

#[tokio::test]
async fn a_restart_keeps_the_balance_without_scanning_again() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();
    assert!(held > Amount::ZERO);

    let before = harness.node.block_calls();
    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    assert_eq!(
        account_balance(&reopened, 0).total(),
        held,
        "the balance is there the moment the wallet unlocks"
    );
    assert_eq!(harness.node.block_calls(), before, "restoring reads no blocks");
}

#[tokio::test]
async fn a_higher_birthday_keeps_the_scan_and_a_lower_one_restarts_it() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();

    let higher =
        reopen(&harness, harness.account.clone(), harness.height.saturating_add(100), config())
            .await
            .unwrap();
    assert_eq!(
        account_balance(&higher, 0).total(),
        held,
        "a birthday above the one already scanned loses nothing"
    );

    let lower =
        reopen(&harness, harness.account.clone(), harness.height.saturating_sub(1), config())
            .await
            .unwrap();
    assert_eq!(
        account_balance(&lower, 0).total(),
        Amount::ZERO,
        "a birthday below it uncovers blocks never scanned, so the scan starts again"
    );
}

#[tokio::test]
async fn a_blob_belonging_to_another_account_is_discarded() {
    let harness = Harness::new().await.unwrap();
    assert!(account_balance(&harness.adapter, 0).total() > Amount::ZERO);
    assert!(stored_scan(&*harness.cursors).await.unwrap().is_some());

    // The blob is bound to its account, so one written for other keys is refused
    // rather than mis-attributed.
    let reopened = reopen(&harness, stranger().unwrap(), harness.height, config()).await.unwrap();
    assert_eq!(
        account_balance(&reopened, 0).total(),
        Amount::ZERO,
        "another account's scan state is not this account's balance"
    );
}

/// A scan blob written back with every output in it twice.
///
/// Before the outputs come magic (8), version (2), account (32), restore (8),
/// next (8), tip (8), lookahead major (4) and minor (4), settled-through (8), the
/// recent block count (4) and forty bytes per block, then the output count (4). The
/// outputs run to the end.
fn with_every_output_twice(blob: &[u8]) -> Fallible<Vec<u8>> {
    fn u32_at(blob: &[u8], offset: usize) -> Option<u32> {
        let bytes: [u8; 4] = blob.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }
    let recent = usize::try_from(u32_at(blob, 82).ok_or("a recent block count")?)?;
    let count_at = 86usize.saturating_add(recent.saturating_mul(40));
    let outputs = u32_at(blob, count_at).ok_or("an output count")?;
    if outputs == 0 {
        return Err("the fixture wallet owns no outputs".into());
    }

    let head = blob.get(..count_at).ok_or("a header")?;
    let tail = blob.get(count_at.saturating_add(4)..).ok_or("an output section")?;
    let mut forged = head.to_vec();
    forged.extend_from_slice(&outputs.saturating_mul(2).to_le_bytes());
    forged.extend_from_slice(tail);
    forged.extend_from_slice(tail);
    Ok(forged)
}

#[tokio::test]
async fn a_blob_that_names_the_same_output_twice_is_discarded() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();
    assert!(held > Amount::ZERO);

    let blob = stored_scan(&*harness.cursors).await.unwrap().expect("a checkpoint was written");
    let forged = blob.with_bytes(with_every_output_twice).unwrap();
    replace_scan(&*harness.cursors, CursorBytes::new(forged)).await.unwrap();

    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    assert_eq!(
        account_balance(&reopened, 0).total(),
        Amount::ZERO,
        "a doubled output is not a doubled balance"
    );

    let events = FakeEventSink::new();
    reopened.sync(harness.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(account_balance(&reopened, 0).total(), held, "and the rescan finds the truth");
}

#[tokio::test]
async fn a_wider_subaddress_window_forces_a_rescan_at_the_next_unlock() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();

    let wider = MoneroConfig { lookahead_major: 20, lookahead_minor: 500, ..config() };
    let reopened = reopen(&harness, harness.account.clone(), harness.height, wider).await.unwrap();
    assert_eq!(
        account_balance(&reopened, 0).total(),
        Amount::ZERO,
        "blocks scanned without the wider window have to be scanned again"
    );

    let events = FakeEventSink::new();
    reopened.sync(harness.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(
        account_balance(&reopened, 0).total(),
        held,
        "and the rescan finds exactly what the narrower window did"
    );
}
