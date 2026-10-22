use jelly_evm::test_util::PAYEE;
use jelly_keypair::test_util::key_at;
use jelly_polygon::ENTRY;

use crate::common::vectors;

#[test]
fn a_key_exports_as_bare_hex_and_imports_back_with_or_without_its_prefix() {
    let suite = vectors().unwrap();
    let exported = ENTRY.export_key(&key_at(&suite.secret_path).unwrap()).unwrap();
    assert_eq!(exported.as_str(), suite.secret);

    let prefixed = format!("0x{}", suite.secret);
    for input in [suite.secret.as_str(), prefixed.as_str()] {
        let imported = ENTRY.import_key(input, None).unwrap();
        assert_eq!(imported.named_script, None);
        assert_eq!(ENTRY.address(&imported.material, None).unwrap(), PAYEE);
        assert_eq!(ENTRY.recognize_key(input), Some("32-byte hex key"));
    }
}

#[test]
fn each_derived_vector_key_reimports_to_its_recorded_address() {
    for row in &vectors().unwrap().derived {
        let key = key_at(&row.path).unwrap();
        let reimported = ENTRY.import_key(&ENTRY.export_key(&key).unwrap(), None).unwrap();
        assert_eq!(ENTRY.address(&reimported.material, None).unwrap(), row.address, "{}", row.path);
    }
}
