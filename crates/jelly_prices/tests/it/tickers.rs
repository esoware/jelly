use jelly_core::Coin;
use jelly_prices::Ticker;

/// Wrapped ether is the one coin priced under another coin's ticker.
#[test]
fn every_coin_but_wrapped_ether_trades_under_its_own_ticker() {
    for coin in Coin::ALL {
        let expected = if coin == Coin::Weth { Coin::Eth } else { coin };
        assert_eq!(Ticker::of(coin).coin(), expected, "{coin}");
    }
    assert!(Ticker::all().all(|ticker| ticker.coin() != Coin::Weth));
}

#[test]
fn every_ticker_prices_an_asset() {
    for ticker in Ticker::all() {
        assert!(ticker.assets().next().is_some(), "{ticker:?} prices nothing");
    }
}
