use jelly_core::{AssetId, ChainId, TokenId, wire_enum};

wire_enum! {
    /// A base asset the wallet asks a price for, always against the US dollar.
    ///
    /// Every [`AssetId`] has exactly one ticker, so the instances of a coin share a
    /// quote across chains. Wrapped ether is ether's, because Kraken lists no market
    /// of its own for it and it redeems one for one.
    pub enum Ticker {
        Btc => "BTC",
        Ltc => "LTC",
        Doge => "DOGE",
        Bch => "BCH",
        Zec => "ZEC",
        Eth => "ETH",
        Bnb => "BNB",
        Pol => "POL",
        Avax => "AVAX",
        Sol => "SOL",
        Trx => "TRX",
        Xrp => "XRP",
        Gram => "GRAM",
        Xmr => "XMR",
        Link => "LINK",
        Wbtc => "WBTC",
        Uni => "UNI",
        Shib => "SHIB",
        Ena => "ENA",
        Aave => "AAVE",
        Pump => "PUMP",
        Ondo => "ONDO",
        Pepe => "PEPE",
        Arb => "ARB",
        Op => "OP",
        Jup => "JUP",
        Usdt => "USDT",
        Usdc => "USDC",
        Usds => "USDS",
        Dai => "DAI",
        Usdg => "USDG",
        Pyusd => "PYUSD",
        Xaut => "XAUT",
        Paxg => "PAXG",
    }
}

impl Ticker {
    pub(crate) fn for_asset(asset: AssetId) -> Option<Self> {
        Self::ALL.into_iter().find(|ticker| ticker.assets().contains(&asset))
    }

    /// Every asset one quote for this ticker prices.
    pub const fn assets(self) -> &'static [AssetId] {
        match self {
            Self::Btc => &[AssetId::Native(ChainId::Bitcoin)],
            Self::Ltc => &[AssetId::Native(ChainId::Litecoin)],
            Self::Doge => &[AssetId::Native(ChainId::Dogecoin)],
            Self::Bch => &[AssetId::Native(ChainId::BitcoinCash)],
            Self::Zec => &[AssetId::Native(ChainId::Zcash)],
            Self::Eth => &[
                AssetId::Native(ChainId::Ethereum),
                AssetId::Native(ChainId::Arbitrum),
                AssetId::Native(ChainId::Base),
                AssetId::Native(ChainId::Optimism),
                AssetId::Token(TokenId::WethErc20),
                AssetId::Token(TokenId::WethArbitrum),
                AssetId::Token(TokenId::WethBase),
                AssetId::Token(TokenId::WethOptimism),
                AssetId::Token(TokenId::WethBep20),
                AssetId::Token(TokenId::WethPolygon),
                AssetId::Token(TokenId::WethAvalanche),
            ],
            Self::Bnb => &[AssetId::Native(ChainId::Bsc)],
            Self::Pol => &[AssetId::Native(ChainId::Polygon)],
            Self::Avax => &[AssetId::Native(ChainId::Avalanche)],
            Self::Sol => &[AssetId::Native(ChainId::Solana)],
            Self::Trx => &[AssetId::Native(ChainId::Tron)],
            Self::Xrp => &[AssetId::Native(ChainId::Xrp)],
            Self::Gram => &[AssetId::Native(ChainId::Ton)],
            Self::Xmr => &[AssetId::Native(ChainId::Monero)],
            Self::Link => &[
                AssetId::Token(TokenId::LinkErc20),
                AssetId::Token(TokenId::LinkArbitrum),
                AssetId::Token(TokenId::LinkBase),
                AssetId::Token(TokenId::LinkOptimism),
                AssetId::Token(TokenId::LinkBep20),
                AssetId::Token(TokenId::LinkPolygon),
                AssetId::Token(TokenId::LinkAvalanche),
                AssetId::Token(TokenId::LinkSpl),
            ],
            Self::Wbtc => &[AssetId::Token(TokenId::WbtcErc20)],
            Self::Uni => &[AssetId::Token(TokenId::UniErc20)],
            Self::Shib => &[AssetId::Token(TokenId::ShibErc20)],
            Self::Ena => &[AssetId::Token(TokenId::EnaErc20)],
            Self::Aave => &[AssetId::Token(TokenId::AaveErc20)],
            Self::Pump => &[AssetId::Token(TokenId::PumpSpl)],
            Self::Ondo => &[AssetId::Token(TokenId::OndoErc20)],
            Self::Pepe => &[AssetId::Token(TokenId::PepeErc20)],
            Self::Arb => &[AssetId::Token(TokenId::ArbErc20), AssetId::Token(TokenId::ArbArbitrum)],
            Self::Op => &[AssetId::Token(TokenId::OpOptimism)],
            Self::Jup => &[AssetId::Token(TokenId::JupSpl)],
            Self::Usdt => &[
                AssetId::Token(TokenId::UsdtErc20),
                AssetId::Token(TokenId::UsdtArbitrum),
                AssetId::Token(TokenId::UsdtOptimism),
                AssetId::Token(TokenId::UsdtBep20),
                AssetId::Token(TokenId::UsdtPolygon),
                AssetId::Token(TokenId::UsdtAvalanche),
                AssetId::Token(TokenId::UsdtSpl),
                AssetId::Token(TokenId::UsdtTrc20),
                AssetId::Token(TokenId::UsdtJetton),
            ],
            Self::Usdc => &[
                AssetId::Token(TokenId::UsdcErc20),
                AssetId::Token(TokenId::UsdcArbitrum),
                AssetId::Token(TokenId::UsdcBase),
                AssetId::Token(TokenId::UsdcOptimism),
                AssetId::Token(TokenId::UsdcBep20),
                AssetId::Token(TokenId::UsdcPolygon),
                AssetId::Token(TokenId::UsdcAvalanche),
                AssetId::Token(TokenId::UsdcSpl),
            ],
            Self::Usds => &[
                AssetId::Token(TokenId::UsdsErc20),
                AssetId::Token(TokenId::UsdsArbitrum),
                AssetId::Token(TokenId::UsdsBase),
                AssetId::Token(TokenId::UsdsOptimism),
                AssetId::Token(TokenId::UsdsAvalanche),
                AssetId::Token(TokenId::UsdsSpl),
            ],
            Self::Dai => &[AssetId::Token(TokenId::DaiErc20)],
            Self::Usdg => &[
                AssetId::Token(TokenId::UsdgErc20),
                AssetId::Token(TokenId::UsdgArbitrum),
                AssetId::Token(TokenId::UsdgSpl),
            ],
            Self::Pyusd => &[
                AssetId::Token(TokenId::PyusdErc20),
                AssetId::Token(TokenId::PyusdArbitrum),
                AssetId::Token(TokenId::PyusdPolygon),
                AssetId::Token(TokenId::PyusdSpl),
            ],
            Self::Xaut => &[
                AssetId::Token(TokenId::XautErc20),
                AssetId::Token(TokenId::XautArbitrum),
                AssetId::Token(TokenId::XautBep20),
                AssetId::Token(TokenId::XautPolygon),
                AssetId::Token(TokenId::XautAvalanche),
                AssetId::Token(TokenId::XautJetton),
            ],
            Self::Paxg => &[AssetId::Token(TokenId::PaxgErc20), AssetId::Token(TokenId::PaxgSpl)],
        }
    }
}
