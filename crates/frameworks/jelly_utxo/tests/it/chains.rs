//! The tables, checked against each chain's published constants and against each
//! other where one figure is defined from another.

use bitcoin::bech32::Hrp;
use bitcoin::bech32::segwit::encode_v1;
use bitcoin::block::{Header, Version};
use bitcoin::consensus::deserialize;
use bitcoin::constants::genesis_block;
use bitcoin::hashes::Hash as _;
use bitcoin::{BlockHash, CompactTarget, Network, Sequence, TxMerkleNode, Weight};
use jelly_core::{Amount, ChainId, CustomFee, FeeRate, ScriptKind, TxWarningKind};
use jelly_utxo::{BumpMethod, DustRule, FeeMarket, Replacement, UtxoError, UtxoParams};

use crate::common::{
    BITCOIN, BITCOIN_CASH, DOGECOIN, Fallible, LITECOIN, LITECOIN_BLOCK_ZERO_HEADER, TABLES,
    auction, checkpoint_header, genesis_header, header_vector, kwu, spec,
};

const fn relay_dust(params: &UtxoParams) -> Option<FeeRate> {
    match params.dust {
        DustRule::RelayFee(rate) => Some(rate),
        DustRule::Fixed(_) => None,
    }
}

#[test]
fn each_table_carries_its_chains_published_constants() {
    for (params, chain) in [
        (BITCOIN, ChainId::Bitcoin),
        (LITECOIN, ChainId::Litecoin),
        (BITCOIN_CASH, ChainId::BitcoinCash),
        (DOGECOIN, ChainId::Dogecoin),
    ] {
        assert_eq!(params.chain, chain);
    }
    // DUST_RELAY_TX_FEE is 3000 per kvB on Bitcoin and 30000 on Litecoin,
    // quartered for weight units. Bitcoin Cash Node triples its 1000 per kB, which
    // lands on Bitcoin's figure.
    assert_eq!(relay_dust(&BITCOIN), Some(kwu(750)));
    assert_eq!(relay_dust(&BITCOIN_CASH), relay_dust(&BITCOIN));
    assert_eq!(
        relay_dust(&LITECOIN).map(FeeRate::sat_per_kwu),
        relay_dust(&BITCOIN).map(|rate| rate.sat_per_kwu() * 10),
        "the single most likely figure to get wrong when porting Bitcoin code across"
    );
    // Dogecoin Core's soft dust limit is a value, not a rate.
    assert_eq!(DOGECOIN.dust, DustRule::Fixed(Amount::from_base_units(1_000_000)));

    // DEFAULT_MIN_RELAY_TX_FEE and DEFAULT_INCREMENTAL_RELAY_FEE are 100 per kvB on
    // Bitcoin since Core 29.1, and still 1000 on Litecoin and Bitcoin Cash.
    assert_eq!(BITCOIN.min_relay_fee, kwu(25));
    assert_eq!(BITCOIN.incremental_relay_fee, kwu(25));
    for params in [LITECOIN, BITCOIN_CASH] {
        assert_eq!(params.min_relay_fee, kwu(250), "{}", params.chain);
        assert_eq!(params.incremental_relay_fee, kwu(250), "{}", params.chain);
    }
    // Dogecoin relays at a tenth of its 0.01 DOGE/kB recommendation and replaces in
    // steps of a tenth of that.
    assert_eq!(DOGECOIN.min_relay_fee, kwu(25_000));
    assert_eq!(DOGECOIN.incremental_relay_fee, kwu(2_500));

    assert_eq!(BITCOIN.max_money, Amount::from_base_units(21_000_000 * 100_000_000));
    assert_eq!(BITCOIN_CASH.max_money, BITCOIN.max_money, "a split keeps the supply cap");
    assert_eq!(
        LITECOIN.max_money,
        Amount::from_base_units(84_000_000 * 100_000_000),
        "four times Bitcoin's cap"
    );
    assert_eq!(
        DOGECOIN.max_money,
        Amount::from_base_units(10_000_000_000 * 100_000_000),
        "ten billion DOGE bounds one transaction, not the supply"
    );

    for params in TABLES {
        assert_eq!(
            params.max_standard_weight,
            Weight::from_wu(400_000),
            "{}: one hundred thousand bytes on the chains without segwit",
            params.chain
        );
        assert_eq!(params.confirmation_watch_depth, 6, "{}", params.chain);
    }
    for params in [BITCOIN, LITECOIN, BITCOIN_CASH] {
        assert_eq!(params.fee_ceiling, Amount::from_base_units(2_000_000), "{}", params.chain);
        assert_eq!(params.coinbase_maturity, 100, "{}", params.chain);
    }
    assert_eq!(DOGECOIN.fee_ceiling, Amount::from_base_units(100 * 100_000_000));
    assert_eq!(DOGECOIN.coinbase_maturity, 240);
}

