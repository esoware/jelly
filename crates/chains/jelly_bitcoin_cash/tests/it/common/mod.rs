use jelly_utxo::test_util::Vectors;

const VECTORS_JSON: &str = include_str!("../../vectors/addresses.json");

pub(crate) fn vectors() -> serde_json::Result<Vectors> {
    serde_json::from_str(VECTORS_JSON)
}
