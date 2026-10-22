//! Which host each request reaches, through the shipped node and pool over loopback
//! stand-ins for the shipped endpoints. A stand-in for a full node answers the `/v1`
//! routes 404, as a real one does.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainId, Clock as _, EndpointHealth, EndpointOverride,
    FailureKind, SecretText, Transport,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore},
};
use jelly_net::{
    Connect, Endpoint, EndpointRegistry, HttpClient, HttpEndpoint, Identity, IdentityCheck as _,
    NetError, NetRuntime, PacerPolicy,
};
use jelly_tron::{PooledNode, TronAdapter, TronIdentity, test_util::OWNER};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::common::{Fallible, TRX, balances, fixture, watched};

const TRONGRID: &str = "https://api.trongrid.io";
const TOKENPOCKET: &str = "https://trx.mytokenpocket.vip";
const KEY: &str = "trongrid-secret";
const KEY_HEADER: &str = "TRON-PRO-API-KEY";
const IDENTITY_PATH: &str = "/wallet/getblockbynum";
const MAINNET_BLOCK_ZERO: &str =
    r#"{"blockID":"00000000000000001ebf88508a03865c71d452e25f4d51194196a1d22b6653dc"}"#;

/// One request a stand-in read.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    has_key: bool,
}

#[derive(Debug)]
struct Host {
    can_serve_history: bool,
    is_down: bool,
    log: Arc<Mutex<Vec<Seen>>>,
}

impl Host {
    fn reply(&self, path: &str) -> (&'static str, String) {
        if self.is_down {
            return ("500 Internal Server Error", String::from("{}"));
        }
        match path {
            IDENTITY_PATH => ("200 OK", MAINNET_BLOCK_ZERO.to_owned()),
            "/wallet/getblock" => ("200 OK", fixture!("now_block").to_owned()),
            "/wallet/getaccount" => (
                "200 OK",
                format!(r#"{{"address":"{OWNER}","balance":7000000,"create_time":1542274980000}}"#),
            ),
            "/wallet/triggerconstantcontract" => {
                ("200 OK", fixture!("constant_balance_of").to_owned())
            }
            history if history.starts_with("/v1/") && self.can_serve_history => {
                ("200 OK", fixture!("page_empty").to_owned())
            }
            _ => ("404 Not Found", String::from("{}")),
        }
    }
}

/// Answers one request per connection until the test drops it.
async fn serve(listener: TcpListener, host: Host) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let Some(seen) = read(&mut stream).await else {
            continue;
        };
        let (status, body) = host.reply(&seen.path);
        host.log.lock().push(seen);
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
    }
}

async fn read(stream: &mut TcpStream) -> Option<Seen> {
    let mut head: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.ok()?;
        head.extend_from_slice(&byte);
    }
    let head = String::from_utf8(head).ok()?;
    let headers: Vec<(&str, &str)> = head.lines().filter_map(|line| line.split_once(':')).collect();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).await.ok()?;
    let target = head.lines().next()?.split(' ').nth(1)?;
    let path = target.split_once('?').map_or(target, |(path, _)| path).to_owned();
    let has_key = headers.iter().any(|(name, _)| name.eq_ignore_ascii_case(KEY_HEADER));
    Some(Seen { path, has_key })
}

/// The shipped connector's identity check, with each configured endpoint connected to
/// its own stand-in. An endpoint with no stand-in is refused, so nothing a test does
/// leaves the machine.
#[derive(Debug)]
struct Redirect {
    client: HttpClient,
    local: HashMap<String, Endpoint>,
}

#[async_trait]
impl Connect<HttpEndpoint> for Redirect {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<HttpEndpoint>, NetError> {
        let local = self.local.get(endpoint.url()).ok_or_else(|| {
            NetError::endpoint(
                endpoint.label(),
                FailureKind::Connect,
                io::Error::other("no stand-in"),
            )
        })?;
        Ok(Arc::new(HttpEndpoint::new(self.client.clone(), local.clone())))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        TronIdentity.identify(endpoint, connection).await
    }
}

fn shipped_full_nodes() -> Fallible<Vec<String>> {
    let registry = EndpointRegistry::new(&[jelly_tron::ENTRY]);
    Ok(registry
        .defaults(ChainId::Tron)?
        .endpoints
        .iter()
        .map(|entry| entry.url.clone())
        .filter(|url| url != TRONGRID)
        .collect())
}

/// A loopback stand-in for every shipped endpoint, and the runtime the pool is built on.
struct Hosts {
    clock: Arc<FakeClock>,
    runtime: Arc<NetRuntime>,
    local: HashMap<String, Endpoint>,
    seen: BTreeMap<String, Arc<Mutex<Vec<Seen>>>>,
    _servers: JoinSet<()>,
}

