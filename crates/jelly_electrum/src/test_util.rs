//! An in-memory Electrum server.
//!
//! It speaks the real wire protocol over a duplex pipe, so an offline test exercises the
//! same framing, batching, reconnection, and decoding a TLS socket would.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::mem::take;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::constants::genesis_block;
use bitcoin::{BlockHash, Network, Script, block};
use jelly_core::EndpointLabel;
use jelly_net::{Connect, Endpoint, Identity, NetError, Socket};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream, duplex};
use tokio::task::JoinSet;

use crate::chain::ElectrumChain;
use crate::client::ElectrumClient;
use crate::connector::SocketSource;
use crate::error::ElectrumError;
use crate::frame::{FrameReader, encode_line};
use crate::method::Method;
use crate::script_hash::ElectrumScriptHash;

/// The default `server.version` answer, so a client opens without any setup.
const DEFAULT_SERVER_VERSION: &str = r#"["Fulcrum 1.12.0","1.6"]"#;
const DEFAULT_PIPE_CAPACITY_BYTES: usize = 65_536;
const READ_CHUNK_BYTES: usize = 4096;

/// Bitcoin's real genesis hash, which [`FakeElectrum::client`] expects and
/// [`FakeElectrum::serves_bitcoin`] advertises.
pub fn bitcoin_genesis() -> BlockHash {
    genesis_block(Network::Bitcoin).block_hash()
}

/// Bitcoin by block zero alone, without the checkpoint its chain crate adds.
pub fn bitcoin_chain() -> ElectrumChain {
    ElectrumChain { genesis: bitcoin_genesis(), checkpoint: None, is_merge_mined: false }
}

/// Bitcoin's real block-zero header as consensus hex, which passes the chain
/// guard as the `blockchain.block.header` answer.
pub fn bitcoin_genesis_header() -> String {
    serialize_hex(&genesis_block(Network::Bitcoin).header)
}

/// The same header with a different nonce, so it hashes to a chain that isn't
/// Bitcoin.
pub fn foreign_genesis_header() -> String {
    serialize_hex(&foreign_header())
}

/// The hash [`foreign_genesis_header`] hashes to.
pub fn foreign_genesis() -> BlockHash {
    foreign_header().block_hash()
}

/// The URL a pool endpoint names the fake by, which redacts to [`endpoint_label`].
pub const ENDPOINT_URL: &str = "ssl://electrum.invalid:50002";

/// The label a client from [`FakeElectrum::client`] attributes its failures to.
pub fn endpoint_label() -> EndpointLabel {
    EndpointLabel::new("ssl", "electrum.invalid", Some(50002))
}

/// The hex scripthash a request about `script` carries as its first parameter, and the
/// key [`Answer::Keyed`] looks it up by.
pub fn script_hash(script: &Script) -> String {
    ElectrumScriptHash::of(script).to_string()
}

fn foreign_header() -> block::Header {
    let mut header = genesis_block(Network::Bitcoin).header;
    header.nonce = 7;
    header
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The `result` member, verbatim JSON. Every element of a batch gets it.
    Result(String),
    /// One `result` per batch element. A list shorter than the batch gives a short
    /// answer, for testing the length check.
    Each(Vec<String>),
    /// One `result` per batch element, looked up by the element's first parameter,
    /// so the answer doesn't depend on what else the batch asked for. An element
    /// whose key is missing gets no reply, which makes a short answer.
    Keyed(BTreeMap<String, String>),
    /// A JSON-RPC error object on every element.
    Error { code: i64, message: String },
    /// A `Result` held back until the next request is answered, so replies arrive
    /// in the opposite order to the requests.
    Deferred(String),
    /// Never answers, so the request times out.
    Silence,
    /// Closes the socket instead of answering.
    HangUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub method: Method,
    /// The `params` member as it arrived, verbatim JSON.
    pub params: String,
}

#[derive(Debug)]
struct State {
    pipe_capacity_bytes: AtomicUsize,
    queued_answers: Mutex<HashMap<Method, VecDeque<Answer>>>,
    standing_answers: Mutex<HashMap<Method, Answer>>,
    pending_notifications: Mutex<Vec<String>>,
    calls: Mutex<Vec<Call>>,
    sockets_opened: AtomicU64,
    /// One server task per open socket. Each holds the state weakly, so dropping the last
    /// handle drops this set, which aborts them.
    servers: Mutex<JoinSet<()>>,
}

/// A scriptable Electrum server with no socket behind it.
#[derive(Debug, Clone)]
pub struct FakeElectrum {
    state: Arc<State>,
}

