use std::fmt;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use percent_encoding::percent_decode_str;
use url::Url;

/// The userinfo of an address, percent-decoded the way the shared HTTP client decodes it,
/// so one address authenticates alike on every transport.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub(crate) username: String,
    pub(crate) password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

impl Credentials {
    /// `None` when `url` carries no userinfo. A username alone has an empty password.
    pub(crate) fn of(url: &Url) -> Option<Self> {
        let has_userinfo = !url.username().is_empty() || url.password().is_some();
        has_userinfo.then(|| Self {
            username: decoded(url.username()),
            password: decoded(url.password().unwrap_or_default()),
        })
    }

    /// The RFC 7617 basic credentials, as an `Authorization` or `Proxy-Authorization`
    /// header value.
    pub(crate) fn basic(&self) -> String {
        let pair = format!("{}:{}", self.username, self.password);
        format!("Basic {}", BASE64_STANDARD.encode(pair))
    }
}

fn decoded(component: &str) -> String {
    percent_decode_str(component).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_decoded_sent_as_basic_and_never_printed() {
        let url = Url::parse("http://us%40er:p%3Ass@proxy.example").expect("a url");
        let credentials = Credentials::of(&url).expect("userinfo");
        assert_eq!(credentials.username, "us@er");
        assert_eq!(credentials.password, "p:ss");
        // Base64 of `us@er:p:ss`.
        assert_eq!(credentials.basic(), "Basic dXNAZXI6cDpzcw==");

        let printed = format!("{credentials:?}");
        assert!(!printed.contains("us@er") && !printed.contains("p:ss"), "{printed}");
    }
}
