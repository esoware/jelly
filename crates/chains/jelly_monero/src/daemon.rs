//! One monerod endpoint over the shared HTTP client, and every call this crate
//! makes to it.
//!
//! `monero-daemon-rpc` reports a failure as an `InterfaceError`, which can't carry
//! a [`NetError`], so the transport stashes the real one for the caller to recover.

use std::fmt;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{Connect, Endpoint, HttpClient, HttpRequest, Identity, NetError};
use monero_daemon_rpc::{HttpTransport, MoneroDaemon};
use monero_interface::{
    EvaluateUnlocked, InterfaceError, ProvidesBlockchainMeta, ProvidesDecoys,
    ProvidesScannableBlocks, ProvidesTransactions, ScannableBlock, TransactionsError,
};
use monero_wallet::ed25519::Point;
use monero_wallet::transaction::Transaction;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::join;

use crate::broadcast::{BroadcastReply, SendRawAnswer};
use crate::error::MoneroError;
use crate::fee::{self, FeeLadder, MAX_PICONERO_PER_WEIGHT, MoneroPriority};
use crate::identity;

const TARGET: &str = "jelly::monero::daemon";

/// The `grace_blocks` wallet2 asks `get_fee_estimate` for.
const FEE_GRACE_BLOCKS: u32 = 10;

/// A response budget in bytes of data, which the library scales for JSON and
/// HTTP overhead.
const SMALL_ANSWER_BYTES: usize = 4096;

/// The budget for the recent block headers the automatic priority reads, each a
/// few hundred bytes.
const HEADERS_ANSWER_BYTES: usize = 16_384;

/// The multipliers wallet2 applies to the unimportant, normal, and elevated
/// priorities when a daemon is too old to quote a rate per priority, from
/// `get_fee_multiplier` in `src/wallet/wallet2.cpp`.
const LEGACY_MULTIPLIERS: [u64; 3] = [1, 5, 25];

/// A monerod HTTP transport over the shared `jelly_net` client.
///
/// The daemon takes epee binary bodies on the `.bin` routes and JSON everywhere
/// else, and can answer a block request with megabytes, so the library's size
/// limit is enforced as the body arrives.
#[derive(Debug, Clone)]
struct JellyTransport {
    client: HttpClient,
    endpoint: Endpoint,
    timeout: Duration,
    /// The last transport failure, shared by every clone, so the caller can recover
    /// its real class for the pool's health record.
    last_failure: Arc<Mutex<Option<NetError>>>,
}

impl JellyTransport {
    fn stash(&self, error: NetError) -> InterfaceError {
        let text = error.to_string();
        *self.last_failure.lock() = Some(error);
        InterfaceError::InterfaceError(text)
    }

    /// The transport's own classification when there is one. Otherwise the
    /// `InterfaceError` came from the protocol layer, which means the node
    /// answered badly.
    fn recover(&self, error: &InterfaceError) -> NetError {
        self.last_failure.lock().take().unwrap_or_else(|| {
            let label = self.endpoint.label();
            tracing::debug!(
                target: TARGET,
                endpoint = %label,
                %error,
                "the daemon library refused the answer"
            );
            NetError::protocol(label, "the daemon's answer cannot be used")
        })
    }
}

impl HttpTransport for JellyTransport {
    fn post(
        &self,
        route: &str,
        body: Vec<u8>,
        response_size_limit: Option<usize>,
    ) -> impl Send + Future<Output = Result<Vec<u8>, InterfaceError>> {
        async move {
            #[expect(
                clippy::case_sensitive_file_extension_comparisons,
                reason = "an rpc route, which monerod matches case-sensitively"
            )]
            let is_binary = route.ends_with(".bin");
            let content_type =
                if is_binary { "application/octet-stream" } else { "application/json" };
            let mut request = HttpRequest::post_bytes(content_type, &body, self.timeout);
            if let Some(limit) = response_size_limit {
                request = request.with_max_bytes(limit);
            }
            let response = self
                .client
                .send(&self.endpoint, route, request)
                .await
                .map_err(|error| self.stash(error))?;
            response.into_bytes().map_err(|error| self.stash(error))
        }
    }
}

