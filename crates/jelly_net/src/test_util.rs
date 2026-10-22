//! Test helpers behind the `test-util` feature, which only `[dev-dependencies]` enable: a
//! network that opens nothing until asked, and an in-memory websocket JSON-RPC server.
//!
//! The server speaks the real websocket protocol over a duplex pipe, so an offline test
//! exercises the same handshake, framing, routing, and reconnection a TLS socket would.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use futures_util::{SinkExt as _, StreamExt as _};
use jelly_core::test_util::{FakeClock, MemoryPinStore};
use jelly_core::{Clock, EndpointLabel};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{DuplexStream, duplex};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;

use crate::connect::{Connect, Identity, IdentityCheck};
use crate::dialler::Socket;
use crate::endpoint::Endpoint;
use crate::error::NetError;
use crate::network::Network;
use crate::registry::ChainEntries;
use crate::ws::{WsClient, WsSockets};

const PIPE_CAPACITY_BYTES: usize = 65_536;

/// The address every fake socket answers for. The `.invalid` name never resolves.
const URL: &str = "wss://ws.invalid";

/// A direct network serving `chains`, on a fake clock with no pins stored.
pub fn direct_network(chains: ChainEntries) -> Result<Arc<Network>, NetError> {
    Network::start(Arc::new(FakeClock::default()), Arc::new(MemoryPinStore::new()), None, chains)
}

/// What the fake answers one request with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsAnswer {
    /// The `result` member, verbatim JSON.
    Result(String),
    Error {
        code: i64,
        message: String,
    },
    /// Closes the socket instead of answering.
    HangUp,
}

/// One request the fake saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsCall {
    pub method: String,
    /// The `params` member as it arrived, verbatim JSON.
    pub params: String,
}

#[derive(Debug)]
struct State {
    endpoint: EndpointLabel,
    queued: Mutex<HashMap<String, VecDeque<WsAnswer>>>,
    fallbacks: Mutex<HashMap<String, WsAnswer>>,
    /// Live server tasks subscribe here, so a push reaches every open socket as soon as
    /// it is queued and uncoupled from any answer, as a real notification arrives.
    pushes: broadcast::Sender<String>,
    hangups: broadcast::Sender<()>,
    calls: Mutex<Vec<WsCall>>,
    sockets_opened: AtomicU64,
    /// One server task per open socket. Each holds the state weakly, so dropping the last
    /// handle drops this set, which aborts them.
    servers: Mutex<JoinSet<()>>,
}

/// A scriptable websocket JSON-RPC server with no socket behind it.
#[derive(Debug, Clone)]
pub struct FakeWs {
    state: Arc<State>,
}

impl Default for FakeWs {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWs {
    pub fn new() -> Self {
        let (pushes, _) = broadcast::channel(64);
        let (hangups, _) = broadcast::channel(4);
        Self {
            state: Arc::new(State {
                endpoint: EndpointLabel::from_url(URL),
                queued: Mutex::new(HashMap::new()),
                fallbacks: Mutex::new(HashMap::new()),
                pushes,
                hangups,
                calls: Mutex::new(Vec::new()),
                sockets_opened: AtomicU64::new(0),
                servers: Mutex::new(JoinSet::new()),
            }),
        }
    }

    /// Queues one answer for `method`, consumed by the next call.
    pub fn on(&self, method: &str, answer: WsAnswer) {
        self.state.queued.lock().entry(method.to_owned()).or_default().push_back(answer);
    }

    /// Sets the answer `method` falls back to once its queue is empty.
    pub fn always(&self, method: &str, answer: WsAnswer) {
        self.state.fallbacks.lock().insert(method.to_owned(), answer);
    }

    /// Sends an id-less push to every open socket, as a real subscription notification
    /// arrives. With no open socket it reaches nobody.
    pub fn push_notification(&self, method: &str, params: &str) {
        let line = format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#);
        let _ = self.state.pushes.send(line);
    }

    /// Closes every open socket without a close frame or a request in flight, as a
    /// provider's idle timeout or restart does.
    pub fn hang_up_sockets(&self) {
        let _ = self.state.hangups.send(());
    }

    /// Every request the fake has seen, oldest first.
    pub fn calls(&self) -> Vec<WsCall> {
        self.state.calls.lock().clone()
    }

    pub fn calls_to(&self, method: &str) -> Vec<WsCall> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    pub fn has_been_called(&self, method: &str) -> bool {
        self.state.calls.lock().iter().any(|call| call.method == method)
    }

    /// How many sockets have been opened against this server. Every one carried
    /// its own websocket handshake.
    pub fn sockets_opened(&self) -> u64 {
        self.state.sockets_opened.load(Ordering::SeqCst)
    }