impl Hosts {
    /// Every host in `down` answers 500 to everything.
    async fn start(down: &[String]) -> Fallible<Self> {
        let clock = Arc::new(FakeClock::default());
        let runtime = NetRuntime::with_chains(
            Arc::<FakeClock>::clone(&clock),
            PacerPolicy::UNTHROTTLED,
            &[jelly_tron::ENTRY],
        );
        let mut servers = JoinSet::new();
        let mut local = HashMap::new();
        let mut seen = BTreeMap::new();
        for entry in &runtime.registry().defaults(ChainId::Tron)?.endpoints {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = format!("http://{}", listener.local_addr()?);
            let log = Arc::new(Mutex::new(Vec::new()));
            let host = Host {
                can_serve_history: entry.url == TRONGRID,
                is_down: down.contains(&entry.url),
                log: Arc::clone(&log),
            };
            servers.spawn(serve(listener, host));
            local.insert(
                entry.url.clone(),
                Endpoint::parse(ChainId::Tron, Transport::TronHttp, &address)?,
            );
            seen.insert(entry.url.clone(), log);
        }
        Ok(Self { clock, runtime, local, seen, _servers: servers })
    }

    /// The endpoints the registry hands the TRON pool once `rows` are the stored
    /// overrides.
    fn configured(&self, rows: Vec<EndpointOverride>) -> Fallible<Vec<Endpoint>> {
        let registry = self.runtime.registry();
        registry.load_overrides(rows.into_iter().map(|row| (ChainId::Tron, row)));
        Ok(registry.enabled(ChainId::Tron, Transport::TronHttp)?)
    }

    /// Every host has answered as often and as fast as every other.
    fn equally_healthy(&self) {
        for url in self.seen.keys() {
            self.runtime.health().restore(ChainId::Tron, url, proven());
        }
    }

    /// An adapter watching [`OWNER`] over a pool of `endpoints`, in that order.
    async fn adapter(&self, endpoints: Vec<Endpoint>, key: Option<&str>) -> Fallible<TronAdapter> {
        let connector = Redirect { client: HttpClient::new(None)?, local: self.local.clone() };
        let pool =
            self.runtime.pool(ChainId::Tron, Transport::TronHttp, Arc::new(connector), endpoints);
        let node = PooledNode::new(Arc::new(pool), key.map(|key| SecretText::new(key.to_owned())))?;
        let adapter = TronAdapter::new(
            Arc::new(node),
            Arc::new(MemoryCursorStore::new()),
            Arc::<FakeClock>::clone(&self.clock),
        );
        adapter.set_watch_set(&[watched(1, OWNER)]).await?;
        Ok(adapter)
    }

    /// One pass, which must succeed, and the balances it reported.
    async fn pass(&self, adapter: &TronAdapter) -> Fallible<Vec<(i64, AssetId, Amount)>> {
        let sink = FakeEventSink::new();
        let deadline = self.clock.deadline_in(Duration::from_secs(30));
        timeout(Duration::from_secs(60), adapter.sync(deadline, &sink)).await??;
        Ok(balances(&sink.drain()))
    }

    fn requests(&self, url: &str) -> Vec<Seen> {
        self.seen.get(url).map(|log| log.lock().clone()).unwrap_or_default()
    }

    fn paths(&self, url: &str) -> Vec<String> {
        self.requests(url).into_iter().map(|seen| seen.path).collect()
    }

    /// Every path the full nodes were asked for, together.
    fn full_node_paths(&self) -> Fallible<Vec<String>> {
        Ok(shipped_full_nodes()?.iter().flat_map(|url| self.paths(url)).collect())
    }
}

/// What an earlier session leaves behind for a host that answered it well.
fn proven() -> EndpointHealth {
    EndpointHealth {
        score_permille: 1000,
        successes: 50,
        latency_ms: Some(40),
        ..EndpointHealth::default()
    }
}

/// The `/wallet` reads among `paths`, leaving out the identity check every host is
/// asked before its first request.
fn wallet_reads(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| path.starts_with("/wallet/") && path.as_str() != IDENTITY_PATH)
        .cloned()
        .collect()
}

fn history_reads(paths: &[String]) -> Vec<String> {
    paths.iter().filter(|path| path.starts_with("/v1/")).cloned().collect()
}

fn has_reported_trx(balances: &[(i64, AssetId, Amount)]) -> bool {
    balances.contains(&(1, TRX, Amount::from_base_units(7_000_000)))
}