/// A negotiated monerod endpoint, as the failover pool caches it. The URL can carry
/// credentials, so `Debug` shows the label.
pub(crate) struct DaemonEndpoint {
    daemon: MoneroDaemon<JellyTransport>,
    transport: JellyTransport,
}

impl fmt::Debug for DaemonEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonEndpoint").field("label", self.label()).finish_non_exhaustive()
    }
}

impl DaemonEndpoint {
    async fn open(
        client: HttpClient,
        endpoint: &Endpoint,
        timeout: Duration,
    ) -> Result<Self, NetError> {
        let transport = JellyTransport {
            client,
            endpoint: endpoint.clone(),
            timeout,
            last_failure: Arc::default(),
        };
        let daemon = MoneroDaemon::new(transport.clone())
            .await
            .map_err(|error| transport.recover(&error))?;
        Ok(Self { daemon, transport })
    }

    pub(crate) fn url(&self) -> &str {
        self.transport.endpoint.url()
    }

    const fn label(&self) -> &EndpointLabel {
        self.transport.endpoint.label()
    }

    pub(crate) async fn latest_block_number(&self) -> Result<usize, NetError> {
        ProvidesBlockchainMeta::latest_block_number(&self.daemon)
            .await
            .map_err(|error| self.transport.recover(&error))
    }

    pub(crate) async fn scannable_blocks(
        &self,
        range: RangeInclusive<usize>,
    ) -> Result<Vec<ScannableBlock>, NetError> {
        ProvidesScannableBlocks::contiguous_scannable_blocks(&self.daemon, range)
            .await
            .map_err(|error| self.transport.recover(&error))
    }

    pub(crate) async fn output_distribution(&self, height: usize) -> Result<Vec<u64>, NetError> {
        ProvidesDecoys::ringct_output_distribution(&self.daemon, ..=height)
            .await
            .map_err(|error| self.transport.recover(&error))
    }

    pub(crate) async fn ring_members(
        &self,
        indexes: &[u64],
    ) -> Result<Vec<Option<[Point; 2]>>, NetError> {
        ProvidesDecoys::unlocked_ringct_outputs(&self.daemon, indexes, EvaluateUnlocked::Normal)
            .await
            .map_err(|error| self.recover_transactions_error(error))
    }

    /// Asks for the pruned form, which answers the same question in a smaller
    /// response.
    pub(crate) async fn is_known(&self, txid: [u8; 32]) -> Result<bool, NetError> {
        match ProvidesTransactions::pruned_transactions(&self.daemon, &[txid]).await {
            Ok(found) => Ok(!found.is_empty()),
            Err(TransactionsError::TransactionNotFound) => Ok(false),
            Err(other) => Err(self.recover_transactions_error(other)),
        }
    }

    /// This node's fee tiers, none above [`MAX_PICONERO_PER_WEIGHT`], and the
    /// priority wallet2's default would pay, which the normal level pays.
    pub(crate) async fn fee_ladder(&self) -> Result<FeeLadder, NetError> {
        let params = json!({ "grace_blocks": FEE_GRACE_BLOCKS });
        let estimate: FeeEstimate =
            self.json_rpc("get_fee_estimate", &params, SMALL_ANSWER_BYTES).await?;
        let ladder = estimate.ladder(self.label())?;
        // wallet2 falls back to the slow priority when it can't judge congestion,
        // but a Monero fee can't be bumped later, so fall back to normal as Feather
        // does.
        let normal = match self.automatic_priority().await {
            Ok(priority) => priority,
            Err(error) => {
                tracing::debug!(
                    target: TARGET,
                    endpoint = %self.label(),
                    %error,
                    "no congestion reading; the normal level pays the normal priority"
                );
                MoneroPriority::Normal
            }
        };
        Ok(ladder.with_normal(normal))
    }

