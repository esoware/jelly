use jelly_core::{AddressError, ChainId, KeyError, ScriptKind};
use jelly_evm::test_util::PAYEE;
use jelly_keypair::{
    PathTemplate, stored_path,
    test_util::{TEST_PHRASE, key_at},
};
use jelly_optimism::{ENTRY, SPEC};

use crate::common::vectors;

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    let suite = vectors().unwrap();
    assert_eq!(suite.phrase, TEST_PHRASE, "the vector phrase is the shared fixture");
    for row in &suite.derived {
        assert_eq!(
            ENTRY.address(&key_at(&row.path).unwrap(), None).unwrap(),
            row.address,
            "{}",
            row.path
        );
    }
}

#[test]
fn the_default_path_at_index_zero_derives_its_vector_address() {
    let first = stored_path(&PathTemplate::parse(SPEC.default_path).unwrap().path_at(0).unwrap());
    let suite = vectors().unwrap();
    let row = suite.derived.iter().find(|row| row.path == first.as_str()).unwrap();
    assert_eq!(ENTRY.address(&key_at(first.as_str()).unwrap(), None).unwrap(), row.address);
}

#[test]
fn an_address_has_no_script_type() {
    let key = key_at(&vectors().unwrap().secret_path).unwrap();
    assert!(matches!(
        ENTRY.address(&key, Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Optimism })
    ));
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::Optimism);
    assert_eq!(
        codec.script_pubkey(PAYEE),
        Err(AddressError::NoScript { chain: ChainId::Optimism })
    );
    assert_eq!(codec.script_kind(PAYEE), None);
}

#[test]
fn a_payment_request_pays_its_address_only_on_op_mainnet() {
    for uri in [
        format!("ethereum:{}", PAYEE.to_lowercase()),
        format!("ethereum:pay-{PAYEE}@10?value=1e18"),
        format!("ethereum:{PAYEE}@0xa"),
    ] {
        let address = ENTRY.validate_address(&uri).unwrap();
        assert_eq!(address.value(), PAYEE, "{uri}");
        assert_eq!(address.chain(), ChainId::Optimism, "{uri}");
    }
    assert_eq!(
        ENTRY.validate_address(&format!("ethereum:{PAYEE}@1")),
        Err(AddressError::WrongNetwork { chain: ChainId::Optimism })
    );
    assert_eq!(
        ENTRY.validate_address("0x5aAeb6053f3E94C9b9A09f33669435E7Ef1BeAed"),
        Err(AddressError::Checksum { chain: ChainId::Optimism }),
        "one character's case flipped"
    );
}
