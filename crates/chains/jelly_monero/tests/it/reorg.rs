use jelly_core::{Amount, ChainSyncProgress, test_util::records};
use jelly_monero::{MoneroConfig, test_util::account_balance};
use monero_interface::ScannableBlock;

use crate::common::{
    CHAIN_BLOCKS, ChainBuilder, Fallible, Harness, PRIMARY_ID, balances_announced, base_chain,
    config, recorded, recorded_block,
};

/// A chain sharing the recorded block and `shared` blocks above it, then diverging
/// and running `extra` blocks past the old tip. Returned with its own tip.
fn diverging(
    height: u64,
    shared: u64,
    extra: u64,
    salt: u8,
) -> Fallible<(Vec<ScannableBlock>, u64)> {
    let forked_at = height.saturating_add(1).saturating_add(shared);
    let after = CHAIN_BLOCKS.saturating_sub(shared).saturating_add(extra);
    let blocks = ChainBuilder::new([0; 32])
        .recorded(recorded_block(&recorded()?)?)
        .run(height.saturating_add(1), shared, 1)
        .run(forked_at, after, salt)
        .finish()?;
    Ok((blocks, height.saturating_add(CHAIN_BLOCKS).saturating_add(extra)))
}

/// A chain that shares nothing, replacing even the block the wallet was born at.
fn replaced(height: u64, extra: u64, salt: u8) -> Fallible<(Vec<ScannableBlock>, u64)> {
    let count = CHAIN_BLOCKS.saturating_add(1).saturating_add(extra);
    let blocks = ChainBuilder::new([0; 32]).run(height, count, salt).finish()?;
    Ok((blocks, height.saturating_add(count).saturating_sub(1)))
}

#[tokio::test]
async fn a_shallow_reorganisation_is_located_without_rescanning_the_wallet() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();
    assert!(held > Amount::ZERO, "the payment was found");
    let a_whole_rescan = harness.node.block_calls();

    // The last four blocks are replaced, and four more are mined on top.
    let (blocks, tip) = diverging(harness.height, CHAIN_BLOCKS.saturating_sub(4), 4, 9).unwrap();
    harness.node.serve(blocks);
    harness.node.set_tip(tip);

    let before = harness.node.block_calls();
    let (progress, events) = harness.sync().await.unwrap();
    assert_eq!(progress.synced_height(), Some(tip));
    assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
    assert_eq!(
        account_balance(&harness.adapter, 0).total(),
        held,
        "the payment is in the block both chains share"
    );
    assert!(records(&events).is_empty(), "nothing new happened on either chain");
    let spent = harness.node.block_calls().saturating_sub(before);
    assert!(
        spent < a_whole_rescan,
        "locating a shallow fork cost {spent} requests; a rescan costs {a_whole_rescan}"
    );
}

#[tokio::test]
async fn a_fork_below_the_first_probe_is_found_by_widening_it() {
    // An overlap of one block, so a fork six blocks down needs the doubling walk
    // rather than the first probe.
    let harness =
        Harness::with(MoneroConfig { reorg_overlap_blocks: 1, ..config() }).await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();

    let (blocks, tip) = diverging(harness.height, CHAIN_BLOCKS.saturating_sub(6), 2, 5).unwrap();
    harness.node.serve(blocks);
    harness.node.set_tip(tip);

    let before = harness.node.block_calls();
    let (progress, _) = harness.sync().await.unwrap();
    assert_eq!(progress.synced_height(), Some(tip));
    assert_eq!(account_balance(&harness.adapter, 0).total(), held);
    let spent = harness.node.block_calls().saturating_sub(before);
    assert!(spent >= 3, "the overlap had to widen more than once, and did: {spent}");
}

#[tokio::test]
async fn a_payment_a_reorganisation_removed_stops_counting() {
    let harness = Harness::new().await.unwrap();
    assert!(account_balance(&harness.adapter, 0).total() > Amount::ZERO);

    // A chain that replaces the block the payment was in, which is also the block
    // this wallet was born at, so the fork is below anything the window remembers.
    let (blocks, tip) = replaced(harness.height, 4, 9).unwrap();
    harness.node.serve(blocks);
    harness.node.set_tip(tip);

    let (gave_up, _) = harness.sync().await.unwrap();
    assert_eq!(
        gave_up.synced_height(),
        Some(harness.height),
        "a fork it cannot locate rewinds to the wallet birthday"
    );
    assert_eq!(
        account_balance(&harness.adapter, 0).total(),
        Amount::ZERO,
        "a payment on a chain that no longer exists is not a balance"
    );

    // The pass after it walks the chain that does exist, and still finds nothing.
    let (rescanned, events) = harness.sync().await.unwrap();
    assert_eq!(rescanned.synced_height(), Some(tip));
    assert!(records(&events).is_empty(), "there is no payment on this chain");
    assert_eq!(account_balance(&harness.adapter, 0).total(), Amount::ZERO);
}

