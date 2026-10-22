//! The TRC-20 tokens this wallet carries. Symbols and decimals come from the
//! compiled [`TokenId`] table, never from the chain.

use jelly_core::{AssetId, TokenId};

use crate::config::CHAIN;
use crate::error::TronError;

pub(crate) const TOKENS: [TokenId; 1] = [TokenId::UsdtTrc20];

/// The token a contract address names, if this build carries it.
pub(crate) fn token_at(contract: &str) -> Option<TokenId> {
    TOKENS.into_iter().find(|token| token.contract() == contract)
}

/// The token an asset names, or `None` for native TRX. An asset this build does
/// not carry on TRON is refused here rather than failing later.
pub(crate) fn asset_token(asset: AssetId) -> Result<Option<TokenId>, TronError> {
    match asset {
        AssetId::Native(CHAIN) => Ok(None),
        AssetId::Token(token) if TOKENS.contains(&token) => Ok(Some(token)),
        other @ (AssetId::Native(_) | AssetId::Token(_)) => {
            Err(TronError::ForeignAsset { asset: other })
        }
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;

    use super::*;

    #[test]
    fn the_token_table_is_total_over_the_assets_this_chain_serves() {
        for token in TOKENS {
            assert_eq!(token.chain(), ChainId::Tron);
            assert_eq!(token_at(token.contract()), Some(token));
            assert_eq!(asset_token(AssetId::Token(token)).expect("a tron token"), Some(token));
        }
        assert_eq!(asset_token(AssetId::Native(ChainId::Tron)).expect("native"), None);
        assert!(matches!(
            asset_token(AssetId::Token(TokenId::UsdtErc20)),
            Err(TronError::ForeignAsset { .. })
        ));
        assert!(matches!(
            asset_token(AssetId::Native(ChainId::Ethereum)),
            Err(TronError::ForeignAsset { .. })
        ));
        assert_eq!(token_at("TQLCsShbQNXMTVCjprY64qZmEA4rBarpQp"), None);
    }
}
