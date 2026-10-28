use jelly_core::AssetId;
use jelly_prices::Ticker;

/// A coin whose symbol no ticker names fails here rather than an asset that
/// silently loses its fiat value.
#[test]
fn every_asset_trades_under_exactly_one_ticker() {
    for asset in AssetId::all() {
        let claimed: Vec<Ticker> = Ticker::ALL
            .into_iter()
            .filter(|ticker| ticker.assets().any(|priced| priced == asset))
            .collect();
        assert_eq!(claimed.len(), 1, "{asset:?} trades under {claimed:?}");
    }
}

/// Wrapped ether is the one asset priced under another coin's code.
#[test]
fn a_ticker_is_the_market_code_of_every_asset_it_prices() {
    for ticker in Ticker::ALL {
        assert!(ticker.assets().next().is_some(), "{ticker} prices nothing");
        for asset in ticker.assets() {
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
