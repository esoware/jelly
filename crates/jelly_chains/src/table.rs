use std::sync::LazyLock;

use jelly_core::{ChainEntry, ChainId, ChainTable};

static CHAINS: LazyLock<ChainTable> = LazyLock::new(|| ChainTable::new(&ChainId::ALL.map(entry)));

pub fn chains() -> &'static ChainTable {
    &CHAINS
}

fn entry(chain: ChainId) -> &'static dyn ChainEntry {
    match chain {
        ChainId::Bitcoin => jelly_bitcoin::ENTRY,
        ChainId::Litecoin => jelly_litecoin::ENTRY,
        ChainId::Dogecoin => jelly_dogecoin::ENTRY,
        ChainId::BitcoinCash => jelly_bitcoin_cash::ENTRY,
        ChainId::Zcash => jelly_zcash::ENTRY,
        ChainId::Ethereum => jelly_ethereum::ENTRY,
        ChainId::Arbitrum => jelly_arbitrum::ENTRY,
        ChainId::Base => jelly_base::ENTRY,
        ChainId::Optimism => jelly_optimism::ENTRY,
        ChainId::Bsc => jelly_bsc::ENTRY,
        ChainId::Polygon => jelly_polygon::ENTRY,
        ChainId::Avalanche => jelly_avalanche::ENTRY,
        ChainId::Solana => jelly_solana::ENTRY,
        ChainId::Tron => jelly_tron::ENTRY,
        ChainId::Xrp => jelly_xrp::ENTRY,
        ChainId::Ton => jelly_ton::ENTRY,
        ChainId::Monero => jelly_monero::ENTRY,
    }
}
