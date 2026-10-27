use jelly_chains::chains;
use jelly_core::{ChainId, CustomFee, FeeLevel, ScriptKind};
use jelly_keypair::{PathTemplate, stored_path};

/// The table lists every chain in declaration order, so a new chain fails here until it
/// has a row.
#[test]
fn every_chain_derives_under_its_expected_coin_type() {
    let expected = [
        (ChainId::Bitcoin, 0),
        (ChainId::Litecoin, 2),
        (ChainId::Dogecoin, 3),
        (ChainId::BitcoinCash, 145),
        (ChainId::Zcash, 133),
        (ChainId::Ethereum, 60),
        (ChainId::Bsc, 60),
        (ChainId::Polygon, 60),
        (ChainId::Arbitrum, 60),
        (ChainId::Base, 60),
        (ChainId::Avalanche, 60),
        (ChainId::Solana, 501),
        (ChainId::Tron, 195),
        (ChainId::Xrp, 144),
        (ChainId::Ton, 607),
        (ChainId::Monero, 128),
    ];
    assert_eq!(expected.map(|(chain, _)| chain), ChainId::ALL);
    for (chain, slip44) in expected {
        assert_eq!(chains().spec(chain).slip44, slip44, "{}", chain.as_str());
    }
}

/// A seed has one address on Ethereum and BNB Smart Chain.
#[test]
fn bsc_shares_ethereums_coin_type_and_trades_as_bnb() {
    assert_eq!(chains().spec(ChainId::Bsc).slip44, chains().spec(ChainId::Ethereum).slip44);
    assert_eq!(ChainId::Bsc.ticker(), "BNB");
    assert_eq!(ChainId::Bsc.display_name(), "BNB Smart Chain");
    assert_eq!(ChainId::Bsc.coin_name(), "BNB");
    assert!(!ChainId::ALL.iter().any(|chain| chain.ticker() == "BSC"));
}

/// MATIC became POL in September 2024, and the network keeps the Polygon name.
#[test]
fn polygon_shares_ethereums_coin_type_and_trades_as_pol() {
    assert_eq!(chains().spec(ChainId::Polygon).slip44, chains().spec(ChainId::Ethereum).slip44);
    assert_eq!(ChainId::Polygon.ticker(), "POL");
    assert_eq!(ChainId::Polygon.display_name(), "Polygon");
    assert!(!ChainId::ALL.iter().any(|chain| chain.ticker() == "MATIC"));
}

/// Both rollups pay gas in ether, so a seed has Ethereum's address on them and their
/// coin trades as ETH.
#[test]
fn the_rollups_share_ethereums_coin_type_and_trade_as_eth() {
    for chain in [ChainId::Arbitrum, ChainId::Base] {
        assert_eq!(chains().spec(chain).slip44, chains().spec(ChainId::Ethereum).slip44);
        assert_eq!(
            chains().spec(chain).default_path,
            chains().spec(ChainId::Ethereum).default_path
        );
        assert_eq!(chain.ticker(), "ETH", "{}", chain.as_str());
    }
    assert_eq!(ChainId::Arbitrum.display_name(), "Arbitrum");
    assert_eq!(ChainId::Base.display_name(), "Base");
}

/// The X-Chain and P-Chain derive at Avalanche's own coin type 9000, but every
/// wallet derives the C-Chain at Ethereum's, so a seed has one address on both.
#[test]
fn the_c_chain_shares_ethereums_coin_type_and_trades_as_avax() {
    let spec = chains().spec(ChainId::Avalanche);
    assert_eq!(spec.slip44, chains().spec(ChainId::Ethereum).slip44);
    assert_eq!(spec.default_path, chains().spec(ChainId::Ethereum).default_path);
    assert_eq!(ChainId::Avalanche.ticker(), "AVAX");
    assert_eq!(ChainId::Avalanche.display_name(), "Avalanche C-Chain");
}

#[test]
fn every_chain_spec_is_complete() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        assert!(!chain.display_name().is_empty(), "{}", chain.as_str());
        assert!(spec.default_path.contains("{index}"), "{}", spec.default_path);
        assert!(
            spec.explorer_tx_url.is_some_and(|template| template.contains("{txid}")),
            "{:?}",
            spec.explorer_tx_url
        );
        assert!(
            spec.explorer_address_url.is_none_or(|template| template.contains("{address}")),
            "{:?}",
            spec.explorer_address_url
        );
        assert!(chain.native_decimals() <= 38);
        assert!(spec.settled_confirmations > 0, "{}", chain.as_str());
    }
}

