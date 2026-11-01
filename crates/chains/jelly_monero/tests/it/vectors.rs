//! Pinned vectors for key images.

use serde::Deserialize;

use crate::common::{account, recorded, scanned};

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
