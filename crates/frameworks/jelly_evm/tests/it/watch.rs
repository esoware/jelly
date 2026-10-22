use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    ChainAdapter as _, ChainId, Clock, Transport,
    test_util::{FakeClock, MemoryCursorStore},
};
use jelly_evm::{EvmAdapter, Method};
use jelly_net::{
    Endpoint, NetRuntime, PacerPolicy, Pool, WsClient,
    test_util::{FakeWs, WsAnswer},
};
use tokio::time::timeout;

use crate::common::{ETHEREUM, FakeNode, Fallible};

fn ws_pool(fake: &FakeWs, clock: &Arc<dyn Clock>) -> Fallible<Arc<Pool<WsClient>>> {
    let runtime = NetRuntime::with_pacing(Arc::clone(clock), PacerPolicy::UNTHROTTLED);
    let endpoint = Endpoint::parse(ChainId::Ethereum, Transport::WsJsonrpc, "wss://ws.invalid")?;
    Ok(Arc::new(runtime.pool(
        ChainId::Ethereum,
        Transport::WsJsonrpc,
        Arc::new(fake.connector(Arc::clone(clock), Arc::new(*ETHEREUM))),
        vec![endpoint],
    )))
}

fn subscribing_fake() -> FakeWs {
    let fake = FakeWs::new();
    fake.always(Method::ChainId.as_str(), WsAnswer::Result(r#""0x1""#.to_owned()));
    fake.always("eth_subscribe", WsAnswer::Result(r#""0xsub""#.to_owned()));
    fake
}

fn adapter(fake: &FakeWs) -> Fallible<EvmAdapter> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    Ok(EvmAdapter::builder(
        ETHEREUM,
        FakeNode::new(),
        Arc::new(MemoryCursorStore::new()),
        Arc::clone(&clock),
    )
    .pushes(ws_pool(fake, &clock)?)
    .build()?)
}

#[tokio::test(start_paused = true)]
async fn a_new_head_push_resolves_the_activity_listener() {
    let fake = subscribing_fake();
    let adapter = Arc::new(adapter(&fake).unwrap());

    // The first listen checks the endpoint's identity, registers the
    // subscription and resolves at once, so sync covers the gap.
    timeout(Duration::from_secs(30), adapter.wait_activity()).await.unwrap();
    assert!(
        fake.has_been_called(Method::ChainId.as_str()),
        "the identity check ran over the socket"
    );
    let registered = fake.calls_to("eth_subscribe");
    assert_eq!(registered.len(), 1);
    assert!(
        registered.first().is_some_and(|call| call.params.contains("newHeads")),
        "the new-heads subscription was registered"
    );

    // The second listen resolves only on a push.
    let waiter = tokio::spawn({
        let adapter = Arc::clone(&adapter);
        async move { adapter.wait_activity().await }
    });
    fake.push_notification("eth_subscription", r#"{"subscription":"0xsub","result":{}}"#);
    timeout(Duration::from_secs(30), waiter).await.expect("the push wakes the listener").unwrap();
    assert_eq!(fake.calls_to("eth_subscribe").len(), 1, "no re-registration was needed");
}

#[tokio::test(start_paused = true)]
async fn a_dead_websocket_is_redialled_and_reregistered() {
    let fake = subscribing_fake();
    let adapter = adapter(&fake).unwrap();
    timeout(Duration::from_secs(30), adapter.wait_activity()).await.unwrap();
    assert_eq!(fake.sockets_opened(), 1);

    // Drop the socket the way a provider's idle timeout does. The next wait
    // re-registers on a fresh socket and resolves as a fresh registration.
    fake.hang_up_sockets();
    timeout(Duration::from_secs(60), adapter.wait_activity()).await.unwrap();
    assert_eq!(fake.sockets_opened(), 2, "a fresh socket was dialled");
    assert_eq!(fake.calls_to("eth_subscribe").len(), 2, "the subscription was registered again");
}