/// A UTXO chain's flavours are output scripts, and TON's are the wallet contract
/// versions one key can hold Gram in.
#[test]
fn only_the_utxo_chains_and_ton_have_script_flavours() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        let has_flavours = chain.is_utxo() || chain == ChainId::Ton;
        assert_eq!(!spec.script_kinds.is_empty(), has_flavours, "{}", chain.as_str());
        assert_eq!(spec.default_script.is_some(), has_flavours, "{}", chain.as_str());
        if let Some(default) = spec.default_script {
            assert!(spec.script_kinds.contains(&default));
        }
    }
}

#[test]
fn script_flavours_and_defaults_match_what_each_chain_supports() {
    for chain in [ChainId::Dogecoin, ChainId::BitcoinCash] {
        let spec = chains().spec(chain);
        assert_eq!(spec.script_kinds, [ScriptKind::P2pkh], "{}", chain.as_str());
        assert_eq!(spec.default_script, Some(ScriptKind::P2pkh), "{}", chain.as_str());
    }
    assert_eq!(chains().spec(ChainId::Bitcoin).default_script, Some(ScriptKind::P2wpkh));
    let litecoin = chains().spec(ChainId::Litecoin);
    assert_eq!(litecoin.default_script, Some(ScriptKind::P2wpkh));
    assert!(
        !litecoin.script_kinds.contains(&ScriptKind::P2tr),
        "Litecoin support is segwit v0 only"
    );
    let ton = chains().spec(ChainId::Ton);
    assert_eq!(ton.script_kinds, [ScriptKind::TonV5r1, ScriptKind::TonV4r2, ScriptKind::TonV3r2]);
    assert_eq!(ton.default_script, Some(ScriptKind::TonV5r1));
}

/// Zcash prices logical actions rather than bytes. A chain whose custom fee is a rate
/// names the unit it is entered in.
#[test]
fn byte_priced_chains_name_a_fee_rate_unit() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        let is_byte_priced =
            (chain.is_utxo() && chain != ChainId::Zcash) || chain == ChainId::Monero;
        assert_eq!(spec.fee_rate_unit.is_some(), is_byte_priced, "{}", chain.as_str());
        assert_eq!(spec.custom_fee == Some(CustomFee::Rate), is_byte_priced, "{}", chain.as_str());
    }
    assert_eq!(chains().spec(ChainId::Bitcoin).fee_rate_unit, Some("sat/vB"));
    assert_eq!(chains().spec(ChainId::Litecoin).fee_rate_unit, Some("lit/vB"));
    assert_eq!(chains().spec(ChainId::Dogecoin).fee_rate_unit, Some("koinu/B"));
    assert_eq!(chains().spec(ChainId::BitcoinCash).fee_rate_unit, Some("sat/B"));
    assert_eq!(chains().spec(ChainId::Monero).fee_rate_unit, Some("piconero/B"));
}

/// Only the chains with a way to speed up or cancel a send offer one.
///
/// Bitcoin and Dogecoin replace by BIP-125 and EVM by reusing the nonce, which on EVM
/// also cancels a send by spending its nonce on nothing. Litecoin Core nodes refuse
/// replacements unless `-mempoolreplacement` is set, so a Litecoin send is sped up by
/// a child spending its change.
///
/// Bitcoin Cash nodes have no replacement at all, Solana, TRON, XRP, TON, and Zcash
/// payments expire instead, and Monero has no replacement mechanism. Only an XRP account
/// can be deleted, which `AccountDelete` does, and only an XRP payment carries a
/// destination tag.
#[test]
fn capability_flags_agree_with_the_chains_that_have_them() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        let can_bump = matches!(
            chain,
            ChainId::Bitcoin
                | ChainId::Litecoin
                | ChainId::Dogecoin
                | ChainId::Ethereum
                | ChainId::Bsc
                | ChainId::Polygon
                | ChainId::Arbitrum
                | ChainId::Base
                | ChainId::Avalanche
        );
        assert_eq!(spec.can_bump_fee, can_bump, "{}", chain.as_str());
        let can_cancel = matches!(
            chain,
            ChainId::Ethereum
                | ChainId::Bsc
                | ChainId::Polygon
                | ChainId::Arbitrum
                | ChainId::Base
                | ChainId::Avalanche
        );
        assert_eq!(spec.can_cancel, can_cancel, "{}", chain.as_str());
        let is_xrp = chain == ChainId::Xrp;
        assert_eq!(spec.can_delete_account, is_xrp, "{}", chain.as_str());
        assert_eq!(spec.has_destination_tags, is_xrp, "{}", chain.as_str());
        let is_monero = chain == ChainId::Monero;
        assert_eq!(spec.has_subaddresses, is_monero, "{}", chain.as_str());
        assert_eq!(spec.needs_account_key, is_monero, "{}", chain.as_str());
        let is_evm = spec.custom_fee == Some(CustomFee::FeeCap)
            || spec.custom_fee == Some(CustomFee::GasPrice);
        assert_eq!(spec.can_pin_nonce, is_evm, "{}", chain.as_str());
        assert_eq!(spec.can_carry_data, is_evm, "{}", chain.as_str());
    }
}

