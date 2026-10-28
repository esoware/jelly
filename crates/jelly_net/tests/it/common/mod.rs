use std::future::pending;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{ChainId, FailureKind, Transport, test_util::FakeClock};
use jelly_net::{
    Connect, Endpoint, HealthTracker, Identity, NetError, NetRuntime, Outcome, PacerPolicy, Pool,
};
use parking_lot::Mutex;
use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

pub(crate) const ENDPOINT_A: &str = "https://a.example";
pub(crate) const ENDPOINT_B: &str = "https://b.example";
pub(crate) const ENDPOINT_C: &str = "https://c.example";

pub(crate) const fn rotate(_: &NetError) -> Outcome {
    Outcome::Rotate
}

/// Reads one HTTP head through its blank line, a byte at a time, so whatever follows it
/// on the stream, such as a TLS handshake inside a tunnel, is left unread.
pub(crate) async fn read_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut head: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        head.extend_from_slice(&byte);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// A loopback server that hands every connection to `serve`, until its handle is aborted.
pub(crate) async fn loopback_server<F, Fut>(serve: F) -> io::Result<(SocketAddr, JoinHandle<()>)>
where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            connections.spawn(serve(stream));
        }
    });
    Ok((address, server))
}

/// Stands in for whatever a chain talks through. It carries the canonical URL
/// so a test can assert which endpoint answered.
#[derive(Debug)]
pub(crate) struct FakeConnection {
    pub(crate) url: String,
}

/// A connector with no sockets behind it, set up through its public fields.
#[derive(Debug)]
pub(crate) struct FakeConnector {
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) connects: Mutex<Vec<String>>,
    pub(crate) identity_checks: Mutex<Vec<String>>,
    pub(crate) refused_urls: Mutex<Vec<String>>,
    pub(crate) wrong_network_urls: Mutex<Vec<String>>,
    /// How far the fake clock moves during each connect, so a test can spend part of an
    /// attempt's budget on the dial.
    pub(crate) advance_on_connect: Mutex<Duration>,
    /// Endpoints whose identity check never answers.
    pub(crate) silent_identity_urls: Mutex<Vec<String>>,
}

impl FakeConnector {
    const fn new(clock: Arc<FakeClock>) -> Self {
        Self {
            clock,
            connects: Mutex::new(Vec::new()),
            identity_checks: Mutex::new(Vec::new()),
            refused_urls: Mutex::new(Vec::new()),
            wrong_network_urls: Mutex::new(Vec::new()),
            advance_on_connect: Mutex::new(Duration::ZERO),
            silent_identity_urls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Connect<FakeConnection> for FakeConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<FakeConnection>, NetError> {
        let url = endpoint.url().to_owned();
        self.connects.lock().push(url.clone());
        self.clock.advance(*self.advance_on_connect.lock());
        if self.refused_urls.lock().contains(&url) {
            return Err(NetError::endpoint(
                endpoint.label(),
                FailureKind::Connect,
                io::Error::other("refused"),
            ));
        }
        Ok(Arc::new(FakeConnection { url }))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        _connection: &FakeConnection,
    ) -> Result<Identity, NetError> {
        let url = endpoint.url().to_owned();
        self.identity_checks.lock().push(url.clone());
        if self.silent_identity_urls.lock().contains(&url) {
            pending::<()>().await;
        }
        if self.wrong_network_urls.lock().contains(&url) {
            return Ok(Identity::Mismatch { expected: "1".to_owned(), found: "56".to_owned() });
        }
        Ok(Identity::confirmed("chain id 1"))
    }
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) runtime: Arc<NetRuntime>,
    pub(crate) health: Arc<HealthTracker>,
    pub(crate) connector: Arc<FakeConnector>,
    pub(crate) pool: Pool<FakeConnection>,
}

impl Harness {
    /// Another pool over the same runtime and connector, so it shares this one's clock,
    /// health, and pacing.
    pub(crate) fn pool_over(&self, endpoints: Vec<Endpoint>) -> Pool<FakeConnection> {
        self.runtime.pool(
            ChainId::Ethereum,
            Transport::HttpJsonrpc,
            Arc::<FakeConnector>::clone(&self.connector),
            endpoints,
        )
    }
}

/// An Ethereum JSON-RPC endpoint, which most of these tests want.
pub(crate) fn endpoint(url: &str) -> Result<Endpoint, NetError> {
    Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, url)
}

/// A pool over `urls`, on a clock only a test moves and with pacing switched
/// off so the walk is the only thing under test.
pub(crate) fn harness(urls: &[&str]) -> Result<Harness, NetError> {
    harness_with(urls, PacerPolicy::UNTHROTTLED)
}

pub(crate) fn harness_with(urls: &[&str], pacing: PacerPolicy) -> Result<Harness, NetError> {
    let clock = Arc::new(FakeClock::default());
    let runtime = NetRuntime::with_pacing(Arc::<FakeClock>::clone(&clock), pacing);
    let connector = Arc::new(FakeConnector::new(clock));
    let endpoints: Result<Vec<Endpoint>, NetError> = urls.iter().map(|url| endpoint(url)).collect();
    let pool = runtime.pool(
        ChainId::Ethereum,
        Transport::HttpJsonrpc,
        Arc::<FakeConnector>::clone(&connector),
        endpoints?,
    );
    let health = Arc::clone(runtime.health());
    Ok(Harness { runtime, health, connector, pool })
}
