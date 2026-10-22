use std::fmt;

use serde::{Deserialize, Serialize};

use crate::endpoint::EndpointLabel;

/// How the process reaches the network, as stored in the settings document and sent over
/// IPC.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxySetting {
    System,
    #[serde(rename_all = "camelCase")]
    Socks5h {
        url: String,
    },
    #[serde(rename_all = "camelCase")]
    Http {
        url: String,
    },
}

/// Proxy userinfo is a credential, so `Debug` shows a URL only as its label.
impl fmt::Debug for ProxySetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => f.write_str("System"),
            Self::Socks5h { url } => {
                f.debug_struct("Socks5h").field("label", &EndpointLabel::from_url(url)).finish()
            }
            Self::Http { url } => {
                f.debug_struct("Http").field("label", &EndpointLabel::from_url(url)).finish()
            }
        }
    }
}