/// Every chain whose fee follows a market offers the same three levels, slowest first.
///
/// Dogecoin and Bitcoin Cash, whose reference wallets pay one flat rate, and XRP and
/// Zcash, whose fee is one network figure, offer only the normal one beside a custom
/// fee. TRON and TON charge what their protocol computes, so they offer neither.
#[test]
fn every_chain_offers_the_fee_levels_and_custom_fee_its_market_allows() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        let (levels, custom): (&[FeeLevel], Option<CustomFee>) = match chain {
            ChainId::Bitcoin | ChainId::Litecoin | ChainId::Monero => {
                (&FeeLevel::ALL, Some(CustomFee::Rate))
            }
            ChainId::Dogecoin | ChainId::BitcoinCash => {
                (&[FeeLevel::Normal], Some(CustomFee::Rate))
            }
            ChainId::Ethereum
            | ChainId::Polygon
            | ChainId::Arbitrum
            | ChainId::Base
            | ChainId::Avalanche => (&FeeLevel::ALL, Some(CustomFee::FeeCap)),
            ChainId::Bsc => (&FeeLevel::ALL, Some(CustomFee::GasPrice)),
            ChainId::Solana => (&FeeLevel::ALL, Some(CustomFee::PriorityFee)),
            ChainId::Zcash | ChainId::Xrp => (&[FeeLevel::Normal], Some(CustomFee::Total)),
            ChainId::Tron | ChainId::Ton => (&[], None),
        };
        assert_eq!(spec.fee_levels, levels, "{}", chain.as_str());
        assert_eq!(spec.custom_fee, custom, "{}", chain.as_str());
    }
}

#[test]
fn every_chain_lists_its_primary_transport_first() {
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        assert_eq!(spec.transports.first(), Some(&spec.primary_transport), "{}", chain.as_str());
    }
}

/// Monero's ten-block depth is also its output spendability rule.
#[test]
fn settled_confirmations_match_each_chains_finality() {
    let expected = [
        (ChainId::Bitcoin, 6),
        (ChainId::Litecoin, 6),
        (ChainId::Dogecoin, 6),
        (ChainId::BitcoinCash, 6),
        (ChainId::Zcash, 10),
        (ChainId::Ethereum, 12),
        (ChainId::Bsc, 3),
        (ChainId::Polygon, 5),
        (ChainId::Arbitrum, 1),
        (ChainId::Base, 1),
        (ChainId::Avalanche, 1),
        (ChainId::Solana, 1),
        (ChainId::Tron, 19),
        (ChainId::Xrp, 1),
        (ChainId::Ton, 1),
        (ChainId::Monero, 10),
    ];
    assert_eq!(expected.map(|(chain, _)| chain), ChainId::ALL);
    for (chain, depth) in expected {
        assert_eq!(chains().spec(chain).settled_confirmations, depth, "{}", chain.as_str());
    }
}

/// The frontend shows `default_path` with the index filled in, and the engine derives
/// from the template it parses to, so the two must name the same paths.
#[test]
fn default_paths_match_their_parsed_templates() {
    for chain in ChainId::ALL {
        let default_path = chains().spec(chain).default_path;
        let template = PathTemplate::parse(default_path).unwrap();
        for slot in [0u32, 1, 7] {
            let default = template.path_at(slot).unwrap();
            assert_eq!(
                stored_path(&default).as_str(),
                default_path.replace("{index}", &slot.to_string()),
                "{}",
                chain.as_str()
            );
            assert_eq!(template.index_of(&default), Some(slot));
        }
    }
}
