use std::collections::BTreeMap;
use std::fmt;
use std::mem::take;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use jelly_core::{Clock, EndpointLabel, FailureKind, duration_millis};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, sleep, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use url::Url;

use crate::connect::{Connect, Identity, IdentityCheck};
use crate::credentials::Credentials;
use crate::dialer::{Dialer, Socket};
use crate::endpoint::{Endpoint, UrlRejection};
use crate::error::NetError;
use crate::jsonrpc::{JsonRpcRequest, JsonRpcResponse, decode_result, envelope_result};

const TARGET: &str = "jelly::net::ws";

/// How many outgoing frames may queue before a caller waits.
const QUEUE_DEPTH: usize = 32;
/// How long one request waits before its socket is closed as half-answered. Every
/// request here is a subscription or a small read, so this much silence means stalled,
/// not busy.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The write task pings this often, so a provider's idle timeout never closes a socket
/// that's only waiting for a rare push.
const KEEPALIVE: Duration = Duration::from_secs(30);
/// Subscription results and notifications are small, so anything past this is an
/// attack, not an answer.
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Opens a fresh byte stream to one endpoint whenever the client's socket is gone.
///
/// Production dials through [`Dialer`], and a test opens an in-memory pipe, so the
/// shipped handshake, framing, and routing run without a network.
#[async_trait]
pub(crate) trait WsSockets: Send + Sync + fmt::Debug {
    /// The redacted endpoint every failure on these sockets is attributed to.
    fn endpoint(&self) -> &EndpointLabel;

    /// The URL the websocket handshake requests.
    fn url(&self) -> &str;

    async fn open(&self) -> Result<Box<dyn Socket>, NetError>;
}

/// One subscription request, registered on a socket so the server starts pushing.
///
/// The returned subscription id is discarded. A redial registers afresh and pushed data
/// is never read, so there's nothing to route by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsSubscription {
    method: String,
    params: serde_json::Value,
}

impl WsSubscription {
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self { method: method.into(), params }
    }
}

/// A cached websocket endpoint handle that redials on its own.
///
/// Register subscriptions through the pool so failures count against the endpoint's
/// health. A push only wakes sync, which reads through pools that verify their network,
/// so a misbehaving websocket backend can cause spurious wakes but never supply wallet
/// data.
pub struct WsClient {
    sockets: Arc<dyn WsSockets>,
    clock: Arc<dyn Clock>,
    connection: AsyncMutex<Option<Arc<WsConnection>>>,
    watching: AsyncMutex<Option<WatchState>>,
    notice_wait: AsyncMutex<()>,
    sockets_opened: AtomicU64,
}

/// What [`WsClient::register`] put on which socket, and how many pushes it has
/// already reported.
struct WatchState {
    connection: Arc<WsConnection>,
    registered: Vec<WsSubscription>,
    seen: u64,
    /// When the last wake was reported, on the injected clock. The floor in
    /// [`WsClient::wait_notice`] counts from it.
    last_wake_millis: u64,
    /// A registration not yet reported as a wake. Nothing was watched before it, so the
    /// first wait after registering doesn't wait for a push and a pass covers the gap.
    is_fresh: bool,
}

