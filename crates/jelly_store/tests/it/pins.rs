use jelly_core::{ChainId, SpkiPin};
use jelly_store::PinRow;

use crate::common;

#[tokio::test]
async fn a_host_keeps_one_pin_and_a_recorded_key_replaces_it() {
    let harness = common::harness().await.unwrap();
    harness
        .store
        .put_pin(PinRow {
            chain: ChainId::Bitcoin,
            host: String::from("Electrum.Example.COM"),
            pin: SpkiPin::new([1u8; 32], 1_700_000_000),
        })
        .await
        .unwrap();
    // Host names are case insensitive, so the second write is the same row. The verifier
    // has already approved the changed key, so the store records it.
    harness
        .store
        .put_pin(PinRow {
            chain: ChainId::Bitcoin,
            host: String::from("electrum.example.com"),
            pin: SpkiPin::new([2u8; 32], 1_700_000_900),
        })
        .await
        .unwrap();

    let rows = harness.store.list_pins().await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = rows.first().expect("one row");
    assert_eq!(row.host, "electrum.example.com");
    assert_eq!(row.pin, SpkiPin::new([2u8; 32], 1_700_000_900));
    assert_eq!(row.chain, ChainId::Bitcoin);
}

#[tokio::test]
async fn pins_on_every_chain_are_listed_together_in_chain_order() {
    let harness = common::harness().await.unwrap();
    harness
        .store
        .put_pin(PinRow {
            chain: ChainId::Litecoin,
            host: String::from("two.example.com"),
            pin: SpkiPin::new([4u8; 32], 2),
        })
        .await
        .unwrap();
    harness
        .store
        .put_pin(PinRow {
            chain: ChainId::Bitcoin,
            host: String::from("one.example.com"),
            pin: SpkiPin::new([3u8; 32], 1),
        })
        .await
        .unwrap();

    let chains: Vec<ChainId> =
        harness.store.list_pins().await.unwrap().into_iter().map(|row| row.chain).collect();
    assert_eq!(chains, vec![ChainId::Bitcoin, ChainId::Litecoin]);
}
