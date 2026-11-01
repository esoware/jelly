use std::collections::BTreeSet;

use jelly_core::{AssetId, ChainId, Coin, TokenId};

#[test]
fn every_asset_has_a_unique_stable_key_that_round_trips() {
    let mut seen = BTreeSet::new();
    for asset in AssetId::all() {
        let key = asset.key();
        assert!(seen.insert(key.clone()), "two assets spell themselves {key}");
        assert_eq!(AssetId::parse_key(&key), Some(asset), "{key}");
    }
    assert_eq!(AssetId::Native(ChainId::Bitcoin).key(), "native:bitcoin");
    assert_eq!(AssetId::Token(TokenId::UsdtErc20).key(), "token:usdt_erc20");
    assert_eq!(AssetId::parse_key("native:namecoin"), None);
    assert_eq!(AssetId::parse_key("usdt_erc20"), None);
    assert_eq!(AssetId::parse_key(""), None);
}

#[test]
fn an_asset_takes_its_chain_decimals_and_coin_from_its_chain_or_token() {
    for asset in AssetId::all() {
        match asset {
            AssetId::Native(chain) => {
                assert_eq!(asset.chain(), chain);
                assert_eq!(asset.decimals(), chain.native_decimals());
                assert_eq!(asset.coin(), chain.coin());
            }
            AssetId::Token(token) => {
                assert_eq!(asset.chain(), token.chain());
                assert_eq!(asset.decimals(), token.decimals());
                assert_eq!(asset.coin(), token.coin());
            }
        }
    }
}

#[test]
fn every_token_carries_its_network_specific_decimals() {
    assert_eq!(TokenId::UsdtBep20.decimals(), 18);
    assert_eq!(TokenId::UsdcBep20.decimals(), 18);
    assert_eq!(TokenId::LinkSpl.decimals(), 9, "Solana's LINK is scaled for its own mint");
    assert_eq!(TokenId::EnaSpl.decimals(), 9, "so is Solana's ENA");
    assert_eq!(TokenId::EnaJetton.decimals(), 6, "and TON's ENA has six");
    assert_eq!(TokenId::WbtcErc20.decimals(), 8, "wrapped bitcoin keeps bitcoin's satoshi");
    for token in TokenId::ALL {
        let expected = match (token.coin(), token.chain()) {
            (Coin::Link | Coin::Ena, ChainId::Solana) => 9,
            (Coin::Ena, ChainId::Ton) => 6,
            (Coin::Wbtc, _) => 8,
            (
                Coin::Link
                | Coin::Weth
                | Coin::Uni
                | Coin::Shib
                | Coin::Ena
                | Coin::Aave
                | Coin::Ondo
                | Coin::Pepe
                | Coin::Arb
                | Coin::Op
                | Coin::Dai
                | Coin::Usds
                | Coin::Paxg,
                chain,
            ) if chain != ChainId::Solana => 18,
            (Coin::Usdt | Coin::Usdc, ChainId::Bsc) => 18,
            _ => 6,
        };
        assert_eq!(token.decimals(), expected, "{}", token.as_str());
    }
}

#[test]
fn usdg_and_pyusd_read_as_one_coin_on_every_chain() {
    for token in [TokenId::UsdgErc20, TokenId::UsdgArbitrum, TokenId::UsdgSpl] {
        assert_eq!(token.coin(), Coin::Usdg, "{}", token.as_str());
    }
    for token in
        [TokenId::PyusdErc20, TokenId::PyusdPolygon, TokenId::PyusdArbitrum, TokenId::PyusdSpl]
    {
        assert_eq!(token.coin(), Coin::Pyusd, "{}", token.as_str());
    }
}

#[test]
fn gold_tokens_keep_their_coin_and_network_specific_decimals() {
    for (token, chain, coin, decimals) in [
        (TokenId::PaxgErc20, ChainId::Ethereum, Coin::Paxg, 18),
        (TokenId::PaxgSpl, ChainId::Solana, Coin::Paxg, 6),
        (TokenId::XautErc20, ChainId::Ethereum, Coin::Xaut, 6),
        (TokenId::XautArbitrum, ChainId::Arbitrum, Coin::Xaut, 6),
        (TokenId::XautBep20, ChainId::Bsc, Coin::Xaut, 6),
        (TokenId::XautPolygon, ChainId::Polygon, Coin::Xaut, 6),
        (TokenId::XautAvalanche, ChainId::Avalanche, Coin::Xaut, 6),
        (TokenId::XautSpl, ChainId::Solana, Coin::Xaut, 6),
        (TokenId::XautJetton, ChainId::Ton, Coin::Xaut, 6),
    ] {
        assert_eq!(token.chain(), chain);
        assert_eq!(token.coin(), coin);
        assert_eq!(token.decimals(), decimals);
    }
}