/// Each table pays what its own reference wallet would.
#[test]
fn each_table_carries_its_reference_wallets_fee_policy() {
    assert_eq!(BITCOIN.min_tx_fee, kwu(250));
    assert_eq!(LITECOIN.min_tx_fee, kwu(2_500));
    assert_eq!(BITCOIN_CASH.min_tx_fee, kwu(250), "one sat per byte");
    assert_eq!(DOGECOIN.min_tx_fee, kwu(250_000), "a thousand koinu per byte");
    assert_eq!(auction(&BITCOIN).unwrap().target_blocks, [6, 3, 1]);
    assert_eq!(auction(&LITECOIN).unwrap().target_blocks, [24, 6, 2]);
    for params in [BITCOIN, LITECOIN] {
        let auction = auction(&params).unwrap();
        assert_eq!(auction.block_vsize, 1_000_000, "the four million weight unit block");
        assert!(auction.api_route.starts_with("/v1/fees/"), "{}", params.chain);
        assert!(
            auction.api_fallback_route.is_none_or(|route| route.starts_with("/v1/fees/")),
            "{}",
            params.chain
        );
    }
    for params in [BITCOIN_CASH, DOGECOIN] {
        assert_eq!(params.fee_market, FeeMarket::Flat, "{}", params.chain);
    }
    for params in TABLES {
        assert!(params.min_tx_fee >= params.min_relay_fee, "{}", params.chain);
    }
}

/// Each table signals and bumps as its nodes relay.
///
/// Bitcoin and Dogecoin nodes relay a replacement by default, so a send is bumped by
/// one. Litecoin's relay one only when configured to, so a child pays for the send
/// instead, and Bitcoin Cash offers neither.
#[test]
fn each_table_signals_and_offers_replacement_as_its_nodes_honour_it() {
    for (params, replacement, bump_method) in [
        (BITCOIN, Replacement::ByDefault, Some(BumpMethod::Replace)),
        (LITECOIN, Replacement::WhenConfigured, Some(BumpMethod::ChildPaysForParent)),
        (BITCOIN_CASH, Replacement::Never, None),
        (DOGECOIN, Replacement::ByDefault, Some(BumpMethod::Replace)),
    ] {
        assert_eq!(params.replacement, replacement, "{}", params.chain);
        assert_eq!(params.bump_method, bump_method, "{}", params.chain);
        assert_eq!(
            params.bump_method.is_some(),
            spec(&params).unwrap().can_bump_fee,
            "{}",
            params.chain
        );
        let sequence = params.replacement.input_sequence();
        assert!(
            sequence.enables_absolute_lock_time(),
            "{}: the anti-fee-sniping locktime needs a non-final sequence",
            params.chain
        );
        assert_eq!(sequence.is_rbf(), replacement != Replacement::Never, "{}", params.chain);
    }
    assert_eq!(Replacement::Never.input_sequence(), Sequence::ENABLE_LOCKTIME_NO_RBF);
}

