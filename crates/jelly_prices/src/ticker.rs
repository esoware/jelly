use jelly_core::{AssetId, wire_enum};

wire_enum! {
    /// A base asset the wallet asks a price for, always against the US dollar.
    ///
    /// An asset trades under the ticker its symbol names, so a coin's instances on
    /// different chains share one quote. Wrapped ether uses ether's, because Kraken lists
    /// no market of its own for it and it redeems one for one.
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
        Self::parse(match asset.symbol() {
            "WETH" => "ETH",
            symbol => symbol,
        })
    }

    /// Every asset one quote for this ticker prices, in [`AssetId::all`] order.
    pub fn assets(self) -> impl Iterator<Item = AssetId> {
        AssetId::all().filter(move |&asset| Self::for_asset(asset) == Some(self))
    }
}
