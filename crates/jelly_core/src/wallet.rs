use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Whether a wallet uses a BIP-39 passphrase and, if so, where it lives.
    pub enum PassphrasePolicy {
        None => "none",
        /// In use and kept in the vault, so unlock never asks for it.
        Stored => "stored",
        /// In use and never persisted, so every unlock asks for it.
        Prompt => "prompt",
    }
}

/// Opaque sRGB stored as lowercase `#rrggbb`. Display balancing doesn't change the saved
/// color.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct WalletColor(String);

impl WalletColor {
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self(text.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WalletColor {
    fn default() -> Self {
        Self(String::from("#71717a"))
    }
}

impl<'de> Deserialize<'de> for WalletColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom("Enter a six-digit hex color."))
    }
}
