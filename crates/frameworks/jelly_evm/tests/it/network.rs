use std::slice::from_ref;
use std::sync::Arc;

use jelly_core::{AssetId, ChainAdapter as _, ChainError, ChainId};
use jelly_evm::{EvmError, PooledNode};

use crate::common::{
    BSC, ETHEREUM, Harness, NETWORKS, OWNER, PEER, Setup, http_pool, request, watched,
};

#[test]
fn a_node_over_another_chains_pool_is_refused_at_construction() {
    let solana = http_pool(ChainId::Solana, &["https://one.example"]).unwrap();
    assert!(matches!(
        PooledNode::new(ETHEREUM, solana),
        Err(EvmError::ForeignChain { chain: ChainId::Solana })
    ));

    let bsc = http_pool(ChainId::Bsc, &["https://one.example"]).unwrap();
    assert!(matches!(
        PooledNode::new(ETHEREUM, Arc::clone(&bsc)),
        Err(EvmError::ForeignChain { chain: ChainId::Bsc })
    ));

    let empty = http_pool(ChainId::Ethereum, &[]).unwrap();
    assert!(
        PooledNode::new(ETHEREUM, empty).is_err(),
        "a pool with no endpoint cannot reach anything"
    );

    PooledNode::new(BSC, bsc).unwrap();
}

#[tokio::test]
async fn an_adapter_reports_the_chain_of_the_network_it_was_built_for() {
    for network in NETWORKS {
        let harness = Harness::new(Setup::on(network)).unwrap();
        assert_eq!(harness.adapter.chain(), network.chain);
        assert_eq!(harness.adapter.in_flight(), 0);
        let rendered = format!("{:?}", harness.adapter);
        assert!(rendered.contains(&format!("chain: {:?}", network.chain)), "{rendered}");
    }
}

#[tokio::test]
async fn a_watch_set_from_the_other_evm_network_is_refused() {
    let harness = Harness::new(Setup::on(ETHEREUM)).unwrap();
    let foreign = watched(1, ChainId::Bsc, OWNER);
    let error = harness.watch(&[foreign]).await.expect_err("the other network");
    assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
}

#[tokio::test]
async fn one_address_is_valid_on_both_networks_and_the_asset_is_not() {
    for (network, other) in [(ETHEREUM, BSC), (BSC, ETHEREUM)] {
        let harness = Harness::new(Setup::on(network)).unwrap();
        let row = watched(7, network.chain, OWNER);
        harness.watch(from_ref(&row)).await.unwrap();

        let mut foreign = request(PEER, AssetId::Native(other.chain), 1);
        foreign.from_address_ids = vec![row.id];
        let error =
            harness.adapter.quote_fee(&foreign).await.expect_err("the other network's coin");
        assert!(matches!(error, ChainError::Unsupported(_)), "{error:?}");
        assert!(harness.node.calls().is_empty(), "nothing was asked of the node");
    }
}
