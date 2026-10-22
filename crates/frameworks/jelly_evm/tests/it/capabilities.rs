use jelly_core::{ChainAdapter as _, ChainId};
use jelly_ethereum::SPEC;

use crate::common::Harness;

#[test]
fn the_chain_spec_declares_exactly_the_capabilities_this_adapter_implements() {
    let adapter = Harness::ethereum().unwrap();
    let adapter = &adapter.adapter;

    assert_eq!(adapter.chain(), ChainId::Ethereum);
    assert_eq!(
        adapter.fee_bump().is_some(),
        SPEC.can_bump_fee,
        "the interface offers a fee bump exactly when this adapter can plan one"
    );
    assert_eq!(
        adapter.tx_cancel().is_some(),
        SPEC.can_cancel,
        "the interface offers a cancel exactly when this adapter can plan one"
    );
    assert_eq!(
        adapter.subaddresses().is_some(),
        SPEC.has_subaddresses,
        "the interface asks for a subaddress exactly when this adapter derives one"
    );
    assert!(adapter.pending_tracker().is_some());
}