impl fmt::Debug for WsClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsClient")
            .field("endpoint", self.endpoint())
            .field("sockets_opened", &self.sockets_opened.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl WsClient {
    /// Opens the first socket and completes the websocket handshake on it, so a
    /// client that exists has talked to its server.
    pub(crate) async fn open(
        sockets: Arc<dyn WsSockets>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, NetError> {
        let connection = WsConnection::open(sockets.as_ref()).await?;
        Ok(Self {
            sockets,
            clock,
            connection: AsyncMutex::new(Some(Arc::new(connection))),
            watching: AsyncMutex::new(None),
            notice_wait: AsyncMutex::new(()),
            sockets_opened: AtomicU64::new(1),
        })
    }

    pub fn endpoint(&self) -> &EndpointLabel {
        self.sockets.endpoint()
    }

    pub async fn call<P: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<T, NetError> {
        let connection = self.connection().await?;
        let raw = connection.call(method, params).await?;
        decode_result(self.endpoint(), &raw)
    }

    /// Registers subscriptions on the current socket, redialling if needed.
    ///
    /// Only subscriptions not yet registered on this socket are sent. One dropped from the
    /// set stays until the next redial and can only cause spurious wakes.
    pub async fn register(&self, subscriptions: &[WsSubscription]) -> Result<(), NetError> {
        let connection = self.connection().await?;
        let mut guard = self.watching.lock().await;
        match guard.as_mut() {
            Some(state) if Arc::ptr_eq(&state.connection, &connection) => {
                for subscription in subscriptions {
                    if !state.registered.contains(subscription) {
                        let _: Box<RawValue> =
                            connection.call(&subscription.method, &subscription.params).await?;
                        state.registered.push(subscription.clone());
                    }
                }
            }
            _ => {
                let mut registered = Vec::with_capacity(subscriptions.len());
                for subscription in subscriptions {
                    let _: Box<RawValue> =
                        connection.call(&subscription.method, &subscription.params).await?;
                    registered.push(subscription.clone());
                }
                let seen = *connection.notices().borrow();
                let last_wake_millis = guard.as_ref().map_or(0, |state| state.last_wake_millis);
                *guard = Some(WatchState {
                    connection: Arc::clone(&connection),
                    registered,
                    seen,
                    last_wake_millis,
                    is_fresh: true,
                });
            }
        }
        Ok(())
    }

    /// Waits for a push on the registered socket. After a fresh registration it doesn't
    /// wait for one, so sync covers the time nothing was watched.
    ///
    /// Wakes come at most once per `floor`, and pushes inside it collapse into one wake.
    /// After an error, register again through the pool. Canceling keeps unread pushes for
    /// the next call.
    pub async fn wait_notice(&self, floor: Duration) -> Result<(), NetError> {
        let _waiting = self.notice_wait.lock().await;
        loop {
            let (connection, seen, is_fresh, last_wake_millis) = {
                let guard = self.watching.lock().await;
                let Some(state) = guard.as_ref() else {
                    return Err(NetError::protocol(
                        self.endpoint(),
                        "nothing is registered to wait on",
                    ));
                };
                (Arc::clone(&state.connection), state.seen, state.is_fresh, state.last_wake_millis)
            };
            if !is_fresh {
                let mut notices = connection.notices();
                if *notices.borrow() == seen && notices.changed().await.is_err() {
                    return Err(closed(self.endpoint()));
                }
            }

            let floor_millis = duration_millis(floor);
            let since_millis = self.clock.elapsed_millis().saturating_sub(last_wake_millis);
            if since_millis < floor_millis {
                sleep(Duration::from_millis(floor_millis.saturating_sub(since_millis))).await;
            }

            let mut guard = self.watching.lock().await;
            let Some(state) = guard.as_mut() else {
                continue;
            };
            if !Arc::ptr_eq(&state.connection, &connection) {
                continue;
            }
            state.is_fresh = false;
            state.seen = *connection.notices().borrow();
            state.last_wake_millis = self.clock.elapsed_millis();
            return if connection.is_live() { Ok(()) } else { Err(closed(self.endpoint())) };
        }
    }

    /// A live socket, dialing a fresh one when the last is gone.
    ///
    /// The lock is held across the dial, so concurrent callers wait for one redial instead
    /// of each opening a socket to the same host.
    async fn connection(&self) -> Result<Arc<WsConnection>, NetError> {
        let mut guard = self.connection.lock().await;
        if let Some(existing) = guard.as_ref()
            && existing.is_live()
        {
            return Ok(Arc::clone(existing));
        }
        *guard = None;
        let fresh = Arc::new(WsConnection::open(self.sockets.as_ref()).await?);
        self.sockets_opened.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            target: TARGET,
            endpoint = %self.endpoint(),
            sockets = self.sockets_opened.load(Ordering::Relaxed),
            "websocket opened"
        );
        *guard = Some(Arc::clone(&fresh));
        Ok(fresh)
    }
}

/// The socket closed with nothing answered, or before the wait was over.
fn closed(endpoint: &EndpointLabel) -> NetError {
    NetError::protocol(endpoint, "the websocket closed")
}

/// The handshake request for an endpoint's canonical URL.
///
/// The canonical form drops the slash of an empty path, which the request target needs,
/// and the URL parser puts it back. The userinfo goes out as basic credentials, because
/// tungstenite would otherwise send it nowhere.
fn handshake_request(endpoint: &EndpointLabel, url: &str) -> Result<Request, NetError> {
    let mut parsed =
        Url::parse(url).map_err(|_| NetError::InvalidUrl { reason: UrlRejection::Unparsable })?;
    let credentials = Credentials::of(&parsed);
    // Clearing the userinfo fails only for a URL with no host, and every endpoint has one.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);

    let refused = |source| NetError::endpoint(endpoint, FailureKind::Protocol, source);
    let mut request = parsed.as_str().into_client_request().map_err(refused)?;
    if let Some(credentials) = credentials {
        let mut value =
            HeaderValue::try_from(credentials.basic()).map_err(|source| refused(source.into()))?;
        value.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    Ok(request)
}

/// One websocket over one byte stream.
///
/// A request that times out closes the socket, so a half-answered connection is never
/// reused. A frame with no id is a server push. It bumps the counter watchers wait on,
/// and its content is never read.
struct WsConnection {
    inner: Arc<Inner>,
    outgoing: mpsc::Sender<Message>,
    next_id: AtomicU64,
    tasks: [JoinHandle<()>; 2],
}

/// Aborts both tasks, so neither the reader nor the writer outlives the connection.
impl Drop for WsConnection {
    fn drop(&mut self) {
        self.inner.close();
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl fmt::Debug for WsConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsConnection")
            .field("endpoint", &self.inner.endpoint)
            .field("is_live", &self.is_live())
            .finish_non_exhaustive()
    }
}

type Ws = WebSocketStream<Box<dyn Socket>>;
type Answer = Result<Box<RawValue>, NetError>;

impl WsConnection {
    async fn open(sockets: &dyn WsSockets) -> Result<Self, NetError> {
        let endpoint = sockets.endpoint().clone();
        let request = handshake_request(&endpoint, sockets.url())?;
        let io = sockets.open().await?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let (stream, _response) =
            tokio_tungstenite::client_async_with_config(request, io, Some(config))
                .await
                .map_err(|source| NetError::endpoint(&endpoint, FailureKind::Protocol, source))?;
        let (sink, reader) = stream.split();

        let (outgoing, queue) = mpsc::channel(QUEUE_DEPTH);
        let (notices, _) = watch::channel(0u64);
        let inner = Arc::new(Inner {
            endpoint,
            is_live: AtomicBool::new(true),
            waiters: Mutex::default(),
            notices,
        });
        let tasks = [
            tokio::spawn(read_loop(reader, Arc::clone(&inner), outgoing.clone())),
            tokio::spawn(write_loop(sink, queue, Arc::clone(&inner))),
        ];
        Ok(Self { inner, outgoing, next_id: AtomicU64::new(1), tasks })
    }

    fn is_live(&self) -> bool {
        self.inner.is_live.load(Ordering::Acquire)
    }

    /// Bumped once per server push on this socket, and once when the socket
    /// closes so a waiter wakes and finds [`WsConnection::is_live`] false.
    fn notices(&self) -> watch::Receiver<u64> {
        self.inner.notices.subscribe()
    }

    /// The waiter is registered before the frame goes out, so a reply can't arrive before
    /// there's somewhere to put it.
    async fn call<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Box<RawValue>, NetError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params)?;
        let wait = self.inner.register(id)?;
        if self.outgoing.send(Message::text(request.as_str())).await.is_err() {
            self.inner.abandon(id);
            return Err(closed(&self.inner.endpoint));
        }
        match timeout(REQUEST_TIMEOUT, wait).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(closed(&self.inner.endpoint)),
            Err(_) => {
                self.inner.abandon(id);
                self.inner.close();
                Err(NetError::timed_out(&self.inner.endpoint, REQUEST_TIMEOUT))
            }
        }
    }
}

