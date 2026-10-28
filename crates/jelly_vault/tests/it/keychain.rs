use std::process;

use jelly_vault::{KeyringStore, MasterKey, MasterKeyStore as _};

/// A headless or locked session can't serve the platform store.
#[test]
#[ignore = "writes to the platform credential store"]
fn the_platform_store_round_trips_the_master_key() {
    let store = KeyringStore;
    store.check_available().expect("the platform credential store is available");

    let wallet_id = format!("jelly-test-wallet-{}", process::id());
    store.delete(&wallet_id).expect("pre-clean");
    assert!(store.get(&wallet_id).expect("get on empty").is_none());

    let key = MasterKey::from_bytes([0x3c; 32]);
    let expected = key.with_bytes(|bytes| *bytes);
    store.put(&wallet_id, &key).expect("put");
    let got = store.get(&wallet_id).expect("get").expect("present");
    assert_eq!(got.with_bytes(|bytes| *bytes), expected, "the master key survived byte for byte");

    store.delete(&wallet_id).expect("delete");
    assert!(store.get(&wallet_id).expect("get after delete").is_none());
}
