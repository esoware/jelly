use serde::Serialize;

#[expect(
    clippy::expect_used,
    reason = "a fixture helper outside a #[test] body is still test code and must fail loudly"
)]
pub(crate) fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize")
}
