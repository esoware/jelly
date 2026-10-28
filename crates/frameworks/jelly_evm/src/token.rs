use alloy::primitives::Address;
use jelly_core::{AssetId, ChainId, TokenId};

use crate::error::EvmError;
use crate::params::EvmParams;

/// A supported token resolved to its contract address.
///
/// Symbol and decimals come from the compiled-in registry and never from the
/// chain, because a contract controls what it reports and wrong decimals corrupt
/// every amount shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenMeta {
    id: TokenId,
    address: Address,
}

impl TokenMeta {
    pub(crate) const fn id(self) -> TokenId {
        self.id
    }

    pub(crate) const fn address(self) -> Address {
        self.address
    }
}

/// The tokens one network carries, resolved once per adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenTable {
    chain: ChainId,
    tokens: Vec<TokenMeta>,
}

impl TokenTable {
    /// Parses compiled-in text. A unit test pins every row, so a failure means the
    /// registry was edited wrongly.
    pub(crate) fn for_params(params: &EvmParams) -> Result<Self, EvmError> {
        Ok(Self {
            chain: params.chain,
            tokens: params
                .tokens
                .iter()
                .copied()
                .map(TokenMeta::try_from)
                .collect::<Result<_, _>>()?,
        })
    }

    pub(crate) fn all(&self) -> &[TokenMeta] {
        &self.tokens
    }

    /// The contracts a log filter names, so one window covers every token.
    pub(crate) fn contracts(&self) -> Vec<Address> {
        self.tokens.iter().map(|token| token.address).collect()
    }

    pub(crate) fn at(&self, address: Address) -> Option<TokenMeta> {
        self.tokens.iter().copied().find(|token| token.address == address)
    }

    /// `None` for this network's native asset, and a refusal for an asset of another
    /// network.
    pub(crate) fn of_asset(&self, asset: AssetId) -> Result<Option<TokenMeta>, EvmError> {
        let foreign = || EvmError::ForeignAsset { asset, chain: self.chain };
        match asset {
            AssetId::Native(chain) if chain == self.chain => Ok(None),
            AssetId::Native(_) => Err(foreign()),
            AssetId::Token(id) => self
                .tokens
                .iter()
                .copied()
                .find(|token| token.id == id)
                .map(Some)
                .ok_or_else(foreign),
        }
    }
}

impl TryFrom<TokenId> for TokenMeta {
    type Error = EvmError;