/// A chain with segregated witness derives a segwit key, counts virtual bytes, and
/// prices its fee per virtual byte, and a chain without it does none of those.
#[test]
fn each_table_has_segwit_exactly_when_its_metadata_says_so() {
    for params in TABLES {
        let metadata = spec(&params).unwrap();
        assert_eq!(
            metadata.script_kinds.contains(&ScriptKind::P2wpkh),
            params.has_segwit,
            "{}",
            params.chain
        );
        assert_eq!(
            metadata.fee_rate_unit.is_some_and(|unit| unit.ends_with("/vB")),
            params.has_segwit,
            "{}",
            params.chain
        );
        assert_eq!(params.script_kinds, metadata.script_kinds, "{}", params.chain);
        assert_eq!(Some(params.fee_rate_unit), metadata.fee_rate_unit, "{}", params.chain);
        assert_eq!(params.codec.chain(), params.chain);
    }
}

/// The interface offers the levels the spec lists, so they have to be the ones the
/// table's market prices.
#[test]
fn each_table_offers_the_fee_levels_its_market_prices() {
    for params in TABLES {
        let metadata = spec(&params).unwrap();
        assert_eq!(params.fee_levels(), metadata.fee_levels, "{}", params.chain);
        assert_eq!(metadata.custom_fee, Some(CustomFee::Rate), "{}", params.chain);
    }
}

/// A high-fee warning above the fee a build refuses could never show.
///
/// A flat market pays its reference rate at every level, so a level that warned would warn
/// on every send of that size. Even the largest standard transaction stays at or under the
/// warning at that rate, which leaves it to a custom rate.
#[test]
fn each_tables_high_fee_warning_sits_under_the_fee_it_refuses() {
    for params in TABLES {
        let high_fee = spec(&params).unwrap().high_fee;
        assert!(high_fee < params.fee_ceiling, "{}", params.chain);
        if params.fee_market == FeeMarket::Flat {
            assert!(
                u128::from(params.min_tx_fee.sat_per_kwu())
                    * u128::from(params.max_standard_weight.to_wu())
                    <= high_fee.base_units() * 1000,
                "{}",
                params.chain
            );
        }
    }
}

#[test]
fn only_bitcoin_cash_signs_with_a_fork_id_and_carries_tokens() {
    for params in TABLES {
        let is_bitcoin_cash = params.chain == ChainId::BitcoinCash;
        assert_eq!(params.sighash_fork_id.is_some(), is_bitcoin_cash, "{}", params.chain);
        assert_eq!(params.has_cash_tokens, is_bitcoin_cash, "{}", params.chain);
        assert_eq!(params.has_slp_tokens, is_bitcoin_cash, "{}", params.chain);
    }
    assert_eq!(BITCOIN_CASH.sighash_fork_id, Some(0));
}

#[test]
fn each_table_names_its_own_unit() {
    assert_eq!(BITCOIN.format_rate(kwu(375)), "1.5 sat/vB");
    assert_eq!(LITECOIN.format_rate(kwu(375)), "1.5 lit/vB");
    assert_eq!(BITCOIN_CASH.format_rate(kwu(375)), "1.5 sat/B");
    assert_eq!(DOGECOIN.format_rate(DOGECOIN.min_tx_fee), "1000 koinu/B");
}

