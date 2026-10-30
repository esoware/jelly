use jelly_core::{AssetId, Coin};

/// A coin the wallet asks a price for, always against the US dollar.
///
/// Every asset trades under its coin's ticker, so a coin's instances on different chains
/// share one quote. Wrapped ether uses ether's, because Kraken lists no market of its own
/// for it and it redeems one for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticker(Coin);

impl Ticker {
    pub const fn of(coin: Coin) -> Self {
        Self(if matches!(coin, Coin::Weth) { Coin::Eth } else { coin })
    }

    /// Every ticker, in [`Coin::ALL`] order.
    pub fn all() -> impl Iterator<Item = Self> {
        Coin::ALL.into_iter().filter(|&coin| Self::of(coin).0 == coin).map(Self)
    }

    pub const fn coin(self) -> Coin {
        self.0
    }

    /// Every asset one quote for this ticker prices, in [`AssetId::all`] order.
    pub fn assets(self) -> impl Iterator<Item = AssetId> {
        AssetId::all().filter(move |asset| Self::of(asset.coin()) == self)
    }
}
