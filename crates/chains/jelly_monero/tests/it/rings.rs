use jelly_core::{ChainAdapter as _, SignedTx};
use jelly_monero::test_util::stored_sends;
use monero_wallet::WalletOutput;

use crate::common::{Fallible, Harness, config, reopen, rings_in};

/// Builds and signs a payment of `amount` without broadcasting it, as a user who
/// backs out of the review does.
async fn abandoned(harness: &Harness, amount: u64) -> Fallible<SignedTx> {
    let unsigned = harness.adapter.build_tx(&harness.request(Some(amount))).await?;
    Ok(harness.adapter.sign_tx(&unsigned, &[harness.spend_key()])?)
}

#[tokio::test]
async fn a_rebuild_offers_each_output_the_ring_it_was_first_offered_with() {
    let harness = Harness::new().await.unwrap();
    let first = rings_in(&abandoned(&harness, 1_000).await.unwrap()).unwrap();
    assert_eq!(first.len(), 1, "one input");
    assert_eq!(first[0].len(), 16);

    let again = rings_in(&abandoned(&harness, 2_000).await.unwrap()).unwrap();
    assert_eq!(again, first, "the same output goes out with the same ring");

    let reopened =
        reopen(&harness, harness.account.clone(), harness.height, config()).await.unwrap();
    let unsigned = reopened.build_tx(&harness.request(Some(3_000))).await.unwrap();
    let signed = reopened.sign_tx(&unsigned, &[harness.spend_key()]).unwrap();
    assert_eq!(rings_in(&signed).unwrap(), first, "the ring outlives a lock and unlock");
}

#[tokio::test]
async fn a_ring_is_durable_before_the_transaction_carrying_it_leaves_the_adapter() {
    let harness = Harness::new().await.unwrap();
    assert!(stored_sends(&*harness.cursors).await.unwrap().is_none(), "nothing reserved yet");
    harness.adapter.build_tx(&harness.request(Some(1_000))).await.unwrap();
    let held = stored_sends(&*harness.cursors).await.unwrap().expect("the ring was stored");
    let images = harness.own_key_images();
    assert!(
        held.with_bytes(|bytes| {
            images.iter().any(|image| bytes.windows(32).any(|window| window == image))
        }),
        "the ring is stored under the key image of the output it surrounds"
    );
}

#[tokio::test]
async fn a_ring_the_chain_no_longer_holds_is_drawn_again_and_kept() {
    let harness = Harness::new().await.unwrap();
    let first = rings_in(&abandoned(&harness, 1_000).await.unwrap()).unwrap();
    let real: Vec<u64> = harness.outputs.iter().map(WalletOutput::index_on_blockchain).collect();
    let decoy = first[0].iter().copied().find(|position| !real.contains(position)).unwrap();
    harness.node.drop_member(decoy);

    let redrawn = rings_in(&abandoned(&harness, 1_000).await.unwrap()).unwrap();
    assert_ne!(redrawn, first, "a ring with a member the node no longer has is not reused");
    assert!(!redrawn[0].contains(&decoy));
    assert!(redrawn[0].iter().any(|position| real.contains(position)), "it still spends ours");

    let after = rings_in(&abandoned(&harness, 1_000).await.unwrap()).unwrap();
    assert_eq!(after, redrawn, "the ring drawn in its place is the one offered from then on");
}

#[tokio::test]
async fn reusing_a_ring_asks_only_the_node_the_distribution_came_from() {
    let harness = Harness::new().await.unwrap();
    abandoned(&harness, 1_000).await.unwrap();
    let asked = harness.node.ring_endpoints().len();
    abandoned(&harness, 1_000).await.unwrap();
    let endpoints = harness.node.ring_endpoints();
    assert_eq!(endpoints.len(), asked.saturating_add(1), "one request validates the ring");
    assert!(endpoints.windows(2).all(|pair| pair[0] == pair[1]));
}