    /// wallet2's automatic priority, from the pool backlog and the last ten blocks'
    /// weights.
    ///
    /// The chain info and the pool statistics are read together, and only the header
    /// range waits for the height.
    async fn automatic_priority(&self) -> Result<MoneroPriority, NetError> {
        let no_params = json!({});
        let (info, pool) = join!(
            self.json_rpc::<InfoAnswer>("get_info", &no_params, SMALL_ANSWER_BYTES),
            self.rpc::<PoolStatsAnswer>(
                "get_transaction_pool_stats",
                &no_params,
                SMALL_ANSWER_BYTES
            ),
        );
        let info = info?;
        if info.status != "OK" {
            return Err(MoneroError::answer(self.label(), "the node reported no chain info").into());
        }
        let pool = pool?;
        if pool.status != "OK" {
            return Err(
                MoneroError::answer(self.label(), "the node reported no pool statistics").into()
            );
        }
        let recent = u64::from(fee::RECENT_BLOCKS);
        let Some(end) = info.height.checked_sub(1) else {
            return Ok(MoneroPriority::Normal);
        };
        let start = end.saturating_sub(recent.saturating_sub(1));
        let params = json!({ "start_height": start, "end_height": end });
        let answer: HeadersAnswer =
            self.json_rpc("get_block_headers_range", &params, HEADERS_ANSWER_BYTES).await?;
        if answer.status != "OK" {
            return Err(
                MoneroError::answer(self.label(), "the node reported no block headers").into()
            );
        }
        let weights: Vec<u64> = answer.headers.iter().map(|header| header.block_weight).collect();
        Ok(fee::automatic_priority(pool.pool_stats.bytes_total, &weights, info.block_weight_limit))
    }

    pub(crate) async fn send_raw_transaction(
        &self,
        transaction: &Transaction,
    ) -> Result<BroadcastReply, NetError> {
        // monerod's sanity check is an RPC-only heuristic on how a ring is spread, not
        // a relay rule. A sound ring can fail it, and monero-wallet's own client turns
        // it off too.
        let params = json!({
            "tx_as_hex": hex::encode(transaction.serialize()),
            "do_sanity_checks": false,
        });
        let answer: SendRawAnswer =
            self.rpc("send_raw_transaction", &params, SMALL_ANSWER_BYTES).await?;
        Ok(answer.read())
    }

    fn recover_transactions_error(&self, error: TransactionsError) -> NetError {
        match error {
            TransactionsError::InterfaceError(inner) => self.transport.recover(&inner),
            other @ (TransactionsError::TransactionNotFound
            | TransactionsError::PrunedTransaction) => {
                tracing::debug!(
                    target: TARGET,
                    endpoint = %self.label(),
                    error = %other,
                    "the daemon's transactions cannot be used"
                );
                NetError::protocol(self.label(), "the transactions answer cannot be used")
            }
        }
    }

    /// One bespoke JSON-RPC call, deserialized into a shape this crate owns.
    async fn json_rpc<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &Value,
        budget_bytes: usize,
    ) -> Result<T, NetError> {
        let body = self
            .daemon
            .json_rpc_call(method, Some(params.to_string()), budget_bytes)
            .await
            .map_err(|error| self.transport.recover(&error))?;
        serde_json::from_str(&body).map_err(|source| self.unparsed(method, &source))
    }

    /// One bespoke non-JSON-RPC call, deserialized into a shape this crate owns.
    async fn rpc<T: DeserializeOwned>(
        &self,
        route: &str,
        params: &Value,
        budget_bytes: usize,
    ) -> Result<T, NetError> {
        let body = self
            .daemon
            .rpc_call(route, Some(params.to_string()), budget_bytes)
            .await
            .map_err(|error| self.transport.recover(&error))?;
        serde_json::from_str(&body).map_err(|source| self.unparsed(route, &source))
    }

    /// Logged by position, because serde's message quotes the answer back.
    fn unparsed(&self, call: &str, source: &serde_json::Error) -> NetError {
        tracing::debug!(
            target: TARGET,
            endpoint = %self.label(),
            call,
            line = source.line(),
            column = source.column(),
            "a daemon answer does not parse"
        );
        NetError::protocol(self.label(), "the answer is not the shape that was asked for")
    }
}