struct Inner {
    endpoint: EndpointLabel,
    is_live: AtomicBool,
    /// Registration checks `is_live` under this lock and closing drains it after clearing
    /// `is_live`, so no waiter is added once the drain has run.
    waiters: Mutex<BTreeMap<u64, oneshot::Sender<Answer>>>,
    /// A counter of server pushes. A push is a hint to ask again, never data to act on.
    notices: watch::Sender<u64>,
}

impl Inner {
    fn register(&self, id: u64) -> Result<oneshot::Receiver<Answer>, NetError> {
        let mut waiters = self.waiters.lock();
        if !self.is_live.load(Ordering::Acquire) {
            return Err(closed(&self.endpoint));
        }
        let (reply, wait) = oneshot::channel();
        waiters.insert(id, reply);
        Ok(wait)
    }

    fn abandon(&self, id: u64) {
        self.waiters.lock().remove(&id);
    }

    fn close(&self) {
        if !self.is_live.swap(false, Ordering::AcqRel) {
            return;
        }
        let waiters = take(&mut *self.waiters.lock());
        for reply in waiters.into_values() {
            let _ = reply.send(Err(closed(&self.endpoint)));
        }
        // Wake every notification waiter so it sees the socket is dead.
        self.notices.send_modify(|count| *count = count.wrapping_add(1));
    }