    pub async fn client(&self, clock: Arc<dyn Clock>) -> Result<WsClient, NetError> {
        WsClient::open(self.source(), clock).await
    }

    /// A pool connector over this server that runs `check` the way
    /// [`crate::WsConnector`] does.
    pub fn connector(
        &self,
        clock: Arc<dyn Clock>,
        check: Arc<dyn IdentityCheck<WsClient>>,
    ) -> FakeWsConnector {
        FakeWsConnector { fake: self.clone(), clock, check }
    }

    fn source(&self) -> Arc<dyn WsSockets> {
        Arc::new(FakeWsSockets { fake: self.clone() })
    }

    fn record(&self, method: &str, params: &str) {
        self.state
            .calls
            .lock()
            .push(WsCall { method: method.to_owned(), params: params.to_owned() });
    }

    fn next_answer(&self, method: &str) -> WsAnswer {
        let queued = self.state.queued.lock().get_mut(method).and_then(VecDeque::pop_front);
        if let Some(answer) = queued {
            return answer;
        }
        let fallback = self.state.fallbacks.lock().get(method).cloned();
        fallback.unwrap_or_else(|| WsAnswer::Error {
            code: -32601,
            message: "unknown method".to_owned(),
        })
    }

    /// A fresh byte stream, with a websocket server task behind it.
    fn spawn_socket(&self) -> Box<dyn Socket> {
        self.state.sockets_opened.fetch_add(1, Ordering::SeqCst);
        let (client, server) = duplex(PIPE_CAPACITY_BYTES);
        let mut servers = self.state.servers.lock();
        while servers.try_join_next().is_some() {}
        servers.spawn(serve(Arc::downgrade(&self.state), server));
        Box::new(client)
    }

    /// Turns one request into the frames to send back, if any.
    fn respond(&self, text: &str) -> WsReply {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return WsReply::None;
        };
        let id = value.get("id").and_then(Value::as_u64).unwrap_or_default();
        let method = value.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
        let params = value.get("params").map(ToString::to_string).unwrap_or_default();
        self.record(&method, &params);

        match self.next_answer(&method) {
            WsAnswer::HangUp => WsReply::HangUp,
            WsAnswer::Result(json) => {
                WsReply::Now(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{json}}}"#))
            }
            WsAnswer::Error { code, message } => {
                let quoted =
                    serde_json::to_string(&message).unwrap_or_else(|_| String::from(r#""""#));
                WsReply::Now(format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{quoted}}}}}"#
                ))
            }
        }
    }
}

enum WsReply {
    Now(String),
    HangUp,
    None,
}

async fn serve(state: Weak<State>, stream: DuplexStream) {
    let Some((mut pushes, mut hangups)) =
        state.upgrade().map(|state| (state.pushes.subscribe(), state.hangups.subscribe()))
    else {
        return;
    };
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    loop {
        tokio::select! {
            _ = hangups.recv() => return,
            next = socket.next() => {
                let Some(Ok(message)) = next else { return };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => return,
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                        continue;
                    }
                };
                let Some(fake) = state.upgrade().map(|state| FakeWs { state }) else { return };
                let reply = fake.respond(&text);
                drop(fake);
                match reply {
                    WsReply::None => {}
                    WsReply::HangUp => return,
                    WsReply::Now(reply) => {
                        if socket.send(Message::text(reply)).await.is_err() {
                            return;
                        }
                    }
                }
            }
            line = pushes.recv() => {
                match line {
                    Ok(line) => {
                        if socket.send(Message::text(line)).await.is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        }
    }
}

#[derive(Debug)]
struct FakeWsSockets {
    fake: FakeWs,
}

#[async_trait]
impl WsSockets for FakeWsSockets {
    fn endpoint(&self) -> &EndpointLabel {
        &self.fake.state.endpoint
    }

    fn url(&self) -> &str {
        URL
    }

    async fn open(&self) -> Result<Box<dyn Socket>, NetError> {
        Ok(self.fake.spawn_socket())
    }
}

/// A pool connector that hands out clients over the in-memory server, running a
/// chain's identity check over the scripted answers.
#[derive(Debug, Clone)]
pub struct FakeWsConnector {
    fake: FakeWs,
    clock: Arc<dyn Clock>,
    check: Arc<dyn IdentityCheck<WsClient>>,
}

#[async_trait]
impl Connect<WsClient> for FakeWsConnector {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Arc<WsClient>, NetError> {
        Ok(Arc::new(self.fake.client(Arc::clone(&self.clock)).await?))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        connection: &WsClient,
    ) -> Result<Identity, NetError> {
        self.check.identify(endpoint, connection).await
    }
}