#[test]
fn each_chain_warns_about_its_own_ambiguous_address_forms() {
    assert!(
        BITCOIN.address_notes.is_empty(),
        "every Bitcoin address form is unambiguous, so there is nothing to warn about"
    );
    assert!(DOGECOIN.address_notes.is_empty(), "Dogecoin's version bytes are its own");
    assert_eq!(LITECOIN.address_notes.len(), 1);
    let note = LITECOIN.address_notes.first().expect("the ambiguity note");
    assert_eq!(note.prefix, "3");
    assert_eq!(note.kind, TxWarningKind::AmbiguousAddress);
    assert!(note.message.contains("Bitcoin"), "{}", note.message);

    let prefixes: Vec<&str> = BITCOIN_CASH.address_notes.iter().map(|note| note.prefix).collect();
    assert_eq!(prefixes, ["1", "3"], "both legacy forms are Bitcoin's too");
    for note in BITCOIN_CASH.address_notes {
        assert_eq!(note.kind, TxWarningKind::AmbiguousAddress);
        assert!(note.message.contains("Bitcoin Cash"), "{}", note.message);
    }
}

#[test]
fn only_litecoin_refuses_an_address_form_by_name() {
    for params in [BITCOIN, BITCOIN_CASH, DOGECOIN] {
        assert!(params.address_refusals.is_empty(), "{}", params.chain);
    }
    assert_eq!(LITECOIN.address_refusals.len(), 1);
    let refusal = LITECOIN.address_refusals.first().expect("the MWEB refusal");
    assert_eq!(refusal.prefix, "ltcmweb");
    assert_eq!(refusal.prefix, refusal.prefix.to_ascii_lowercase());
    assert!(refusal.message.contains("MWEB"), "{}", refusal.message);
}

/// Bitcoin prices a change output at Core's consolidation rate, Litecoin and
/// Bitcoin Cash at their relay floor, and Dogecoin at the rate every spend pays.
#[test]
fn each_chain_prices_change_its_own_way() {
    assert_eq!(BITCOIN.long_term_fee_rate, kwu(2_500));
    assert_eq!(LITECOIN.long_term_fee_rate, kwu(250));
    assert_eq!(BITCOIN_CASH.long_term_fee_rate, BITCOIN_CASH.min_relay_fee);
    assert_eq!(DOGECOIN.long_term_fee_rate, DOGECOIN.min_tx_fee);
}

#[test]
fn the_fixture_block_zero_is_litecoins_own() {
    let bytes = hex::decode(LITECOIN_BLOCK_ZERO_HEADER).unwrap();
    assert_eq!(bytes.len(), 80);
    let header: Header = deserialize(&bytes).unwrap();
    assert_eq!(header.block_hash(), LITECOIN.electrum_chain().genesis);
    assert_eq!(header.time, 1_317_972_665, "2011-10-07");
}

/// The headers the fake servers answer with are the chains' own, so the guard
/// over them is the one production runs.
#[test]
fn every_recorded_header_hashes_to_its_tables_block() {
    for params in TABLES {
        let electrum = params.electrum_chain();
        let genesis: Header = deserialize(&hex::decode(genesis_header(&params)).unwrap()).unwrap();
        assert_eq!(genesis.block_hash(), electrum.genesis, "{}", params.chain);
        match (checkpoint_header(&params), electrum.checkpoint) {
            (Some(hex_text), Some(checkpoint)) => {
                let header: Header = deserialize(&hex::decode(hex_text).unwrap()).unwrap();
                assert_eq!(header.block_hash(), checkpoint.hash, "{}", params.chain);
            }
            (None, None) => {}
            (recorded, named) => panic!("{}: {recorded:?} {named:?}", params.chain),
        }
    }
    let dogecoin: Header =
        deserialize(&hex::decode(header_vector("dogecoin0").unwrap()).unwrap()).unwrap();
    assert_eq!(dogecoin.time, 1_386_325_540, "2013-12-06");
}

/// Block zero as its chain's own source states it, so a table's constant is
/// checked against the header's published fields rather than against another
/// copy of itself.
fn published_header(merkle_root: &str, time: u32, nonce: u32) -> Fallible<Header> {
    let mut merkle = <[u8; 32]>::try_from(hex::decode(merkle_root)?.as_slice())?;
    merkle.reverse();
    Ok(Header {
        version: Version::ONE,
        prev_blockhash: BlockHash::all_zeros(),
        merkle_root: TxMerkleNode::from_byte_array(merkle),
        time,
        bits: CompactTarget::from_consensus(0x1e0f_fff0),
        nonce,
    })
}