    fn try_from(id: TokenId) -> Result<Self, Self::Error> {
        let address = id
            .contract()
            .parse::<Address>()
            .map_err(|_| EvmError::BadConstant { what: "token contract" })?;
        Ok(Self { id, address })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::MULTICALL3;
    use crate::fixtures::NETWORKS;
    use crate::test_util::ETHEREUM;

    #[test]
    fn the_table_resolves_every_network_and_answers_by_address_and_by_id() {
        assert_eq!(
            MULTICALL3.to_string(),
            "0xcA11bde05977b3631167028862bE2a173976CA11",
            "the checksummed spelling"
        );
        for params in NETWORKS {
            let table = TokenTable::for_params(&params).expect("the registry parses");
            for &token in table.all() {
                assert_eq!(token.address().to_string(), token.id().contract());
                assert_eq!(table.at(token.address()), Some(token));
                assert_eq!(
                    table.of_asset(AssetId::Token(token.id())).expect("present"),
                    Some(token)
                );
            }
            assert_eq!(table.contracts().len(), params.tokens.len());
            assert!(table.at(MULTICALL3).is_none());
        }
    }

    #[test]
    fn an_asset_from_another_network_is_named_rather_than_looked_up() {
        let table = TokenTable::for_params(&ETHEREUM).expect("the registry parses");
        assert_eq!(table.of_asset(AssetId::Native(ChainId::Ethereum)).expect("native"), None);
        assert!(table.of_asset(AssetId::Token(TokenId::UsdtErc20)).expect("token").is_some());
        assert!(matches!(
            table.of_asset(AssetId::Native(ChainId::Bsc)),
            Err(EvmError::ForeignAsset { .. })
        ));
        assert!(matches!(
            table.of_asset(AssetId::Token(TokenId::UsdtBep20)),
            Err(EvmError::ForeignAsset { chain: ChainId::Ethereum, .. })
        ));
    }

    #[test]
    fn the_token_registry_is_checksummed_and_carries_the_published_decimals() {
        let expected = [
            (TokenId::LinkErc20, "0x514910771AF9Ca656af840dff83E8264EcF986CA", 18u8),
            (TokenId::WbtcErc20, "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", 8),
            (TokenId::WethErc20, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", 18),
            (TokenId::UniErc20, "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984", 18),
            (TokenId::ShibErc20, "0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE", 18),
            (TokenId::EnaErc20, "0x57e114B691Db790C35207b2e685D4A43181e6061", 18),
            (TokenId::AaveErc20, "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9", 18),
            (TokenId::OndoErc20, "0xfAbA6f8e4a5E8Ab82F62fe7C39859FA577269BE3", 18),
            (TokenId::PepeErc20, "0x6982508145454Ce325dDbE47a25d4ec3d2311933", 18),
            (TokenId::ArbErc20, "0xB50721BCf8d664c30412Cfbc6cf7a15145234ad1", 18),
            (TokenId::UsdtErc20, "0xdAC17F958D2ee523a2206206994597C13D831ec7", 6),
            (TokenId::UsdcErc20, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", 6),
            (TokenId::UsdsErc20, "0xdC035D45d973E3EC169d2276DDab16f1e407384F", 18),
            (TokenId::DaiErc20, "0x6B175474E89094C44Da98b954EedeAC495271d0F", 18),
            (TokenId::UsdgErc20, "0xe343167631d89B6Ffc58B88d6b7fB0228795491D", 6),
            (TokenId::PyusdErc20, "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8", 6),
            (TokenId::XautErc20, "0x68749665FF8D2d112Fa859AA293F07A622782F38", 6),
            (TokenId::PaxgErc20, "0x45804880De22913dAFE09f4980848ECE6EcbAf78", 18),
            (TokenId::LinkArbitrum, "0xf97f4df75117a78c1A5a0DBb814Af92458539FB4", 18),
            (TokenId::WethArbitrum, "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", 18),
            (TokenId::EnaArbitrum, "0x58538e6A46E07434d7E7375Bc268D3cb839C0133", 18),
            (TokenId::ArbArbitrum, "0x912CE59144191C1204E64559FE8253a0e49E6548", 18),
            (TokenId::UsdtArbitrum, "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9", 6),
            (TokenId::UsdcArbitrum, "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", 6),
            (TokenId::UsdsArbitrum, "0x6491c05A82219b8D1479057361ff1654749b876b", 18),
            (TokenId::UsdgArbitrum, "0x004B506865409877C9fA29bfb1ebA929984B9bbC", 6),
            (TokenId::PyusdArbitrum, "0x46850aD61C2B7d64d08c9C754F45254596696984", 6),
            (TokenId::XautArbitrum, "0x40461291347e1eCbb09499F3371D3f17f10d7159", 6),
            (TokenId::LinkBase, "0x88Fb150BDc53A65fe94Dea0c9BA0a6dAf8C6e196", 18),
            (TokenId::WethBase, "0x4200000000000000000000000000000000000006", 18),
            (TokenId::EnaBase, "0x58538e6A46E07434d7E7375Bc268D3cb839C0133", 18),
            (TokenId::UsdcBase, "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", 6),
            (TokenId::UsdsBase, "0x820C137fa70C8691f0e44Dc420a5e53c168921Dc", 18),
            (TokenId::LinkOptimism, "0x350a791Bfc2C21F9Ed5d10980Dad2e2638ffa7f6", 18),
            (TokenId::WethOptimism, "0x4200000000000000000000000000000000000006", 18),
            (TokenId::EnaOptimism, "0x58538e6A46E07434d7E7375Bc268D3cb839C0133", 18),
            (TokenId::OpOptimism, "0x4200000000000000000000000000000000000042", 18),
            (TokenId::UsdtOptimism, "0x01bFF41798a0BcF287b996046Ca68b395DbC1071", 6),
            (TokenId::UsdcOptimism, "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85", 6),
            (TokenId::UsdsOptimism, "0x4F13a96EC5C4Cf34e442b46Bbd98a0791F20edC3", 18),
            (TokenId::LinkBep20, "0xF8A0BF9cF54Bb92F17374d9e9A321E6a111a51bD", 18),
            (TokenId::WethBep20, "0x2170Ed0880ac9A755fd29B2688956BD959F933F8", 18),
            (TokenId::EnaBep20, "0x58538e6A46E07434d7E7375Bc268D3cb839C0133", 18),
            (TokenId::UsdtBep20, "0x55d398326f99059fF775485246999027B3197955", 18),
            (TokenId::UsdcBep20, "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d", 18),
            (TokenId::XautBep20, "0x21cAef8A43163Eea865baeE23b9C2E327696A3bf", 6),
            (TokenId::LinkPolygon, "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39", 18),
            (TokenId::WethPolygon, "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619", 18),
            (TokenId::UsdtPolygon, "0xc2132D05D31c914a87C6611C10748AEb04B58e8F", 6),
            (TokenId::UsdcPolygon, "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", 6),
            (TokenId::PyusdPolygon, "0x99aF3EeA856556646C98c8B9b2548Fe815240750", 6),
            (TokenId::XautPolygon, "0xF1815bd50389c46847f0Bda824eC8da914045D14", 6),
            (TokenId::LinkAvalanche, "0x5947BB275c521040051D82396192181b413227A3", 18),
            (TokenId::WethAvalanche, "0x49D5c2BdFfac6CE2BFdB6640F4F80f226bc10bAB", 18),
            (TokenId::UsdtAvalanche, "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7", 6),
            (TokenId::UsdcAvalanche, "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E", 6),
            (TokenId::UsdsAvalanche, "0x86Ff09db814ac346a7C6FE2Cd648F27706D1D470", 18),
            (TokenId::XautAvalanche, "0x2775d5105276781B4b85bA6eA6a6653bEeD1dd32", 6),
        ];
        for params in NETWORKS {
            let table = TokenTable::for_params(&params).unwrap();
            for &token in table.all() {
                let row = expected
                    .iter()
                    .find(|(id, _, _)| *id == token.id())
                    .expect("every shipped token has a pinned row");
                assert_eq!(token.address().to_string(), row.1, "the checksummed spelling");
                assert_eq!(token.id().decimals(), row.2);
                assert_eq!(token.id().chain(), params.chain);
            }
        }
        assert_eq!(
            TokenId::UsdtBep20.decimals(),
            18,
            "the binance peg carries eighteen, unlike its ethereum original"
        );
    }
}
