use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    Address, BuildError, ChainAdapter, ChainId, Clock as _, ScriptKind, SystemClock, WalletEvent,
    test_util::{FakeEventSink, MemoryPinStore, build_request},
};
use jelly_net::Network;

use super::common::history_watched;

/// An adapter over the shipped mainnet endpoints, on the system clock.
async fn mainnet_adapter(clock: &Arc<SystemClock>) -> Result<Arc<dyn ChainAdapter>, BuildError> {
    let network = Network::start(
        Arc::<SystemClock>::clone(clock),
        Arc::new(MemoryPinStore::new()),
        None,
        &[jelly_ton::ENTRY],
    )?;
    jelly_ton::build(&network, &build_request(&[ChainId::Ton])).await
}

#[tokio::test]
#[ignore = "Queries public mainnet account history and depends on TON Center availability."]
async fn mainnet_read_smoke() {
    let clock = Arc::new(SystemClock);
    let adapter = mainnet_adapter(&clock).await.unwrap();
    adapter.set_watch_set(&[history_watched()]).await.unwrap();
    let events = FakeEventSink::new();
    let progress = adapter.sync(clock.deadline_in(Duration::from_secs(90)), &events).await.unwrap();
    assert!(progress.is_complete());
    assert!(progress.tip_height().is_some_and(|height| height > 50_000_000));
    let events = events.drain();
    assert!(events.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(_))));
    assert!(events.iter().any(|event| matches!(event, WalletEvent::TxDetected(_))));
}

/// wallet-core's Trust Wallet test phrase holds its funds in a V4R2 wallet, which the
/// probe finds in use on mainnet, and leaves its never-used V3R2 wallet out.
#[tokio::test]
#[ignore = "Queries public mainnet accounts and depends on TON Center availability."]
async fn mainnet_probe_finds_the_wallet_version_a_key_was_used_under() {
    let adapter = mainnet_adapter(&Arc::new(SystemClock)).await.unwrap();
    let key = jelly_ton::ENTRY
        .import_key("c38f49de2fb13223a9e7d37d5d0ffbdd89a5eb7c8b0ee4d1c299f2cefe7dc4a0", None)
        .unwrap()
        .material;
    let candidates: Vec<Address> = [ScriptKind::TonV4r2, ScriptKind::TonV3r2]
        .map(|script| {
            Address::assume_canonical(
                ChainId::Ton,
                jelly_ton::ENTRY.address(&key, Some(script)).unwrap(),
            )
        })
        .into();
    assert_eq!(candidates[0].value(), "UQDYW_1eScJVxtitoBRksvoV9cCYo4uKGWLVNIHB1JqRRyQx");
    let used = adapter.script_probe().unwrap().used(&candidates).await.unwrap();
    assert_eq!(used, [candidates[0].clone()]);
}
