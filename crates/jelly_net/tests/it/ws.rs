use std::io;
use std::net::SocketAddr;
use std::slice::from_ref;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{ChainId, Clock, Transport, test_util::FakeClock};
use jelly_net::{
    Connect as _, Endpoint, Identity, IdentityCheck, NetError, NetRuntime, NoIdentity, PacerPolicy,
    Pool, WsClient, WsConnector, WsSubscription,
    test_util::{FakeWs, WsAnswer, direct_network},
};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};

use crate::common::{read_head, rotate};

const NO_FLOOR: Duration = Duration::ZERO;

/// Asks the socket one question and admits only one answer, which is the shape a
/// chain's own check takes.
#[derive(Debug)]
struct AnswerCheck {
    method: &'static str,
    expected: &'static str,
}

#[async_trait]
impl IdentityCheck<WsClient> for AnswerCheck {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &WsClient,
    ) -> Result<Identity, NetError> {
        let found: String = connection.call(self.method, &[(); 0]).await?;
        if found == self.expected {
            return Ok(Identity::confirmed(found));
        }
        Ok(Identity::Mismatch { expected: self.expected.to_owned(), found })
    }
}

fn ethereum_check() -> Arc<AnswerCheck> {
    Arc::new(AnswerCheck { method: "eth_chainId", expected: "0x1" })
}

fn clock() -> Arc<FakeClock> {
    Arc::new(FakeClock::default())
}

fn heads() -> WsSubscription {
    WsSubscription::new("eth_subscribe", serde_json::json!(["newHeads"]))
}

