use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// The supported chains. Endpoint file names and database values use these wire
    /// strings.
    pub enum ChainId {
        Bitcoin => "bitcoin",
        Litecoin => "litecoin",
        Dogecoin => "dogecoin",
        BitcoinCash => "bitcoin_cash",
        Zcash => "zcash",
        Ethereum => "ethereum",
        Bsc => "bsc",
        Polygon => "polygon",
        Arbitrum => "arbitrum",
        Base => "base",
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
            | Self::Bsc
            | Self::Polygon
            | Self::Arbitrum
            | Self::Base
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
            | Self::Bsc
            | Self::Polygon
            | Self::Arbitrum
            | Self::Base
            | Self::Avalanche => 18,
            Self::Solana | Self::Ton => 9,
            Self::Tron | Self::Xrp => 6,
            Self::Monero => 12,
        }
    }

    pub const fn ticker(self) -> &'static str {
        match self {
            Self::Bitcoin => "BTC",
            Self::Litecoin => "LTC",
            Self::Dogecoin => "DOGE",
            Self::BitcoinCash => "BCH",
            Self::Zcash => "ZEC",
            Self::Ethereum | Self::Arbitrum | Self::Base => "ETH",
            Self::Bsc => "BNB",
            Self::Polygon => "POL",
            Self::Avalanche => "AVAX",
            Self::Solana => "SOL",
            Self::Tron => "TRX",
            Self::Xrp => "XRP",
            Self::Ton => "GRAM",
            Self::Monero => "XMR",
        }
    }

    /// User-facing chain name. Do not display the wire string.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Bitcoin => "Bitcoin",
            Self::Litecoin => "Litecoin",
            Self::Dogecoin => "Dogecoin",
            Self::BitcoinCash => "Bitcoin Cash",
            Self::Zcash => "Zcash",
            Self::Ethereum => "Ethereum",
            Self::Bsc => "BNB Smart Chain",
            Self::Polygon => "Polygon",
            Self::Arbitrum => "Arbitrum",
            Self::Base => "Base",
            Self::Avalanche => "Avalanche C-Chain",
            Self::Solana => "Solana",
            Self::Tron => "TRON",
            Self::Xrp => "XRP",
            Self::Ton => "TON",
            Self::Monero => "Monero",
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// The flavours one key's address can take: a UTXO output script type, or the TON
    /// wallet contract that holds the key. The same private key produces a different
    /// address for each.
    pub enum ScriptKind {
        /// BIP-86 taproot.
        P2tr => "p2tr",
        /// BIP-84 native segwit.
        P2wpkh => "p2wpkh",
        /// BIP-49 wrapped segwit.
        P2shP2wpkh => "p2sh_p2wpkh",
        /// BIP-44 legacy.
        P2pkh => "p2pkh",
        TonV5r1 => "ton_v5r1",
        TonV4r2 => "ton_v4r2",
        TonV3r2 => "ton_v3r2",
    }
}

impl ScriptKind {
    /// The BIP-44 purpose component this flavour derives under. A TON wallet contract
    /// derives under the same path whatever its version, so it has none.
    pub const fn purpose(self) -> Option<u32> {
        match self {
            Self::P2tr => Some(86),
            Self::P2wpkh => Some(84),
            Self::P2shP2wpkh => Some(49),
            Self::P2pkh => Some(44),
            Self::TonV5r1 | Self::TonV4r2 | Self::TonV3r2 => None,
        }
    }
}
