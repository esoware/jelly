use crate::amount::Amount;
use crate::coin::Coin;
use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// The supported chains. The database and IPC use these wire strings.
    ///
    /// The wallet lists chains as declared, so the rollups that pay fees in ether follow
    /// Ethereum.
    pub enum ChainId {
        Bitcoin => "bitcoin",
        Litecoin => "litecoin",
        Dogecoin => "dogecoin",
        BitcoinCash => "bitcoin_cash",
        Zcash => "zcash",
        Ethereum => "ethereum",
        Arbitrum => "arbitrum",
        Base => "base",
        Optimism => "optimism",
        Bsc => "bsc",
        Polygon => "polygon",
        Avalanche => "avalanche",
        Solana => "solana",
        Tron => "tron",
        Xrp => "xrp",
        Ton => "ton",
        Monero => "monero",
    }
}

impl ChainId {
    pub const fn is_utxo(self) -> bool {
        match self {
            Self::Bitcoin | Self::Litecoin | Self::Dogecoin | Self::BitcoinCash | Self::Zcash => {
                true
            }
            Self::Ethereum
            | Self::Arbitrum
            | Self::Base
            | Self::Optimism
            | Self::Bsc
            | Self::Polygon
            | Self::Avalanche
            | Self::Solana
            | Self::Tron
            | Self::Xrp
            | Self::Ton
            | Self::Monero => false,
        }
    }

    pub const fn native_decimals(self) -> u8 {
        match self {
            Self::Bitcoin | Self::Litecoin | Self::Dogecoin | Self::BitcoinCash | Self::Zcash => 8,
            Self::Ethereum
            | Self::Arbitrum
            | Self::Base
            | Self::Optimism
            | Self::Bsc
            | Self::Polygon
            | Self::Avalanche => 18,
            Self::Solana | Self::Ton => 9,
            Self::Tron | Self::Xrp => 6,
            Self::Monero => 12,
        }
    }

    /// The coin the chain pays fees in.
    pub const fn coin(self) -> Coin {
        match self {
            Self::Bitcoin => Coin::Btc,
            Self::Litecoin => Coin::Ltc,
            Self::Dogecoin => Coin::Doge,
            Self::BitcoinCash => Coin::Bch,
            Self::Zcash => Coin::Zec,
            Self::Ethereum | Self::Arbitrum | Self::Base | Self::Optimism => Coin::Eth,
            Self::Bsc => Coin::Bnb,
            Self::Polygon => Coin::Pol,
            Self::Avalanche => Coin::Avax,
            Self::Solana => Coin::Sol,
            Self::Tron => Coin::Trx,
            Self::Xrp => Coin::Xrp,
            Self::Ton => Coin::Gram,
            Self::Monero => Coin::Xmr,
        }
    }

    /// An amount of the chain's own coin with its symbol, such as `0.0001 BTC`.
    pub fn format_amount(self, amount: Amount) -> String {
        format!("{} {}", amount.format(self.native_decimals()), self.coin().symbol())
    }

    /// Arbitrum One and OP Mainnet take their network's full name, because Arbitrum Nova
    /// and the rest of the Superchain share their brands.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Bitcoin => "Bitcoin",
            Self::Litecoin => "Litecoin",
            Self::Dogecoin => "Dogecoin",
            Self::BitcoinCash => "Bitcoin Cash",
            Self::Zcash => "Zcash",
            Self::Ethereum => "Ethereum",
            Self::Arbitrum => "Arbitrum One",
            Self::Base => "Base",
            Self::Optimism => "OP Mainnet",
            Self::Bsc => "BNB Smart Chain",
            Self::Polygon => "Polygon",
            Self::Avalanche => "Avalanche C-Chain",
            Self::Solana => "Solana",
            Self::Tron => "TRON",
            Self::Xrp => "XRP",
            Self::Ton => "TON",
            Self::Monero => "Monero",
        }
    }
}
