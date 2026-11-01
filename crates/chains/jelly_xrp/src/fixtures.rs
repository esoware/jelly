use crate::error::XrpError;

#[track_caller]
pub(crate) fn assert_refused(error: &XrpError, message: &str) {
    assert!(
        matches!(error, XrpError::Invalid(text) if *text == message),
        "expected {message:?}, got {error:?}"
    );
}
