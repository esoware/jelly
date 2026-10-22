//! Published and pinned vectors for subaddress derivation and key images.

use jelly_monero::{MoneroAccount, MoneroKeys};
use serde::Deserialize;

use crate::common::{account, bytes32, recorded, scanned};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddressVectors {
    spend_key: String,
    view_key: String,
    public_spend: String,
    public_view: String,
    primary: String,
    rows: Vec<Subaddress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Subaddress {
    major: u32,
    minor: u32,
    address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyImages {
    spend_key: String,
    cases: Vec<KeyImageCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyImageCase {
    output_key: String,
    key_image: String,
}

#[test]
fn wallet2s_published_subaddresses_are_derived_exactly() {
    let vectors: AddressVectors =
        serde_json::from_str(include_str!("../vectors/subaddresses.json")).unwrap();
    let keys = MoneroKeys::from_spend_bytes(&bytes32(&vectors.spend_key).unwrap());
    assert_eq!(keys.with_view_bytes(|bytes| hex::encode(bytes)), vectors.view_key);
    assert_eq!(hex::encode(keys.public_spend()), vectors.public_spend);
    assert_eq!(hex::encode(keys.public_view()), vectors.public_view);

    let account = MoneroAccount::try_from(keys).unwrap();
    assert_eq!(account.address(0, 0).unwrap(), vectors.primary);
    for row in &vectors.rows {
        assert_eq!(
            account.address(row.major, row.minor).unwrap(),
            row.address,
            "subaddress ({}, {})",
            row.major,
            row.minor
        );
    }
}

#[test]
fn the_test_chain_outputs_derive_the_key_images_upstream_signs_with() {
    let vectors: KeyImages =
        serde_json::from_str(include_str!("../vectors/key_images.json")).unwrap();
    let recorded = recorded().unwrap();
    assert_eq!(vectors.spend_key, recorded.spend_key);
    let account = account(&recorded).unwrap();
    let outputs = scanned(&recorded).unwrap();
    assert_eq!(outputs.len(), vectors.cases.len());
    for (output, case) in outputs.iter().zip(&vectors.cases) {
        assert_eq!(hex::encode(output.key().compress().to_bytes()), case.output_key);
        let image = account.key_image(output);
        assert_eq!(hex::encode(image), case.key_image);
    }
}
