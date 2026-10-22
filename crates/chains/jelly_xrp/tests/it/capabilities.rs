use jelly_core::{ChainAdapter as _, ChainId};
use jelly_xrp::SPEC;

use super::common::Node;

#[tokio::test]
async fn the_chain_spec_declares_exactly_the_capabilities_this_adapter_implements() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();

    assert_eq!(adapter.chain(), ChainId::Xrp);
    assert_eq!(SPEC.chain, ChainId::Xrp);
    assert_eq!(
        adapter.fee_bump().is_some(),
        SPEC.can_bump_fee,
        "the interface offers a fee bump exactly when this adapter can plan one"
    );
    assert_eq!(
        adapter.subaddresses().is_some(),
        SPEC.has_subaddresses,
        "the interface asks for a subaddress exactly when this adapter derives one"
    );
    assert!(adapter.pending_tracker().is_some());
}
