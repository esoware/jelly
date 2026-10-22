use jelly_core::ChainAdapter as _;

use crate::common::{Harness, Setup, TABLES, spec};

#[test]
fn the_chain_spec_declares_exactly_the_capabilities_this_adapter_implements() {
    for params in TABLES {
        let harness = Harness::new(Setup::on(&params)).unwrap();
        let adapter = &harness.adapter;
        let metadata = spec(&params).unwrap();

        assert_eq!(adapter.chain(), params.chain);
        assert_eq!(
            adapter.fee_bump().is_some(),
            metadata.can_bump_fee,
            "{}: the interface offers a fee bump exactly when this adapter can plan one",
            params.chain
        );
        assert_eq!(
            adapter.subaddresses().is_some(),
            metadata.has_subaddresses,
            "{}: the interface asks for a subaddress exactly when this adapter derives one",
            params.chain
        );
        assert!(adapter.pending_tracker().is_some());
    }
}
