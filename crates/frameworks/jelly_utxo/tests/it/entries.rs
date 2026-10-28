//! The chain table names a key's chain by asking every entry to recognize it, so each
//! entry must recognize exactly the keys of its own version byte.

use jelly_core::{ChainEntry, ChainId};

/// BIP-84 publishes this Bitcoin WIF for `m/84'/0'/0'/0/0`.
const BITCOIN_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";
/// The Litecoin WIF of `m/44'/2'/0'/0/0`.
const LITECOIN_WIF: &str = "T5b4RiWRs7XG8xZ2bCHBoJcn4JrpMTbGRFYXgoZHd7nD8izwqhMK";
/// The Dogecoin WIF of `m/44'/3'/0'/0/0`.
const DOGECOIN_WIF: &str = "QPkeC1ZfHx3c9g7WTj9cQ8gnvk2iSAfAcbq1aVAWjNTwDAKfZUzx";

const ENTRIES: [&dyn ChainEntry; 4] =
    [jelly_bitcoin::ENTRY, jelly_litecoin::ENTRY, jelly_bitcoin_cash::ENTRY, jelly_dogecoin::ENTRY];

#[test]
fn a_wif_imports_into_every_chain_its_version_byte_names() {
    let prefixed = format!("p2wpkh:{BITCOIN_WIF}");
    for (input, chains) in [
        (BITCOIN_WIF, [ChainId::Bitcoin, ChainId::BitcoinCash].as_slice()),
        (prefixed.as_str(), &[ChainId::Bitcoin, ChainId::BitcoinCash]),
        (LITECOIN_WIF, &[ChainId::Litecoin]),
        (DOGECOIN_WIF, &[ChainId::Dogecoin]),
    ] {
        let importers: Vec<ChainId> = ENTRIES
            .iter()
            .filter(|entry| entry.import_key(input, None).is_ok())
            .map(|entry| entry.spec().chain)
            .collect();
        assert_eq!(importers, chains, "{input}");
    }
}

/// A chain that refuses a key leaves it unnamed, so the table can name the chain it
/// belongs to.
#[test]
fn a_key_another_chain_owns_is_named_by_that_chain() {
    for (input, owner, name) in [
        (BITCOIN_WIF, ChainId::Bitcoin, "Bitcoin WIF key"),
        (LITECOIN_WIF, ChainId::Litecoin, "Litecoin WIF key"),
        (DOGECOIN_WIF, ChainId::Dogecoin, "Dogecoin WIF key"),
    ] {
        for entry in ENTRIES {
            let chain = entry.spec().chain;
            let named = entry.recognize_key(input);
            if entry.import_key(input, None).is_ok() {
                assert_eq!(named, Some(name), "{chain} imports {owner}'s key and names it");
            } else {
                assert_eq!(named, None, "{chain} refuses {owner}'s key and leaves it unnamed");
            }
        }
    }
}