/// `TronGrid` is listed first here, so only its standing can put it behind the full
/// nodes.
#[tokio::test]
async fn wallet_reads_go_to_a_full_node_ahead_of_an_equally_healthy_trongrid() {
    let hosts = Hosts::start(&[]).await.unwrap();
    hosts.equally_healthy();
    let mut endpoints = hosts.configured(Vec::new()).unwrap();
    endpoints.sort_by_key(|endpoint| endpoint.url() != TRONGRID);
    assert_eq!(endpoints.first().map(Endpoint::url), Some(TRONGRID));
    let adapter = hosts.adapter(endpoints, None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    assert_eq!(wallet_reads(&hosts.paths(TRONGRID)), Vec::<String>::new());
    let served = wallet_reads(&hosts.full_node_paths().unwrap());
    for route in ["/wallet/getblock", "/wallet/getaccount", "/wallet/triggerconstantcontract"] {
        assert!(served.iter().any(|path| path == route), "{route}: {served:?}");
    }
}

/// An existing user's `TronGrid` has a long record of answers and the full nodes have
/// none, which must not keep the balance reads on `TronGrid`.
#[tokio::test]
async fn history_goes_to_trongrid_and_wallet_reads_to_full_nodes_nobody_has_tried() {
    let hosts = Hosts::start(&[]).await.unwrap();
    hosts.runtime.health().restore(ChainId::Tron, TRONGRID, proven());
    let adapter = hosts.adapter(hosts.configured(Vec::new()).unwrap(), None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    let trongrid = hosts.paths(TRONGRID);
    assert_eq!(wallet_reads(&trongrid), Vec::<String>::new());
    assert_eq!(
        history_reads(&trongrid),
        [
            format!("/v1/accounts/{OWNER}/transactions"),
            format!("/v1/accounts/{OWNER}/transactions/trc20"),
            format!("/v1/accounts/{OWNER}/internal-transactions"),
        ]
    );
    let full_nodes = hosts.full_node_paths().unwrap();
    assert_eq!(history_reads(&full_nodes), Vec::<String>::new());
    assert!(!wallet_reads(&full_nodes).is_empty());
}

#[tokio::test]
async fn wallet_reads_fall_back_to_trongrid_when_every_full_node_fails() {
    let hosts = Hosts::start(&shipped_full_nodes().unwrap()).await.unwrap();
    let adapter = hosts.adapter(hosts.configured(Vec::new()).unwrap(), None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    for url in shipped_full_nodes().unwrap() {
        let asked = hosts.paths(&url);
        assert!(!asked.is_empty(), "{url} was never tried");
        assert!(asked.iter().all(|path| path == IDENTITY_PATH), "{url}: {asked:?}");
    }
    let served = wallet_reads(&hosts.paths(TRONGRID));
    for route in ["/wallet/getblock", "/wallet/getaccount", "/wallet/triggerconstantcontract"] {
        assert!(served.iter().any(|path| path == route), "{route}: {served:?}");
    }
}

#[tokio::test]
async fn the_trongrid_key_never_reaches_a_full_node() {
    let hosts = Hosts::start(&[]).await.unwrap();
    let adapter = hosts.adapter(hosts.configured(Vec::new()).unwrap(), Some(KEY)).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    assert!(!wallet_reads(&hosts.full_node_paths().unwrap()).is_empty());
    for url in shipped_full_nodes().unwrap() {
        for seen in hosts.requests(&url) {
            assert!(!seen.has_key, "{url} was sent the key with {}", seen.path);
        }
    }
}

/// A user who pinned `TronGrid` chose it for every request.
#[tokio::test]
async fn a_pinned_trongrid_keeps_the_wallet_reads() {
    let hosts = Hosts::start(&[]).await.unwrap();
    let pin = EndpointOverride { is_pinned: true, ..EndpointOverride::new(TRONGRID) };
    let adapter = hosts.adapter(hosts.configured(vec![pin]).unwrap(), None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    assert!(!wallet_reads(&hosts.paths(TRONGRID)).is_empty());
    assert_eq!(hosts.full_node_paths().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn a_pinned_full_node_takes_the_wallet_reads_and_never_history() {
    let hosts = Hosts::start(&[]).await.unwrap();
    let pin = EndpointOverride { is_pinned: true, ..EndpointOverride::new(TOKENPOCKET) };
    let adapter = hosts.adapter(hosts.configured(vec![pin]).unwrap(), None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    let pinned = hosts.paths(TOKENPOCKET);
    assert!(!wallet_reads(&pinned).is_empty());
    assert_eq!(history_reads(&pinned), Vec::<String>::new());
    assert_eq!(history_reads(&hosts.paths(TRONGRID)).len(), 3);
}

/// Balances still come from the full nodes, and no host is asked for history it
/// cannot serve.
#[tokio::test]
async fn with_trongrid_turned_off_no_host_is_asked_for_history() {
    let hosts = Hosts::start(&[]).await.unwrap();
    let off = EndpointOverride { is_enabled: false, ..EndpointOverride::new(TRONGRID) };
    let adapter = hosts.adapter(hosts.configured(vec![off]).unwrap(), None).await.unwrap();

    assert!(has_reported_trx(&hosts.pass(&adapter).await.unwrap()));
    assert_eq!(hosts.paths(TRONGRID), Vec::<String>::new());
    let full_nodes = hosts.full_node_paths().unwrap();
    assert!(!wallet_reads(&full_nodes).is_empty());
    assert_eq!(history_reads(&full_nodes), Vec::<String>::new());
}