    /// A message with no id is a server push and bumps the counter. An id nobody waits on
    /// is an answer that arrived after its caller gave up, and is dropped. Anything that
    /// isn't a JSON-RPC object returns false, which closes the socket.
    ///
    /// A `result` of null is still an answer and reaches its caller.
    fn deliver(&self, bytes: &[u8]) -> bool {
        let Ok(envelope) = serde_json::from_slice::<JsonRpcResponse<Box<RawValue>>>(bytes) else {
            return false;
        };
        let Some(id) = envelope.id else {
            self.notices.send_modify(|count| *count = count.wrapping_add(1));
            return true;
        };
        let waiter = self.waiters.lock().remove(&id);
        if let Some(reply) = waiter {
            let _ = reply.send(envelope_result(&self.endpoint, envelope));
        }
        true
    }
}

async fn read_loop(mut reader: SplitStream<Ws>, inner: Arc<Inner>, control: mpsc::Sender<Message>) {
    while let Some(next) = reader.next().await {
        let Ok(message) = next else { break };
        match message {
            Message::Text(text) => {
                if !inner.deliver(text.as_bytes()) {
                    break;
                }
            }
            Message::Binary(bytes) => {
                if !inner.deliver(&bytes) {
                    break;
                }
            }
            // Dropping the pong when the queue is full is fine; the server pings again.
            Message::Ping(payload) => {
                let _ = control.try_send(Message::Pong(payload));
            }
            Message::Close(_) => break,
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    inner.close();
}

async fn write_loop(
    mut sink: SplitSink<Ws, Message>,
    mut queue: mpsc::Receiver<Message>,
    inner: Arc<Inner>,
) {
    let mut keepalive = interval(KEEPALIVE);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick is immediate and the handshake just proved the socket.
    keepalive.tick().await;
    loop {
        tokio::select! {
            outgoing = queue.recv() => match outgoing {
                Some(message) => {
                    if sink.send(message).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = keepalive.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = sink.close().await;
    inner.close();
}

/// Dials one endpoint's byte streams through the connector's [`Dialer`].
#[derive(Debug)]
struct DialedSockets {
    dialer: Dialer,
    endpoint: Endpoint,
}

#[async_trait]
impl WsSockets for DialedSockets {
    fn endpoint(&self) -> &EndpointLabel {
        self.endpoint.label()
    }

    fn url(&self) -> &str {
        self.endpoint.url()
    }

    async fn open(&self) -> Result<Box<dyn Socket>, NetError> {
        self.dialer.connect(&self.endpoint).await
    }
}

/// Builds websocket clients for a pool, and runs the chain's identity check over the
/// socket itself, so a pool never waits on pushes from a host serving another chain.
#[derive(Debug, Clone)]
pub struct WsConnector {
    dialer: Dialer,
    clock: Arc<dyn Clock>,
    check: Arc<dyn IdentityCheck<WsClient>>,
}

impl WsConnector {
    pub const fn new(
        dialer: Dialer,
        clock: Arc<dyn Clock>,
        check: Arc<dyn IdentityCheck<WsClient>>,
    ) -> Self {
        Self { dialer, clock, check }
    }
}

#[async_trait]
impl Connect<WsClient> for WsConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<WsClient>, NetError> {
        let sockets =
            Arc::new(DialedSockets { dialer: self.dialer.clone(), endpoint: endpoint.clone() });
        let client = WsClient::open(sockets, Arc::clone(&self.clock)).await?;
        Ok(Arc::new(client))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        connection: &WsClient,
    ) -> Result<Identity, NetError> {
        self.check.identify(endpoint, connection).await
    }
}

/// The pool keeps one client per endpoint and hands `Arc`s of it to concurrent
/// tasks.
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<WsClient>();