/// Opens a [`DaemonEndpoint`] per endpoint and runs this crate's `get_info`
/// mainnet check once per endpoint per session.
#[derive(Debug, Clone)]
pub(crate) struct MoneroConnector {
    client: HttpClient,
    /// The budget for each request through an opened endpoint. The pool bounds the
    /// dial.
    request_timeout: Duration,
}

impl MoneroConnector {
    pub(crate) const fn new(client: HttpClient, request_timeout: Duration) -> Self {
        Self { client, request_timeout }
    }
}

#[async_trait]
impl Connect<DaemonEndpoint> for MoneroConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<DaemonEndpoint>, NetError> {
        DaemonEndpoint::open(self.client.clone(), endpoint, self.request_timeout)
            .await
            .map(Arc::new)
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        _connection: &DaemonEndpoint,
    ) -> Result<Identity, NetError> {
        identity::identify(&self.client, endpoint).await
    }
}

/// monerod's `get_fee_estimate`.
///
/// `fees` is the per-priority table modern daemons answer with, and `fee` is the
/// one base rate older ones answer with. `quantization_mask` defaults to one, since
/// a daemon that omits it quantizes by one and a zero mask isn't usable.
#[derive(Debug, Deserialize)]
struct FeeEstimate {
    status: String,
    fees: Option<Vec<u64>>,
    #[serde(default)]
    fee: u64,
    #[serde(default = "one")]
    quantization_mask: u64,
}

const fn one() -> u64 {
    1
}

impl FeeEstimate {
    /// The ladder this answer quotes, refused whole when any rate is above
    /// [`MAX_PICONERO_PER_WEIGHT`].
    fn ladder(&self, endpoint: &EndpointLabel) -> Result<FeeLadder, MoneroError> {
        if self.status != "OK" {
            return Err(MoneroError::answer(endpoint, "the node reported no fee estimate"));
        }
        // monerod answers a fourth tier, wallet2's priority tier, which no level pays.
        let per_weight = match self.fees.as_deref() {
            Some([unimportant, normal, elevated, ..]) => [*unimportant, *normal, *elevated],
            Some(_) => {
                return Err(MoneroError::answer(
                    endpoint,
                    "the node quoted fewer than three fee tiers",
                ));
            }
            // A daemon too old for per-priority rates. Scale its base rate the way
            // wallet2 does.
            None => {
                let [unimportant, normal, elevated] = LEGACY_MULTIPLIERS;
                let scale = |multiplier: u64| {
                    self.fee.checked_mul(multiplier).ok_or_else(|| {
                        MoneroError::answer(endpoint, "the quoted base fee overflows")
                    })
                };
                [scale(unimportant)?, scale(normal)?, scale(elevated)?]
            }
        };
        let ladder = FeeLadder::new(per_weight, self.quantization_mask)
            .ok_or_else(|| MoneroError::answer(endpoint, "the node quoted a zero fee"))?;
        for rate in ladder.rates() {
            if rate.per_weight() > MAX_PICONERO_PER_WEIGHT {
                return Err(MoneroError::FeeCeiling {
                    endpoint: endpoint.clone(),
                    piconero_per_weight: rate.per_weight(),
                    ceiling_piconero_per_weight: MAX_PICONERO_PER_WEIGHT,
                });
            }
        }
        Ok(ladder)
    }
}

/// What the automatic priority reads off `get_info`.
#[derive(Debug, Deserialize)]
struct InfoAnswer {
    status: String,
    height: u64,
    block_weight_limit: u64,
}

/// monerod's `get_transaction_pool_stats`, of which only the pool's weight is read.
#[derive(Debug, Deserialize)]
struct PoolStatsAnswer {
    status: String,
    pool_stats: PoolStats,
}