fn subscribing_fake() -> FakeWs {
    let fake = FakeWs::new();
    fake.always("eth_subscribe", WsAnswer::Result(r#""0xsub""#.to_owned()));
    fake.always("eth_chainId", WsAnswer::Result(r#""0x1""#.to_owned()));
    fake
}

async fn client(fake: &FakeWs) -> Result<WsClient, NetError> {
    fake.client(clock()).await
}

/// An Ethereum websocket pool of one endpoint over `fake`, guarded by the Ethereum check.
fn ws_pool(fake: &FakeWs) -> Result<Pool<WsClient>, NetError> {
    let clock: Arc<dyn Clock> = clock();
    let runtime = NetRuntime::with_pacing(Arc::clone(&clock), PacerPolicy::UNTHROTTLED);
    Ok(runtime.pool(
        ChainId::Ethereum,
        Transport::WsJsonrpc,
        Arc::new(fake.connector(clock, ethereum_check())),
        vec![Endpoint::parse(ChainId::Ethereum, Transport::WsJsonrpc, "wss://ws.invalid")?],
    ))
}

#[tokio::test]
async fn a_call_round_trips_over_the_real_handshake() {
    let fake = subscribing_fake();
    let client = client(&fake).await.expect("the handshake completes over the pipe");
    let answer: String = client.call("eth_chainId", &[(); 0]).await.expect("an answer");
    assert_eq!(answer, "0x1");
    assert_eq!(fake.sockets_opened(), 1);
}

#[tokio::test]
async fn an_rpc_error_reaches_the_caller_as_the_rpc_variant() {
    let fake = FakeWs::new();
    fake.always("eth_subscribe", WsAnswer::Error { code: -32601, message: "no subs".to_owned() });
    let client = client(&fake).await.expect("the handshake completes over the pipe");
    let error = client.register(&[heads()]).await.expect_err("the refusal reaches the caller");
    assert!(matches!(error, NetError::Rpc { code: -32601, .. }), "{error}");
}

#[tokio::test(start_paused = true)]
async fn a_fresh_registration_wakes_once_and_a_push_wakes_the_next_wait() {
    let fake = subscribing_fake();
    let client = Arc::new(client(&fake).await.expect("the handshake completes over the pipe"));
    client.register(&[heads()]).await.expect("the registration goes out");
    assert_eq!(fake.calls_to("eth_subscribe").len(), 1);

    // The first wait covers the gap, so it resolves without any push.
    timeout(Duration::from_secs(5), client.wait_notice(NO_FLOOR))
        .await
        .expect("a fresh registration is itself a wake")
        .expect("the wake is an activity hint");

    let waiter = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.wait_notice(NO_FLOOR).await }
    });

    // The fake delivers the push at once, and the counter keeps the wake race-free
    // whatever the request's timing.
    fake.push_notification("eth_subscription", r#"{"subscription":"0xsub","result":{}}"#);
    let _: String = client.call("eth_chainId", &[(); 0]).await.expect("a flushing request");

    timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the push wakes the wait")
        .expect("the waiter task runs")
        .expect("the woken wait is an activity hint");
}

#[tokio::test]
async fn a_grown_registration_set_sends_only_the_additions() {
    let fake = subscribing_fake();
    let client = client(&fake).await.expect("the handshake completes over the pipe");
    let first = heads();
    let second =
        WsSubscription::new("eth_subscribe", serde_json::json!(["newPendingTransactions"]));

    client.register(from_ref(&first)).await.expect("one registration");
    assert_eq!(fake.calls_to("eth_subscribe").len(), 1);

    client.register(&[first, second]).await.expect("the grown set registers");
    assert_eq!(fake.calls_to("eth_subscribe").len(), 2, "the addition alone was sent");
}

#[tokio::test(start_paused = true)]
async fn a_notification_wait_does_not_block_new_registrations() {
    let fake = subscribing_fake();
    let client = client(&fake).await.expect("the handshake completes over the pipe");
    client.register(&[heads()]).await.expect("the registration goes out");
    client.wait_notice(NO_FLOOR).await.expect("the gap cover");
    let waiting = client.wait_notice(NO_FLOOR);
    tokio::pin!(waiting);
    let subscriptions =
        [heads(), WsSubscription::new("eth_subscribe", serde_json::json!(["logs"]))];
    tokio::select! {
        biased;
        outcome = &mut waiting => panic!("no push was sent: {outcome:?}"),
        registered = timeout(Duration::from_secs(1), client.register(&subscriptions)) => {
            registered
                .expect("registration is not blocked by the waiter")
                .expect("the grown set registers");
        }
    }
    assert_eq!(fake.calls_to("eth_subscribe").len(), 2);
    fake.push_notification("eth_subscription", r#"{"result":1}"#);
    timeout(Duration::from_secs(1), waiting)
        .await
        .expect("the push wakes the wait")
        .expect("the woken wait is an activity hint");
}

#[tokio::test(start_paused = true)]
async fn registration_during_the_wake_floor_does_not_consume_a_canceled_fresh_wake() {
    let fake = subscribing_fake();
    let client = client(&fake).await.expect("the handshake completes over the pipe");
    client.register(&[heads()]).await.expect("the registration goes out");
    let floor = Duration::from_secs(5);
    {
        let waiting = client.wait_notice(floor);
        tokio::pin!(waiting);
        let subscriptions =
            [heads(), WsSubscription::new("eth_subscribe", serde_json::json!(["logs"]))];
        tokio::select! {
            biased;
            outcome = &mut waiting => panic!("the floor has not elapsed: {outcome:?}"),
            registered = timeout(Duration::from_secs(1), client.register(&subscriptions)) => {
                registered
                    .expect("the wake floor does not lock registrations")
                    .expect("the grown set registers");
            }
        }
    }
    timeout(Duration::from_secs(1), client.wait_notice(NO_FLOOR))
        .await
        .expect("cancellation retains the fresh wake")
        .expect("the fresh wake is an activity hint");
}

#[tokio::test(start_paused = true)]
async fn a_dead_socket_fails_the_wait_and_reregistration_dials_afresh() {
    let fake = subscribing_fake();
    let client = Arc::new(client(&fake).await.expect("the handshake completes over the pipe"));
    client.register(&[heads()]).await.expect("the registration goes out");
    client.wait_notice(NO_FLOOR).await.expect("the gap cover");

    let waiter = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.wait_notice(NO_FLOOR).await }
    });

    // Killing the socket must wake the waiter with an error, not strand it.
    fake.on("eth_chainId", WsAnswer::HangUp);
    let _ = client.call::<_, String>("eth_chainId", &[(); 0]).await;
    let outcome = timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the close wakes the wait")
        .expect("the waiter task runs");
    assert!(
        matches!(outcome, Err(NetError::Protocol { detail: "the websocket closed", .. })),
        "a dead socket is an error, not activity: {outcome:?}"
    );

    // Registering again re-dials underneath and registers afresh.
    client.register(&[heads()]).await.expect("the re-registration");
    assert_eq!(fake.sockets_opened(), 2, "a fresh socket was dialled");
    assert_eq!(fake.calls_to("eth_subscribe").len(), 2, "the set was registered again");
}

