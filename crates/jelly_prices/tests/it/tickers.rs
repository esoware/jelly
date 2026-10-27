use jelly_core::{AssetId, ChainId, TokenId};
use jelly_prices::Ticker;

/// The match is exhaustive, so a new chain or token is a compile error here
/// rather than an asset that silently loses its fiat value.
const fn ticker_for(asset: AssetId) -> Ticker {
    match asset {
        AssetId::Native(ChainId::Bitcoin) => Ticker::Btc,
        AssetId::Native(ChainId::Litecoin) => Ticker::Ltc,
        AssetId::Native(ChainId::Dogecoin) => Ticker::Doge,
        AssetId::Native(ChainId::BitcoinCash) => Ticker::Bch,
        AssetId::Native(ChainId::Zcash) => Ticker::Zec,
        AssetId::Native(
            ChainId::Ethereum | ChainId::Arbitrum | ChainId::Base | ChainId::Optimism,
        )
        | AssetId::Token(
            TokenId::WethErc20
            | TokenId::WethArbitrum
            | TokenId::WethBase
            | TokenId::WethOptimism
            | TokenId::WethBep20
            | TokenId::WethPolygon
            | TokenId::WethAvalanche,
        ) => Ticker::Eth,
        AssetId::Native(ChainId::Bsc) => Ticker::Bnb,
        AssetId::Native(ChainId::Polygon) => Ticker::Pol,
        AssetId::Native(ChainId::Avalanche) => Ticker::Avax,
        AssetId::Native(ChainId::Solana) => Ticker::Sol,
        AssetId::Native(ChainId::Tron) => Ticker::Trx,
        AssetId::Native(ChainId::Xrp) => Ticker::Xrp,
        AssetId::Native(ChainId::Ton) => Ticker::Gram,
        AssetId::Native(ChainId::Monero) => Ticker::Xmr,
        AssetId::Token(
            TokenId::LinkErc20
            | TokenId::LinkArbitrum
            | TokenId::LinkBase
            | TokenId::LinkOptimism
            | TokenId::LinkBep20
            | TokenId::LinkPolygon
            | TokenId::LinkAvalanche
            | TokenId::LinkSpl,
        ) => Ticker::Link,
        AssetId::Token(TokenId::WbtcErc20) => Ticker::Wbtc,
        AssetId::Token(TokenId::UniErc20) => Ticker::Uni,
        AssetId::Token(TokenId::ShibErc20) => Ticker::Shib,
        AssetId::Token(TokenId::EnaErc20) => Ticker::Ena,
        AssetId::Token(TokenId::AaveErc20) => Ticker::Aave,
        AssetId::Token(TokenId::PumpSpl) => Ticker::Pump,
        AssetId::Token(TokenId::OndoErc20) => Ticker::Ondo,
        AssetId::Token(TokenId::PepeErc20) => Ticker::Pepe,
        AssetId::Token(TokenId::ArbErc20 | TokenId::ArbArbitrum) => Ticker::Arb,
        AssetId::Token(TokenId::OpOptimism) => Ticker::Op,
        AssetId::Token(TokenId::JupSpl) => Ticker::Jup,
        AssetId::Token(
            TokenId::UsdtErc20
            | TokenId::UsdtArbitrum
            | TokenId::UsdtOptimism
            | TokenId::UsdtBep20
            | TokenId::UsdtPolygon
            | TokenId::UsdtAvalanche
            | TokenId::UsdtSpl
            | TokenId::UsdtTrc20
            | TokenId::UsdtJetton,
        ) => Ticker::Usdt,
        AssetId::Token(
            TokenId::UsdcErc20
            | TokenId::UsdcArbitrum
            | TokenId::UsdcBase
            | TokenId::UsdcOptimism
            | TokenId::UsdcBep20
            | TokenId::UsdcPolygon
            | TokenId::UsdcAvalanche
            | TokenId::UsdcSpl,
        ) => Ticker::Usdc,
        AssetId::Token(
            TokenId::UsdsErc20
            | TokenId::UsdsArbitrum
            | TokenId::UsdsBase
            | TokenId::UsdsOptimism
            | TokenId::UsdsAvalanche
            | TokenId::UsdsSpl,
        ) => Ticker::Usds,
        AssetId::Token(TokenId::DaiErc20) => Ticker::Dai,
        AssetId::Token(TokenId::UsdgErc20 | TokenId::UsdgArbitrum | TokenId::UsdgSpl) => {
            Ticker::Usdg
        }
        AssetId::Token(
            TokenId::PyusdErc20
            | TokenId::PyusdArbitrum
            | TokenId::PyusdPolygon
            | TokenId::PyusdSpl,
        ) => Ticker::Pyusd,
        AssetId::Token(
            TokenId::XautErc20
            | TokenId::XautArbitrum
            | TokenId::XautBep20
            | TokenId::XautPolygon
            | TokenId::XautAvalanche
            | TokenId::XautJetton,
        ) => Ticker::Xaut,
        AssetId::Token(TokenId::PaxgErc20 | TokenId::PaxgSpl) => Ticker::Paxg,
    }
}

#[test]
fn the_tickers_cover_the_assets_exactly() {
    for asset in AssetId::all() {
        let ticker = ticker_for(asset);
        assert!(ticker.assets().contains(&asset), "{ticker} does not claim {asset:?}");
        let claimed: Vec<Ticker> =
            Ticker::ALL.into_iter().filter(|other| other.assets().contains(&asset)).collect();
        assert_eq!(claimed, vec![ticker], "{asset:?} is claimed by more than one ticker");
    }
    let covered: usize = Ticker::ALL.iter().map(|ticker| ticker.assets().len()).sum();
    assert_eq!(covered, AssetId::COUNT, "a ticker claims one asset twice");
}

/// Wrapped ether is the one asset priced under another coin's code.
#[test]
fn a_ticker_is_the_market_code_of_every_asset_it_prices() {
    for ticker in Ticker::ALL {
        assert!(!ticker.assets().is_empty(), "{ticker} prices nothing");
        for &asset in ticker.assets() {
            let code = if asset.symbol() == "WETH" { "ETH" } else { asset.symbol() };
            assert_eq!(ticker.as_str(), code, "{asset:?}");
        }
    }
}

#[test]
fn no_two_tickers_share_a_code() {
    for (at, ticker) in Ticker::ALL.into_iter().enumerate() {
        let twin = Ticker::ALL
            .into_iter()
            .skip(at.saturating_add(1))
            .find(|other| other.as_str() == ticker.as_str());
        assert_eq!(twin, None, "{ticker} appears twice");
    }
}