impl Default for FakeElectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeElectrum {
    pub fn new() -> Self {
        let standing_answers = HashMap::from([
            (Method::ServerVersion, Answer::Result(DEFAULT_SERVER_VERSION.to_owned())),
            (Method::ServerFeatures, Answer::Result("{}".to_owned())),
            (Method::BlockHeader, Answer::Result(format!("\"{}\"", bitcoin_genesis_header()))),
        ]);
        Self {
            state: Arc::new(State {
                pipe_capacity_bytes: AtomicUsize::new(DEFAULT_PIPE_CAPACITY_BYTES),
                queued_answers: Mutex::new(HashMap::new()),
                standing_answers: Mutex::new(standing_answers),
                pending_notifications: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                sockets_opened: AtomicU64::new(0),
                servers: Mutex::new(JoinSet::new()),
            }),
        }
    }

    /// A tiny pipe forces the reader to reassemble every line from several reads.
    pub fn set_pipe_capacity(&self, bytes: usize) {
        self.state.pipe_capacity_bytes.store(bytes.max(1), Ordering::SeqCst);
    }

    /// Queues one answer for `method`, consumed by the next call.
    pub fn on(&self, method: Method, answer: Answer) {
        self.state.queued_answers.lock().entry(method).or_default().push_back(answer);
    }

    /// Sets the answer `method` falls back to once its queue is empty.
    pub fn always(&self, method: Method, answer: Answer) {
        self.state.standing_answers.lock().insert(method, answer);
    }

    /// Answers `blockchain.block.header` and `server.features` the way a real
    /// Bitcoin server would, so the chain guard passes.
    ///
    /// The header is queued rather than standing, because a sync pass asks for
    /// other heights through the same method.
    pub fn serves_bitcoin(&self) {
        let header = bitcoin_genesis_header();
        self.on(Method::BlockHeader, Answer::Result(format!("\"{header}\"")));
        self.always(
            Method::ServerFeatures,
            Answer::Result(format!(r#"{{"genesis_hash":"{}"}}"#, bitcoin_genesis())),
        );
    }

    /// Queues an unsolicited push, delivered with the next answer.
    ///
    /// Real servers send these once the tip read registers the socket, and the
    /// client ignores their content.
    pub fn push_notification(&self, method: Method, params: &str) {
        let line =
            format!(r#"{{"jsonrpc":"2.0","method":"{}","params":{params}}}"#, method.as_str());
        self.state.pending_notifications.lock().push(line);
    }

    /// Every request the fake has seen, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.state.calls.lock().clone()
    }

    pub fn calls_to(&self, method: Method) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    pub fn has_been_called(&self, method: Method) -> bool {
        self.state.calls.lock().iter().any(|call| call.method == method)
    }

    pub fn forget_calls(&self) {
        self.state.calls.lock().clear();
    }

    /// Sockets opened against this server, each with its own handshake.
    pub fn sockets_opened(&self) -> u64 {
        self.state.sockets_opened.load(Ordering::SeqCst)
    }

    /// A client over this server that expects Bitcoin.
    pub async fn client(&self) -> Result<ElectrumClient, ElectrumError> {
        self.client_expecting(bitcoin_chain()).await
    }

    pub async fn client_expecting(
        &self,
        chain: ElectrumChain,
    ) -> Result<ElectrumClient, ElectrumError> {
        ElectrumClient::open(self.source_for(endpoint_label()), chain).await
    }

    /// A pool connector over this server, running the real chain guard against
    /// `chain`.
    pub fn connector(&self, chain: ElectrumChain) -> FakeConnector {
        FakeConnector { fake: self.clone(), chain }
    }

    /// A socket source under a caller's label, for a pool whose endpoints name
    /// their own hosts.
    fn source_for(&self, endpoint: EndpointLabel) -> Arc<dyn SocketSource> {
        Arc::new(FakeSockets { fake: self.clone(), endpoint })
    }

    fn record(&self, method: Method, params: &str) {
        self.state.calls.lock().push(Call { method, params: params.to_owned() });
    }

    fn next_answer(&self, method: Option<Method>) -> Answer {
        let Some(method) = method else {
            return Answer::Error { code: -32601, message: "unknown method".to_owned() };
        };
        let queued =
            self.state.queued_answers.lock().get_mut(&method).and_then(VecDeque::pop_front);
        if let Some(answer) = queued {
            return answer;
        }
        let standing = self.state.standing_answers.lock().get(&method).cloned();
        standing.unwrap_or_else(|| Answer::Error {
            code: -32601,
            message: format!("unknown method {method}"),
        })
    }

    fn take_notifications(&self) -> Vec<String> {
        take(&mut *self.state.pending_notifications.lock())
    }

    fn spawn_socket(&self) -> Box<dyn Socket> {
        let capacity_bytes = self.state.pipe_capacity_bytes.load(Ordering::SeqCst);
        self.state.sockets_opened.fetch_add(1, Ordering::SeqCst);
        let (client, server) = duplex(capacity_bytes);
        let mut servers = self.state.servers.lock();
        while servers.try_join_next().is_some() {}
        servers.spawn(serve(Arc::downgrade(&self.state), server));
        Box::new(client)
    }

    /// Turns one request line into the line to send back, if any.
    fn respond(&self, line: &[u8]) -> Reply {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return Reply::Nothing;
        };
        let is_batch = value.is_array();
        let requests: Vec<&Value> = match value.as_array() {
            Some(items) => items.iter().collect(),
            None => vec![&value],
        };

        let mut ids = Vec::with_capacity(requests.len());
        let mut keys = Vec::with_capacity(requests.len());
        let mut method: Option<Method> = None;
        for request in &requests {
            ids.push(request.get("id").and_then(Value::as_u64).unwrap_or_default());
            keys.push(
                request
                    .get("params")
                    .and_then(|params| params.get(0))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            let name = request.get("method").and_then(Value::as_str).unwrap_or_default();
            let params = request.get("params").map(ToString::to_string).unwrap_or_default();
            if let Some(parsed) = Method::parse(name) {
                self.record(parsed, &params);
                method = method.or(Some(parsed));
            }
        }

        let answer = self.next_answer(method);
        if answer == Answer::HangUp {
            return Reply::HangUp;
        }
        let is_deferred = matches!(answer, Answer::Deferred(_));
        let Some(elements) = elements(&ids, &keys, &answer) else {
            return Reply::Nothing;
        };
        let text = if is_batch {
            format!("[{}]", elements.join(","))
        } else {
            match elements.into_iter().next() {
                Some(text) => text,
                None => return Reply::Nothing,
            }
        };
        if is_deferred { Reply::Later(text) } else { Reply::Now(text) }
    }
}

/// A pool connector that hands out clients over the in-memory server.
#[derive(Debug, Clone)]
pub struct FakeConnector {
    fake: FakeElectrum,
    chain: ElectrumChain,
}

#[async_trait]
impl Connect<ElectrumClient> for FakeConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<ElectrumClient>, NetError> {
        let sockets = self.fake.source_for(endpoint.label().clone());
        let client = ElectrumClient::open(sockets, self.chain).await?;
        Ok(Arc::new(client))
    }

    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &ElectrumClient,
    ) -> Result<Identity, NetError> {
        Ok(connection.verify_identity().await?)
    }
}