#[tokio::test(start_paused = true)]
async fn pushes_inside_the_floor_collapse_into_one_wake() {
    let fake = subscribing_fake();
    let client = Arc::new(client(&fake).await.expect("the handshake completes over the pipe"));
    let floor = Duration::from_secs(5);
    client.register(&[heads()]).await.expect("the registration goes out");
    client.wait_notice(floor).await.expect("the gap cover");

    fake.push_notification("eth_subscription", r#"{"subscription":"0xsub","result":1}"#);
    let _: String = client.call("eth_chainId", &[(); 0]).await.expect("a flushing request");
    let waiter = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.wait_notice(floor).await }
    });

    // Paused time moves only once the waiter is asleep on the floor, so this push lands
    // inside it.
    sleep(Duration::from_secs(1)).await;
    fake.push_notification("eth_subscription", r#"{"subscription":"0xsub","result":2}"#);
    let _: String = client.call("eth_chainId", &[(); 0]).await.expect("a flushing request");

    waiter.await.expect("the waiter task runs").expect("both pushes are one wake");
    let starved = timeout(Duration::from_secs(1), client.wait_notice(floor)).await;
    assert!(starved.is_err(), "everything inside the floor was drained by the one wake");
}

#[tokio::test]
async fn the_pool_refuses_a_websocket_on_the_wrong_network() {
    let fake = FakeWs::new();
    fake.always("eth_chainId", WsAnswer::Result(r#""0x38""#.to_owned()));
    let pool = ws_pool(&fake).expect("a pool over the fake");

    let error = pool
        .call("ws watch", rotate, async |client| {
            client.register(&[heads()]).await?;
            Ok(client)
        })
        .await
        .expect_err("a bsc answer is not an ethereum endpoint");
    assert!(matches!(error, NetError::AllFailed { .. }), "{error}");
}

/// The head of the handshake request the shipped websocket connector sends.
///
/// The connector dials direct with no identity check to a loopback server that reads the
/// handshake request, refuses it, and hands back the head it read. `address` turns the
/// server's address into the endpoint URL.
async fn handshake_head(address: impl FnOnce(SocketAddr) -> String) -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = address(listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let head = read_head(&mut stream).await?;
        stream.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n").await?;
        Ok::<String, io::Error>(head)
    });

    let network = direct_network(&[]).map_err(io::Error::other)?;
    let dialler = network.ws_dialler().map_err(io::Error::other)?;
    let connector = WsConnector::new(dialler, Arc::clone(network.clock()), Arc::new(NoIdentity));
    let endpoint =
        Endpoint::parse(ChainId::Ethereum, Transport::WsJsonrpc, &url).map_err(io::Error::other)?;

    let _ = connector.connect(&endpoint).await;
    server.await.map_err(io::Error::other)?
}

/// A websocket key in the query of the root path is asked for at `/?key`.
///
/// Providers such as `wss://mainnet.helius-rpc.com/?api-key=...` take their key that way.
/// RFC 6455 section 4.1 and RFC 9112 section 3.2.1 make the request target an absolute
/// path, so it starts with a slash, followed by the query.
#[tokio::test]
async fn a_websocket_url_with_a_query_on_its_root_asks_for_the_root_path() {
    let head = handshake_head(|address| format!("ws://{address}/?api-key=KEY"))
        .await
        .expect("the handshake was read");

    let request_line = head.lines().next().unwrap_or_default();
    assert_eq!(
        request_line, "GET /?api-key=KEY HTTP/1.1",
        "the handshake should ask for the root path with the key in its query"
    );
}

/// The websocket handshake carries the credentials in the endpoint URL.
///
/// The shared HTTP client sends the userinfo of an endpoint URL as basic credentials, so
/// a node behind basic authentication answers the HTTP transports. The websocket
/// handshake to the same node must carry the credentials the user typed as well.
#[tokio::test]
async fn the_websocket_handshake_presents_the_credentials_in_the_endpoint_url() {
    let head = handshake_head(|address| format!("ws://user:secret@{address}"))
        .await
        .expect("the handshake was read");

    // Base64 of `user:secret`, the RFC 7617 basic credentials.
    let has_credentials =
        head.lines().filter_map(|line| line.split_once(':')).any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value.trim() == "Basic dXNlcjpzZWNyZXQ="
        });
    assert!(
        has_credentials,
        "the handshake should present the endpoint's credentials, but it asked without them: \
         {head}"
    );
}

#[tokio::test]
async fn the_pool_admits_a_websocket_that_answers_for_its_own_network() {
    let fake = subscribing_fake();
    let pool = ws_pool(&fake).expect("a pool over the fake");

    let client = pool
        .call("ws watch", rotate, async |client| {
            client.register(&[heads()]).await?;
            Ok(client)
        })
        .await
        .expect("the endpoint is admitted and registered");
    client.wait_notice(NO_FLOOR).await.expect("the fresh registration is a wake");
}
