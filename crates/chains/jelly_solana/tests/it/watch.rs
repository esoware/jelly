use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    ChainAdapter as _, ChainId, Clock, Transport,
    test_util::{FakeClock, MemoryCursorStore},
};
use jelly_net::{
    Endpoint, NetRuntime, PacerPolicy, Pool, WsClient,
    test_util::{FakeWs, WsAnswer},
};
use jelly_solana::{SolanaAdapter, SolanaIdentity};
use tokio::time::timeout;

use crate::common::{
    ACCOUNTS_PER_OWNER, FakeNode, Fallible, OWNER, PEER, SOLANA_TOKEN_IDS, token_account_address,
    watched,
};

fn ws_pool(fake: &FakeWs, clock: &Arc<dyn Clock>) -> Fallible<Arc<Pool<WsClient>>> {
    let runtime = NetRuntime::with_pacing(Arc::clone(clock), PacerPolicy::UNTHROTTLED);
    let endpoint = Endpoint::parse(ChainId::Solana, Transport::WsJsonrpc, "wss://ws.invalid")?;
    Ok(Arc::new(runtime.pool(
        ChainId::Solana,
        Transport::WsJsonrpc,
        Arc::new(fake.connector(Arc::clone(clock), Arc::new(SolanaIdentity))),
        vec![endpoint],
    )))
}

/// A Solana websocket serves subscriptions only, so it refuses every plain
/// RPC method the way every public endpoint does.
fn subscribing_fake() -> FakeWs {
    let fake = FakeWs::new();
    fake.always(
        "getGenesisHash",
        WsAnswer::Error { code: -32601, message: "Method not found".to_owned() },
    );
    fake.always("accountSubscribe", WsAnswer::Result("11".to_owned()));
    fake
}

/// The owner and each of its associated token accounts.
fn watched_accounts(owner: &str) -> Fallible<Vec<String>> {
    let mut accounts = vec![owner.to_owned()];
    for token in SOLANA_TOKEN_IDS {
        accounts.push(token_account_address(owner, token)?);
    }
    Ok(accounts)
}

fn adapter(fake: &FakeWs) -> Fallible<SolanaAdapter> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    Ok(SolanaAdapter::builder(
        FakeNode::new(),
        Arc::new(MemoryCursorStore::new()),
        Arc::clone(&clock),
    )
    .pushes(ws_pool(fake, &clock)?)
    .build())
}

#[tokio::test(start_paused = true)]
async fn a_push_about_a_watched_owner_resolves_the_activity_listener() {
    let fake = subscribing_fake();
    let adapter = Arc::new(adapter(&fake).unwrap());
    adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();

    // The first listen registers the owner and its token accounts and resolves
    // at once, so sync covers the gap.
    timeout(Duration::from_secs(30), adapter.wait_activity()).await.unwrap();
    let accounts = fake.calls_to("accountSubscribe");
    assert_eq!(accounts.len(), ACCOUNTS_PER_OWNER);
    for account in watched_accounts(OWNER).unwrap() {
        assert!(accounts.iter().any(|call| call.params.contains(&account)), "{account}");
    }
    assert!(!fake.has_been_called("logsSubscribe"), "public endpoints never answer it");

    // The second listen resolves only on a push.
    let waiter = tokio::spawn({
        let adapter = Arc::clone(&adapter);
        async move { adapter.wait_activity().await }
    });
    fake.push_notification("accountNotification", r#"{"subscription":11,"result":{}}"#);
    timeout(Duration::from_secs(30), waiter).await.expect("the push wakes the listener").unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_grown_watch_set_registers_the_addition_alone() {
    let fake = subscribing_fake();
    let adapter = Arc::new(adapter(&fake).unwrap());
    adapter.set_watch_set(&[watched(7, OWNER)]).await.unwrap();
    timeout(Duration::from_secs(30), adapter.wait_activity()).await.unwrap();
    assert_eq!(fake.calls_to("accountSubscribe").len(), ACCOUNTS_PER_OWNER);

    adapter.set_watch_set(&[watched(7, OWNER), watched(9, PEER)]).await.unwrap();
    let waiter = tokio::spawn({
        let adapter = Arc::clone(&adapter);
        async move { adapter.wait_activity().await }
    });
    fake.push_notification("accountNotification", r#"{"subscription":11,"result":{}}"#);
    timeout(Duration::from_secs(30), waiter).await.expect("the push wakes the listener").unwrap();

    let accounts = fake.calls_to("accountSubscribe");
    assert_eq!(
        accounts.len(),
        2 * ACCOUNTS_PER_OWNER,
        "only the new owner's accounts were registered"
    );
    for account in watched_accounts(PEER).unwrap() {
        assert!(accounts.iter().any(|call| call.params.contains(&account)), "{account}");
    }
}
