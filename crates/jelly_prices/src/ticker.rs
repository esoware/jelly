use jelly_core::{AssetId, Coin, wire_enum};

wire_enum! {
    /// A base asset the wallet asks a price for, always against the US dollar.
    ///
    /// Every asset trades under its coin's ticker, so a coin's instances on different
    /// chains share one quote. Wrapped ether uses ether's, because Kraken lists no market
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
    pub(crate) const fn for_coin(coin: Coin) -> Self {
        match coin {
            Coin::Btc => Self::Btc,
            Coin::Ltc => Self::Ltc,
            Coin::Doge => Self::Doge,
            Coin::Bch => Self::Bch,
            Coin::Zec => Self::Zec,
            Coin::Eth | Coin::Weth => Self::Eth,
            Coin::Bnb => Self::Bnb,
            Coin::Pol => Self::Pol,
            Coin::Avax => Self::Avax,
            Coin::Sol => Self::Sol,
            Coin::Trx => Self::Trx,
            Coin::Xrp => Self::Xrp,
            Coin::Gram => Self::Gram,
            Coin::Xmr => Self::Xmr,
            Coin::Link => Self::Link,
            Coin::Wbtc => Self::Wbtc,
            Coin::Uni => Self::Uni,
            Coin::Shib => Self::Shib,
            Coin::Ena => Self::Ena,
            Coin::Aave => Self::Aave,
            Coin::Ondo => Self::Ondo,
            Coin::Pepe => Self::Pepe,
            Coin::Arb => Self::Arb,
            Coin::Op => Self::Op,
            Coin::Pump => Self::Pump,
            Coin::Jup => Self::Jup,
            Coin::Usdt => Self::Usdt,
            Coin::Usdc => Self::Usdc,
            Coin::Usds => Self::Usds,
            Coin::Dai => Self::Dai,
            Coin::Usdg => Self::Usdg,
            Coin::Pyusd => Self::Pyusd,
            Coin::Xaut => Self::Xaut,
            Coin::Paxg => Self::Paxg,
        }
    }

    /// Every asset one quote for this ticker prices, in [`AssetId::all`] order.
    pub fn assets(self) -> impl Iterator<Item = AssetId> {
        AssetId::all().filter(move |asset| Self::for_coin(asset.coin()) == self)
    }
}
