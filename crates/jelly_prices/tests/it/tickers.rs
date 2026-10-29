use jelly_core::Coin;
use jelly_prices::Ticker;

/// Wrapped ether is the one coin priced under another coin's code.
#[test]
fn a_ticker_is_the_symbol_of_every_coin_it_prices() {
    for ticker in Ticker::ALL {
        assert!(ticker.assets().next().is_some(), "{ticker} prices nothing");
        for asset in ticker.assets() {
            let coin = if asset.coin() == Coin::Weth { Coin::Eth } else { asset.coin() };
            assert_eq!(ticker.as_str(), coin.symbol(), "{asset:?}");
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
