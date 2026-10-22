use std::sync::LazyLock;

use jelly_core::ChainTable;

/// Every chain's entry, in [`jelly_core::ChainId::ALL`] order, which the constructor
/// checks.
static CHAINS: LazyLock<ChainTable> = LazyLock::new(|| {
    ChainTable::new([
        jelly_bitcoin::ENTRY,
        jelly_litecoin::ENTRY,
        jelly_dogecoin::ENTRY,
        jelly_bitcoin_cash::ENTRY,
        jelly_zcash::ENTRY,
        jelly_ethereum::ENTRY,
        jelly_bsc::ENTRY,
        jelly_polygon::ENTRY,
        jelly_arbitrum::ENTRY,
        jelly_base::ENTRY,
        jelly_avalanche::ENTRY,
        jelly_solana::ENTRY,
        jelly_tron::ENTRY,
        jelly_xrp::ENTRY,
        jelly_ton::ENTRY,
        jelly_monero::ENTRY,
    ])
});

pub fn chains() -> &'static ChainTable {
    &CHAINS
}
