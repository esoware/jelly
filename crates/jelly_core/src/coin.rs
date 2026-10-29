use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// A coin the wallet holds, the same on every chain that carries it: ether on Ethereum
    /// and its rollups, or USDT on every network Tether issues it on.
    pub enum Coin {
        Btc => "btc",
        Ltc => "ltc",
        Doge => "doge",
        Bch => "bch",
        Zec => "zec",
        Eth => "eth",
        Bnb => "bnb",
        Pol => "pol",
        Avax => "avax",
        Sol => "sol",
        Trx => "trx",
        Xrp => "xrp",
        Gram => "gram",
        Xmr => "xmr",
        Link => "link",
        Wbtc => "wbtc",
        Weth => "weth",
        Uni => "uni",
        Shib => "shib",
        Ena => "ena",
        Aave => "aave",
        Ondo => "ondo",
        Pepe => "pepe",
        Arb => "arb",
        Op => "op",
        Pump => "pump",
        Jup => "jup",
        Usdt => "usdt",
        Usdc => "usdc",
        Usds => "usds",
        Dai => "dai",
        Usdg => "usdg",
        Pyusd => "pyusd",
        Xaut => "xaut",
        Paxg => "paxg",
    }
}

impl Coin {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Btc => "BTC",
            Self::Ltc => "LTC",
            Self::Doge => "DOGE",
            Self::Bch => "BCH",
            Self::Zec => "ZEC",
            Self::Eth => "ETH",
            Self::Bnb => "BNB",
            Self::Pol => "POL",
            Self::Avax => "AVAX",
            Self::Sol => "SOL",
            Self::Trx => "TRX",
            Self::Xrp => "XRP",
            Self::Gram => "GRAM",
            Self::Xmr => "XMR",
            Self::Link => "LINK",
            Self::Wbtc => "WBTC",
            Self::Weth => "WETH",
            Self::Uni => "UNI",
            Self::Shib => "SHIB",
            Self::Ena => "ENA",
            Self::Aave => "AAVE",
            Self::Ondo => "ONDO",
            Self::Pepe => "PEPE",
            Self::Arb => "ARB",
            Self::Op => "OP",
            Self::Pump => "PUMP",
            Self::Jup => "JUP",
            Self::Usdt => "USDT",
            Self::Usdc => "USDC",
            Self::Usds => "USDS",
            Self::Dai => "DAI",
            Self::Usdg => "USDG",
            Self::Pyusd => "PYUSD",
            Self::Xaut => "XAUT",
            Self::Paxg => "PAXG",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Btc => "Bitcoin",
            Self::Ltc => "Litecoin",
            Self::Doge => "Dogecoin",
            Self::Bch => "Bitcoin Cash",
            Self::Zec => "Zcash",
            Self::Eth => "Ethereum",
            Self::Bnb => "BNB",
            Self::Pol => "Polygon",
            Self::Avax => "Avalanche",
            Self::Sol => "Solana",
            Self::Trx => "TRON",
            Self::Xrp => "XRP",
            Self::Gram => "Gram",
            Self::Xmr => "Monero",
            Self::Link => "Chainlink",
            Self::Wbtc => "Wrapped Bitcoin",
            Self::Weth => "Wrapped Ether",
            Self::Uni => "Uniswap",
            Self::Shib => "Shiba Inu",
            Self::Ena => "Ethena",
            Self::Aave => "Aave",
            Self::Ondo => "Ondo",
            Self::Pepe => "Pepe",
            Self::Arb => "Arbitrum",
            Self::Op => "Optimism",
            Self::Pump => "Pump.fun",
            Self::Jup => "Jupiter",
            Self::Usdt => "Tether USD",
            Self::Usdc => "USD Coin",
            Self::Usds => "USDS",
            Self::Dai => "Dai",
            Self::Usdg => "Global Dollar",
            Self::Pyusd => "PayPal USD",
            Self::Xaut => "Tether Gold",
            Self::Paxg => "PAX Gold",
        }
    }

    /// A dollar coin or a gold token.
    pub const fn is_real_world_asset(self) -> bool {
        match self {
            Self::Usdt
            | Self::Usdc
            | Self::Usds
            | Self::Dai
            | Self::Usdg
            | Self::Pyusd
            | Self::Xaut
            | Self::Paxg => true,
            Self::Btc
            | Self::Ltc
            | Self::Doge
            | Self::Bch
            | Self::Zec
            | Self::Eth
            | Self::Bnb
            | Self::Pol
            | Self::Avax
            | Self::Sol
            | Self::Trx
            | Self::Xrp
            | Self::Gram
            | Self::Xmr
            | Self::Link
            | Self::Wbtc
            | Self::Weth
            | Self::Uni
            | Self::Shib
            | Self::Ena
            | Self::Aave
            | Self::Ondo
            | Self::Pepe
            | Self::Arb
            | Self::Op
            | Self::Pump
            | Self::Jup => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::TokenId;
    use crate::chain::ChainId;

    #[test]
    fn every_coin_is_some_chains_or_some_tokens() {
        for coin in Coin::ALL {
            let is_native = ChainId::ALL.iter().any(|chain| chain.coin() == coin);
            let is_token = TokenId::ALL.iter().any(|token| token.coin() == coin);
            assert!(is_native || is_token, "{coin}");
        }
    }

    #[test]
    fn only_dollar_and_gold_coins_are_real_world_assets() {
        for coin in Coin::ALL {
            let is_dollar_or_gold = matches!(
                coin.symbol(),
                "USDT" | "USDC" | "USDS" | "DAI" | "USDG" | "PYUSD" | "XAUT" | "PAXG"
            );
            assert_eq!(coin.is_real_world_asset(), is_dollar_or_gold, "{coin}");
        }
    }

    /// A coin takes its network's name unless it has its own, and a rollup's coin is ether,
    /// named as it is on Ethereum. The table lists every chain in declaration order.
    #[test]
    fn every_chain_names_its_own_coin() {
        let expected = [
            (ChainId::Bitcoin, "BTC", "Bitcoin"),
            (ChainId::Litecoin, "LTC", "Litecoin"),
            (ChainId::Dogecoin, "DOGE", "Dogecoin"),
            (ChainId::BitcoinCash, "BCH", "Bitcoin Cash"),
            (ChainId::Zcash, "ZEC", "Zcash"),
            (ChainId::Ethereum, "ETH", "Ethereum"),
            (ChainId::Arbitrum, "ETH", "Ethereum"),
            (ChainId::Base, "ETH", "Ethereum"),
            (ChainId::Optimism, "ETH", "Ethereum"),
            (ChainId::Bsc, "BNB", "BNB"),
            (ChainId::Polygon, "POL", "Polygon"),
            (ChainId::Avalanche, "AVAX", "Avalanche"),
            (ChainId::Solana, "SOL", "Solana"),
            (ChainId::Tron, "TRX", "TRON"),
            (ChainId::Xrp, "XRP", "XRP"),
            (ChainId::Ton, "GRAM", "Gram"),
            (ChainId::Monero, "XMR", "Monero"),
        ];
        assert_eq!(expected.map(|(chain, _, _)| chain), ChainId::ALL);
        for (chain, symbol, name) in expected {
            assert_eq!(chain.coin().symbol(), symbol, "{}", chain.as_str());
            assert_eq!(chain.coin().display_name(), name, "{}", chain.as_str());
        }
    }
}