/// `bytes_total` mirrors monerod's name but sums transaction weights.
#[derive(Debug, Deserialize)]
struct PoolStats {
    bytes_total: u64,
}

/// monerod's `get_block_headers_range`, of which only the weights are read.
#[derive(Debug, Deserialize)]
struct HeadersAnswer {
    status: String,
    headers: Vec<HeaderWeight>,
}

#[derive(Debug, Deserialize)]
struct HeaderWeight {
    block_weight: u64,
}

#[cfg(test)]
mod tests {
    use monero_interface::FeeRate;

    use super::*;
    use crate::fixtures::label;

    fn estimate(json: &str) -> FeeEstimate {
        serde_json::from_str(json).expect("the fixture parses")
    }

    #[test]
    fn a_modern_daemon_quotes_every_tier_in_one_answer() {
        let answer = estimate(
            r#"{"status":"OK","fees":[20000,80000,320000,4000000],"fee":20000,
                "quantization_mask":10000}"#,
        );
        let ladder = answer.ladder(&label()).expect("a ladder");
        assert_eq!(ladder.rate(MoneroPriority::Unimportant).per_weight(), 20_000);
        assert_eq!(ladder.rate(MoneroPriority::Normal).per_weight(), 80_000);
        assert_eq!(ladder.rate(MoneroPriority::Elevated).per_weight(), 320_000);
        assert_eq!(
            ladder.rate(MoneroPriority::Unimportant),
            FeeRate::new(20_000, 10_000).expect("a rate"),
            "every rate carries the daemon's quantization mask"
        );
    }

    #[test]
    fn a_daemon_without_a_per_priority_table_is_scaled_the_way_wallet2_scales_it() {
        let answer = estimate(r#"{"status":"OK","fee":300,"quantization_mask":1}"#);
        let ladder = answer.ladder(&label()).expect("a ladder");
        let seen: Vec<u64> = ladder.rates().iter().map(FeeRate::per_weight).collect();
        assert_eq!(seen, vec![300, 1_500, 7_500]);
    }

    /// A restricted node's answer, trimmed to the fields monerod always sends.
    #[test]
    fn the_pool_weight_is_read_off_the_pool_statistics() {
        let answer: PoolStatsAnswer = serde_json::from_str(
            r#"{"status":"OK","untrusted":false,"pool_stats":{"bytes_total":412345,
                "bytes_min":1500,"bytes_max":40000,"bytes_med":2200,"fee_total":91000000,
                "oldest":1700000000,"txs_total":120,"num_failing":0,"num_10m":3,
                "num_not_relayed":0,"histo_98pc":0,"histo":[{"txs":3,"bytes":6000}],
                "num_double_spends":0}}"#,
        )
        .expect("the fixture parses");
        assert_eq!(answer.status, "OK");
        assert_eq!(answer.pool_stats.bytes_total, 412_345);
    }

    #[test]
    fn a_quote_above_the_ceiling_a_zero_rate_a_short_table_or_a_failed_status_is_refused() {
        let cases = [
            r#"{"status":"OK","fees":[1,2,999999999999,4],"fee":1,"quantization_mask":1}"#,
            r#"{"status":"OK","fees":[0,2,3,4],"fee":1,"quantization_mask":1}"#,
            r#"{"status":"OK","fees":[1,2],"fee":1,"quantization_mask":1}"#,
            r#"{"status":"Failed","fee":1,"quantization_mask":1}"#,
        ];
        let seen: Vec<MoneroError> = cases
            .into_iter()
            .map(|case| {
                estimate(case).ladder(&label()).expect_err("every one of these is unusable")
            })
            .collect();
        assert!(matches!(seen.first(), Some(MoneroError::FeeCeiling { .. })), "{seen:?}");
        for rest in seen.iter().skip(1) {
            assert!(matches!(rest, MoneroError::Answer { .. }), "{rest:?}");
        }
    }
}
