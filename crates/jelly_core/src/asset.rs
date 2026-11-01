use serde::{Deserialize, Serialize};

use crate::chain::ChainId;
use crate::coin::Coin;
use crate::token::TokenId;

/// A native coin or supported token. Decimals belong to the asset, while `Amount` stores
/// base units.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AssetId {
    Native(ChainId),
    Token(TokenId),
}

impl AssetId {
    #[cfg(feature = "test-util")]
    pub const COUNT: usize = ChainId::COUNT + TokenId::COUNT;

    /// Every native coin in [`ChainId::ALL`] order, then every token in [`TokenId::ALL`]
    /// order.
    pub fn all() -> impl Iterator<Item = Self> {
        ChainId::ALL.into_iter().map(Self::Native).chain(TokenId::ALL.into_iter().map(Self::Token))
    }

    pub const fn chain(self) -> ChainId {
        match self {
            Self::Native(chain) => chain,
            Self::Token(token) => token.chain(),
        }
    }

    pub const fn decimals(self) -> u8 {
        match self {
            Self::Native(chain) => chain.native_decimals(),
            Self::Token(token) => token.decimals(),
        }
    }

    pub const fn coin(self) -> Coin {
        match self {
            Self::Native(chain) => chain.coin(),
            Self::Token(token) => token.coin(),
        }
    }

    /// The stable `<kind>:<name>` string the store's `asset` columns hold.
    pub fn key(self) -> String {
        match self {
            Self::Native(chain) => format!("native:{}", chain.as_str()),
            Self::Token(token) => format!("token:{}", token.as_str()),
        }
    }

    /// The inverse of [`AssetId::key`].
    pub fn parse_key(key: &str) -> Option<Self> {
        match key.split_once(':')? {
            ("native", chain) => ChainId::parse(chain).map(Self::Native),
            ("token", token) => TokenId::parse(token).map(Self::Token),
            _ => None,
        }
    }
}