#[derive(Debug, Clone)]
struct FakeSockets {
    fake: FakeElectrum,
    endpoint: EndpointLabel,
}

#[async_trait]
impl SocketSource for FakeSockets {
    fn endpoint(&self) -> &EndpointLabel {
        &self.endpoint
    }

    async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError> {
        Ok(self.fake.spawn_socket())
    }
}

enum Reply {
    Now(String),
    Later(String),
    HangUp,
    Nothing,
}

fn elements(ids: &[u64], keys: &[Option<String>], answer: &Answer) -> Option<Vec<String>> {
    match *answer {
        Answer::Silence | Answer::HangUp => None,
        Answer::Result(ref json) | Answer::Deferred(ref json) => {
            Some(ids.iter().map(|id| success(*id, json)).collect())
        }
        Answer::Each(ref list) => {
            Some(ids.iter().zip(list.iter()).map(|(id, json)| success(*id, json)).collect())
        }
        Answer::Keyed(ref by_key) => Some(
            ids.iter()
                .zip(keys)
                .filter_map(|(id, key)| {
                    key.as_ref().and_then(|key| by_key.get(key)).map(|json| success(*id, json))
                })
                .collect(),
        ),
        Answer::Error { code, ref message } => {
            Some(ids.iter().map(|id| failure(*id, code, message)).collect())
        }
    }
}

fn success(id: u64, json: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{json}}}"#)
}

fn failure(id: u64, code: i64, message: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{}}}}}"#,
        serde_json::to_string(message).unwrap_or_else(|_| String::from(r#""""#))
    )
}

async fn serve(state: Weak<State>, mut stream: DuplexStream) {
    let mut lines = FrameReader::new(usize::MAX);
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    let mut held: Option<String> = None;

    loop {
        let read_bytes = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read_bytes) => read_bytes,
        };
        let Some(slice) = chunk.get(..read_bytes) else { return };
        lines.extend(slice);

        while let Some(line) = lines.take_line() {
            let Some(fake) = state.upgrade().map(|state| FakeElectrum { state }) else { return };
            let mut replies: Vec<String> = Vec::new();
            match fake.respond(&line) {
                Reply::Nothing => {}
                Reply::HangUp => return,
                Reply::Now(text) => {
                    replies.push(text);
                    if let Some(text) = held.take() {
                        replies.push(text);
                    }
                }
                Reply::Later(text) => held = Some(text),
            }
            replies.extend(fake.take_notifications());
            drop(fake);
            for text in replies {
                if stream.write_all(&encode_line(&text)).await.is_err()
                    || stream.flush().await.is_err()
                {
                    return;
                }
            }
        }
    }
}