#[test]
fn every_token_names_a_contract_on_its_own_chain() {
    for token in TokenId::ALL {
        let contract = token.contract();
        assert!(!contract.is_empty(), "{}", token.as_str());
        match token.chain() {
            ChainId::Ethereum
            | ChainId::Arbitrum
            | ChainId::Base
            | ChainId::Optimism
            | ChainId::Bsc
            | ChainId::Polygon
            | ChainId::Avalanche => {
                assert!(contract.starts_with("0x"), "{contract}");
                assert_eq!(contract.len(), 42, "{contract}");
            }
            ChainId::Tron => assert!(contract.starts_with('T'), "{contract}"),
            ChainId::Solana => assert!(!contract.starts_with("0x"), "{contract}"),
            ChainId::Ton => {
                assert!(contract.starts_with("EQ"), "{contract}");
                assert_eq!(contract.len(), 48, "{contract}");
            }
            other @ (ChainId::Bitcoin
            | ChainId::Litecoin
            | ChainId::Dogecoin
            | ChainId::BitcoinCash
            | ChainId::Zcash
            | ChainId::Xrp
            | ChainId::Monero) => {
                panic!("{} has no tokens", other.as_str())
            }
        }
    }
}

/// Tether's omnichain `USDT0` and `XAUt0` on Polygon read as the coins they
/// stand for, so a holder sees one USDT and one XAUT across networks.
#[test]
fn polygon_tokens_read_as_the_coins_they_stand_for() {
    for (token, coin) in [
        (TokenId::UsdtPolygon, Coin::Usdt),
        (TokenId::UsdcPolygon, Coin::Usdc),
        (TokenId::PyusdPolygon, Coin::Pyusd),
        (TokenId::XautPolygon, Coin::Xaut),
    ] {
        assert_eq!(token.chain(), ChainId::Polygon, "{}", token.as_str());
        assert_eq!(token.coin(), coin, "{}", token.as_str());
        assert_eq!(token.decimals(), 6, "{}", token.as_str());
    }
}

#[test]
fn rollup_tokens_read_as_their_coins() {
    for (token, chain, coin, decimals) in [
        (TokenId::UsdtArbitrum, ChainId::Arbitrum, Coin::Usdt, 6),
        (TokenId::UsdcArbitrum, ChainId::Arbitrum, Coin::Usdc, 6),
        (TokenId::PyusdArbitrum, ChainId::Arbitrum, Coin::Pyusd, 6),
        (TokenId::XautArbitrum, ChainId::Arbitrum, Coin::Xaut, 6),
        (TokenId::UsdgArbitrum, ChainId::Arbitrum, Coin::Usdg, 6),
        (TokenId::UsdsArbitrum, ChainId::Arbitrum, Coin::Usds, 18),
        (TokenId::UsdcBase, ChainId::Base, Coin::Usdc, 6),
        (TokenId::UsdsBase, ChainId::Base, Coin::Usds, 18),
        (TokenId::OpOptimism, ChainId::Optimism, Coin::Op, 18),
        (TokenId::UsdtOptimism, ChainId::Optimism, Coin::Usdt, 6),
        (TokenId::UsdcOptimism, ChainId::Optimism, Coin::Usdc, 6),
        (TokenId::UsdsOptimism, ChainId::Optimism, Coin::Usds, 18),
    ] {
        assert_eq!(token.chain(), chain, "{}", token.as_str());
        assert_eq!(token.coin(), coin, "{}", token.as_str());
        assert_eq!(token.decimals(), decimals, "{}", token.as_str());
    }
}

#[test]
fn c_chain_tokens_read_as_their_coins() {
    for (token, coin, decimals) in [
        (TokenId::UsdtAvalanche, Coin::Usdt, 6),
        (TokenId::UsdcAvalanche, Coin::Usdc, 6),
        (TokenId::XautAvalanche, Coin::Xaut, 6),
        (TokenId::UsdsAvalanche, Coin::Usds, 18),
    ] {
        assert_eq!(token.chain(), ChainId::Avalanche, "{}", token.as_str());
        assert_eq!(token.coin(), coin, "{}", token.as_str());
        assert_eq!(token.decimals(), decimals, "{}", token.as_str());
    }
}
