use serde::Serialize;

#[expect(clippy::expect_used, reason = "test helper")]
pub(crate) fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize")
}
