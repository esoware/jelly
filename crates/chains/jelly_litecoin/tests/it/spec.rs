use std::collections::BTreeSet;

use jelly_core::{ChainId, KeyKind, ScriptKind, Transport};
use jelly_keypair::PathTemplate;
use jelly_litecoin::{ENTRY, PARAMS, SPEC};
use jelly_net::DefaultEndpoints;
use jelly_utxo::test_util;

#[test]
fn the_spec_and_the_table_describe_one_chain() {
    assert_eq!(SPEC.chain, ChainId::Litecoin);
    assert_eq!(ENTRY.spec(), &SPEC);
    assert_eq!(PARAMS.chain, SPEC.chain);
    assert_eq!(PARAMS.codec.chain(), SPEC.chain);
    assert_eq!(SPEC.key_kind, KeyKind::Secp256k1);
    assert_eq!(PARAMS.script_kinds, SPEC.script_kinds);
    assert_eq!(Some(PARAMS.fee_rate_unit), SPEC.fee_rate_unit);
    assert_eq!(PARAMS.bump_method.is_some(), SPEC.can_bump_fee);
    assert_eq!(SPEC.script_kinds.contains(&ScriptKind::P2wpkh), PARAMS.has_segwit);
    assert_eq!(SPEC.fee_rate_unit.is_some_and(|unit| unit.ends_with("/vB")), PARAMS.has_segwit);
    assert_eq!(SPEC.primary_transport, Transport::ElectrumSsl);
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
    const { assert!(!SPEC.has_subaddresses && !SPEC.needs_account_key) };
}

#[test]
fn the_default_path_derives_the_default_flavour_at_the_chains_coin_type() {
    let default_script = SPEC.default_script.unwrap();
    assert!(SPEC.script_kinds.contains(&default_script));
    let first = PathTemplate::parse(SPEC.default_path).unwrap().path_at(0).unwrap();
    assert_eq!(
        first,
        PathTemplate::bip44(default_script.purpose().unwrap(), SPEC.slip44).path_at(0).unwrap()
    );
}

#[test]
fn the_shipped_endpoints_use_exactly_the_specs_transports() {
    let file: DefaultEndpoints = serde_json::from_str(ENTRY.default_endpoints()).unwrap();
    assert_eq!(file.chain, SPEC.chain);
    let used: BTreeSet<Transport> =
        file.endpoints.iter().map(|endpoint| endpoint.transport).collect();
    let declared: BTreeSet<Transport> = SPEC.transports.iter().copied().collect();
    assert_eq!(used, declared);
}

/// The framework's own unit tests run on a copy of this table.
#[test]
fn the_table_matches_the_frameworks_test_copy() {
    assert_eq!(format!("{PARAMS:?}"), format!("{:?}", test_util::LITECOIN));
}