#[test]
fn the_bitcoin_genesis_constant_is_the_hash_of_block_zero() {
    let block = genesis_block(Network::Bitcoin);
    let genesis = BITCOIN.electrum_chain().genesis;
    assert_eq!(genesis, block.block_hash());
    assert_eq!(
        genesis.to_string(),
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
    );
}

#[test]
fn the_litecoin_genesis_constant_is_the_hash_of_the_published_block_zero() {
    let header = published_header(
        "97ddfbbae6be97fd6cdf3e7ca13232a3afff2353e29badfab7f73011edd4ced9",
        1_317_972_665,
        2_084_524_493,
    )
    .unwrap();
    let genesis = LITECOIN.electrum_chain().genesis;
    assert_eq!(genesis, header.block_hash());
    assert_eq!(
        genesis.to_string(),
        "12a765e31ffd4059bada1e25190f6e98c99d9714d334efa41a195a7e7e04bfe2"
    );
    assert_ne!(
        genesis,
        BITCOIN.electrum_chain().genesis,
        "the two server populations share ports, so this is what tells them apart"
    );
}

/// Feathercoin and Litecoin Cash kept Litecoin's block zero, so Litecoin proves a
/// block mined after both parted from it.
#[test]
fn litecoin_proves_a_block_past_the_chains_that_share_its_block_zero() {
    let checkpoint = LITECOIN.electrum_chain().checkpoint.expect("a checkpoint");
    assert_eq!(checkpoint.height, 2_520_000);
    assert_eq!(
        checkpoint.hash.to_string(),
        "54cd0ef48a977c5bb9c845e0c67a4e9d17a5894b084aea4bf7e594122f5c0dc1"
    );
}

/// Dogecoin's block zero hashes with SHA-256d like every other header, though its
/// proof of work is scrypt.
#[test]
fn the_dogecoin_genesis_constant_is_the_hash_of_the_published_block_zero() {
    let header = published_header(
        "5b2a3f53f605d62c53e62932dac6925e3d74afa5a4b459745c36d42d0ed26a69",
        1_386_325_540,
        99_943,
    )
    .unwrap();
    let genesis = DOGECOIN.electrum_chain().genesis;
    assert_eq!(genesis, header.block_hash());
    assert_eq!(
        genesis.to_string(),
        "1a91e3dace36e2be3bf030a65679fe821aa1d6ef92e7c9902eb318182c355691"
    );
}

/// Block zero cannot tell Bitcoin's servers from Bitcoin Cash's, so each proves
/// its own block 840000, which differ.
#[test]
fn bitcoin_and_bitcoin_cash_share_block_zero_and_part_at_the_checkpoint() {
    assert_eq!(BITCOIN_CASH.electrum_chain().genesis, BITCOIN.electrum_chain().genesis);
    let bitcoin = BITCOIN.electrum_chain().checkpoint.expect("a checkpoint");
    let bitcoin_cash = BITCOIN_CASH.electrum_chain().checkpoint.expect("a checkpoint");
    assert_eq!(bitcoin.height, bitcoin_cash.height);
    assert_ne!(bitcoin.hash, bitcoin_cash.hash);
    assert_eq!(
        bitcoin.hash.to_string(),
        "0000000000000000000320283a032748cef8227873ff4872689bf23f1cda83a5"
    );
    assert_eq!(
        bitcoin_cash.hash.to_string(),
        "000000000000000000b3cfd73dbd87c5e6cae26d89a5956ee78193733f61340e"
    );
}