#[tokio::test]
async fn a_chain_that_shrank_resumes_from_its_new_tip() {
    let harness = Harness::new().await.unwrap();
    let shorter = harness.height.saturating_add(50);
    harness.node.set_tip(shorter);
    harness.node.serve(
        ChainBuilder::new([0; 32])
            .recorded(recorded_block(&recorded().unwrap()).unwrap())
            .run(harness.height.saturating_add(1), 50, 1)
            .finish()
            .unwrap(),
    );

    let (progress, _) = harness.sync().await.unwrap();
    assert_eq!(progress.synced_height(), Some(shorter));
    assert_eq!(progress.tip_height(), Some(shorter));
    assert!(
        account_balance(&harness.adapter, 0).total() > Amount::ZERO,
        "the payment is still where it was"
    );
}

/// A chain that grew on top of the last scanned block is read from where the
/// last pass stopped, because the first new block's parent proves nothing under
/// it changed.
#[tokio::test]
async fn a_pass_over_the_same_chain_downloads_only_the_new_blocks() {
    let harness = Harness::new().await.unwrap();
    let held = account_balance(&harness.adapter, 0).total();
    for round in 0..3u64 {
        let grown = CHAIN_BLOCKS.saturating_add(round.saturating_mul(2)).saturating_add(2);
        harness.node.serve(
            ChainBuilder::new([0; 32])
                .recorded(recorded_block(&recorded().unwrap()).unwrap())
                .run(harness.height.saturating_add(1), grown, 1)
                .finish()
                .unwrap(),
        );
        harness.node.set_tip(harness.height.saturating_add(grown));
        let before = harness.node.blocks_served();
        let (_, events) = harness.sync().await.unwrap();
        assert_eq!(
            harness.node.blocks_served().saturating_sub(before),
            2,
            "round {round} downloaded more than the two new blocks"
        );
        assert!(records(&events).is_empty(), "round {round} reported something twice");
        assert_eq!(account_balance(&harness.adapter, 0).total(), held);
    }
}

#[tokio::test]
async fn a_payment_a_reorganisation_removed_announces_the_empty_balance_it_left() {
    let harness = Harness::new().await.unwrap();
    assert!(account_balance(&harness.adapter, 0).spendable() > Amount::ZERO);

    let (blocks, tip) = replaced(harness.height, 4, 9).unwrap();
    harness.node.serve(blocks);
    harness.node.set_tip(tip);
    let (_, mut events) = harness.sync().await.unwrap();
    let (_, rescanned) = harness.sync().await.unwrap();
    events.extend(rescanned);

    assert_eq!(account_balance(&harness.adapter, 0).total(), Amount::ZERO);
    assert_eq!(
        balances_announced(&events, PRIMARY_ID).last(),
        Some(&Amount::ZERO),
        "the interface was last told the address held the vanished payment"
    );
}

/// A lower tip from a lagging node undoes no block that was never reorganised.
///
/// A node one block behind the one the last pass read reports a lower tip, while
/// every block it does hold is a block this wallet already scanned. Nothing about
/// the chain changed, so nothing already scanned may be undone.
#[tokio::test]
async fn a_node_one_block_behind_does_not_bring_a_spent_coin_back() {
    let harness = Harness::new().await.unwrap();
    let images = harness.own_key_images();
    let spent = *images.first().expect("the wallet owns an output");
    let landed = harness.tip.saturating_add(1);
    harness.node.serve(
        base_chain(harness.height)
            .unwrap()
            .spending(landed, 1, [0xee; 32], &[spent], 900)
            .finish()
            .unwrap(),
    );
    harness.node.set_tip(landed);
    harness.sync().await.unwrap();
    let one_output = Amount::from(harness.recorded.output_amount);
    assert_eq!(account_balance(&harness.adapter, 0).total(), one_output, "one coin was spent");

    // The next tip query reaches a node that has not received block `landed` yet.
    harness.node.set_tip(harness.tip);
    harness.sync().await.unwrap();
    let after = account_balance(&harness.adapter, 0);
    assert_eq!(
        after.total(),
        one_output,
        "expected the coin spent in block {landed} to stay spent after a node one block behind \
         answered, but the wallet now holds {:?} with {:?} spendable",
        after.total(),
        after.spendable()
    );
}

/// A fork deeper than everything the wallet remembers is rescanned from the birthday
/// with the overlap the adapter ships with.
///
/// Locating it walks further back on every rewind, and a walk that runs out of rewinds
/// or of remembered blocks gives up on locating the fork and rescans, rather than going
/// on over the new chain as if nothing were wrong.
#[tokio::test]
async fn a_reorganisation_deeper_than_the_window_drops_a_payment_the_new_chain_lacks() {
    let shipped = MoneroConfig::default().reorg_overlap_blocks;
    let harness =
        Harness::with(MoneroConfig { reorg_overlap_blocks: shipped, ..config() }).await.unwrap();
    assert!(account_balance(&harness.adapter, 0).total() > Amount::ZERO, "the payment was found");

    // The same replacement chain the window-sized walk handles, with the overlap the
    // adapter ships with.
    let (blocks, tip) = replaced(harness.height, 4, 9).unwrap();
    harness.node.serve(blocks);
    harness.node.set_tip(tip);
    harness.sync().await.unwrap();
    harness.sync().await.unwrap();

    let after = account_balance(&harness.adapter, 0);
    assert_eq!(
        after.total(),
        Amount::ZERO,
        "expected a rescan from the birthday to drop the payment the new chain does not \
         contain, but the wallet still counts {:?}",
        after.total()
    );
}