#[test]
fn every_table_proves_a_distinct_chain() {
    for (index, params) in TABLES.iter().enumerate() {
        for other in TABLES.iter().skip(index.saturating_add(1)) {
            assert_ne!(
                params.electrum_chain(),
                other.electrum_chain(),
                "{} {}",
                params.chain,
                other.chain
            );
        }
    }
    assert!(DOGECOIN.electrum_chain().is_merge_mined);
}

/// The superseded `3...` form pays the same script as the current `M...` one, and
/// is never what a script encodes back to.
#[test]
fn the_superseded_litecoin_script_hash_form_is_read_but_never_written() {
    // The same script hash as `M7wtsL7wSHDBJVMWWhtQfTMSYYkyooAAXM` under version
    // byte 0x05.
    let legacy = "31jkZShyVAMkVz5cQpu4qp73DrAXtFP3rZ";
    let script_pubkey = LITECOIN.script_pubkey(legacy).expect("a standard script");
    let written = LITECOIN.address_of(script_pubkey.as_script()).expect("a script hash address");
    assert!(written.starts_with('M'), "{written}");
    assert_eq!(LITECOIN.script_pubkey(&written).expect("a standard script"), script_pubkey);
    let note = LITECOIN.address_notes.first().expect("the ambiguity note");
    assert!(legacy.starts_with(note.prefix));
}

/// A legacy address pays the script its `CashAddr` form pays, and the script is
/// written back as `CashAddr`, so the preview names the form the chain prefers.
#[test]
fn a_bitcoin_cash_legacy_address_is_read_and_written_back_as_cashaddr() {
    let legacy = "1BpEi6DfDAUFd7GtittLSdBeYJvcoaVggu";
    let script_pubkey = BITCOIN_CASH.script_pubkey(legacy).expect("a standard script");
    assert_eq!(
        BITCOIN_CASH.address_of(script_pubkey.as_script()).as_deref(),
        Some("bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a")
    );
    assert_eq!(BITCOIN_CASH.canonical(legacy).expect("an address"), legacy);
    assert!(BITCOIN_CASH.address_notes.iter().any(|note| legacy.starts_with(note.prefix)));
}

#[test]
fn an_mweb_address_is_named_before_any_checksum_is_read() {
    let mweb = "ltcmweb1qqfyxdrgtqzrsfsjtlnywsn6vswdla2wsuqw4vfhkq0nug5xy0enq8q";
    assert!(matches!(LITECOIN.canonical(mweb), Err(UtxoError::RefusedAddress { .. })));
    assert!(
        matches!(
            LITECOIN.canonical(&format!("  {}", mweb.to_uppercase())),
            Err(UtxoError::RefusedAddress { .. })
        ),
        "bech32 permits an all-uppercase spelling"
    );
    assert!(
        matches!(BITCOIN.canonical(mweb), Err(UtxoError::Address(_))),
        "Bitcoin refuses no form by name, so its codec reads it"
    );
}

/// Litecoin activated taproot in 2022 and relays it, so a `ltc1p` recipient is
/// paid like any other.
///
/// The address is built by an independent bech32m encoder rather than remembered,
/// so this is a fact about the encoding.
#[test]
fn a_litecoin_taproot_recipient_passes_the_gate_and_has_an_output_script() {
    let address = encode_v1(Hrp::parse_unchecked("ltc"), &[0x02u8; 32])
        .expect("a thirty-two byte witness program encodes");
    assert!(address.starts_with("ltc1p"), "{address}");
    assert_eq!(LITECOIN.canonical(&address).expect("a Litecoin address"), address);
    assert_eq!(
        hex::encode(LITECOIN.script_pubkey(&address).expect("an output script").as_bytes()),
        format!("5120{}", hex::encode([0x02u8; 32]))
    );
}

#[test]
fn each_electrum_chain_keeps_its_cursors_under_its_own_stream() {
    for params in TABLES {
        assert_eq!(
            params.scan_stream.as_str(),
            format!("{}.electrum_status", params.chain.as_str()),
            "the chains store the same cursor bytes under different names"
        );
    }
}
