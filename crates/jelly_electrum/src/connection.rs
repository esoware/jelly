use std::sync::atomic::{AtomicBool, Ordering};

use bitcoin::consensus::encode::serialize_hex;
use bitcoin::{BlockHash, ScriptBuf, Transaction, Txid, block};
use jelly_core::EndpointLabel;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::sync::Mutex as AsyncMutex;

use crate::chain::ElectrumChain;
use crate::connector::SocketSource;
use crate::error::ElectrumError;
use crate::fee::{EstimationMode, FeeReading, HistogramBucket, MempoolInfo, read_btc_per_kvb};
use crate::header::header_from_hex;
use crate::history::HistoryEntry;
use crate::method::Method;
use crate::reject::classify_rejection;
use crate::rpc_socket::RpcSocket;
use crate::script_hash::ElectrumScriptHash;
use crate::tx::{ServedTx, tx_from_bytes};
use crate::unspent::Unspent;
use crate::version::ProtocolVersion;

const TARGET: &str = "jelly::electrum::connection";

/// A name neither `ElectrumX` nor Fulcrum reads as an outdated Electrum wallet.
///
/// Both answer a landed broadcast from a client name they read as an Electrum
/// version below 3.3.4 with an upgrade warning instead of the txid. Fulcrum
/// reads the version after the first slash, so the name has no slash and leads
/// with a word neither server parses as a number.
const CLIENT_NAME: &str = "jelly";
/// Blockstream's electrs, behind a shipped Bitcoin endpoint, closes the socket
/// on a batch of more than 20.
const MAX_BATCH: usize = 20;
/// How `ElectrumX` refuses an answer past its `MAX_SEND`.
///
/// aiorpcx, which it runs on, counts a batch's answers toward that limit
/// together, so every element past the running total gets this error in place
/// of its result, however small each answer is.
const OVERSIZED_ANSWER: &str = "response too large";

/// One negotiated Electrum socket, what each of its answers means, and the
/// subscriptions registered on it.
///
/// Reconnection belongs to `ElectrumClient`.
pub(crate) struct Connection {
    socket: RpcSocket,
    version: ServerVersion,
    chain: ElectrumChain,
    is_proved: AtomicBool,
    registration: AsyncMutex<Option<Registration>>,
}

/// What one socket agreed with its server during the handshake.
struct ServerVersion {
    software: String,
    protocol: ProtocolVersion,
}

/// What [`Connection::watch`] registered on this socket, and how many pushes it
/// has already reported.
struct Registration {
    scripts: Vec<ScriptBuf>,
    seen: u64,
}

impl Connection {
    /// Opens a socket from `sockets` and negotiates `server.version` on it,
    /// which the protocol requires to be the first call and permits only once.
    pub(crate) async fn open(
        sockets: &dyn SocketSource,
        chain: ElectrumChain,
    ) -> Result<Self, ElectrumError> {
        let socket = RpcSocket::open(sockets.open().await?, sockets.endpoint().clone());
        let version = negotiate(&socket).await?;
        Ok(Self {
            socket,
            version,
            chain,
            is_proved: AtomicBool::new(false),
            registration: AsyncMutex::new(None),
        })
    }

    fn endpoint(&self) -> &EndpointLabel {
        self.socket.endpoint()
    }

    pub(crate) fn software(&self) -> &str {
        &self.version.software
    }

    pub(crate) const fn protocol(&self) -> ProtocolVersion {
        self.version.protocol
    }

    pub(crate) fn is_live(&self) -> bool {
        self.socket.is_live()
    }

    /// The tip's height, which also registers this socket for header
    /// notifications because the protocol has no plain tip read. Those pushes
    /// wake [`Connection::watch`].
    ///
    /// The tip's header is decoded only so a malformed answer is refused.
    pub(crate) async fn tip_height(&self) -> Result<u32, ElectrumError> {
        let wire: TipWire = self.call(Method::HeadersSubscribe, &[(); 0]).await?;
        self.header(Method::HeadersSubscribe, &wire.hex)?;
        Ok(wire.height)
    }

    /// A registration belongs to this socket, so a socket that replaced another
    /// registers afresh.
    ///
    /// The wait itself holds no lock, so concurrent callers each wake on the next
    /// push rather than queueing behind the first.
    pub(crate) async fn watch(&self, scripts: &[ScriptBuf]) -> Result<(), ElectrumError> {
        let seen = {
            let mut registration = self.registration.lock().await;
            let Some(registered) = registration.as_mut() else {
                self.tip_height().await?;
                self.register_scripts(scripts).await?;
                let seen = *self.socket.notices().borrow();
                *registration = Some(Registration { scripts: scripts.to_vec(), seen });
                return Ok(());
            };
            if registered.scripts != scripts {
                self.register_scripts(scripts).await?;
                registered.scripts = scripts.to_vec();
            }
            registered.seen
        };
        let mut notices = self.socket.notices();
        if notices.wait_for(|count| *count != seen).await.is_err() {
            return Err(ElectrumError::closed(self.endpoint()));
        }
        if let Some(registered) = self.registration.lock().await.as_mut() {
            registered.seen = *notices.borrow();
        }
        if self.is_live() { Ok(()) } else { Err(ElectrumError::closed(self.endpoint())) }
    }

    /// The statuses the server answers with are discarded, because the protocol's
    /// status hash is not this build's history fingerprint and a caller that just
    /// registered runs a pass anyway.
    ///
    /// A server's refusal is a degraded registration rather than a failure. The
    /// refusal is deterministic, so a retry would only hammer the server, and the
    /// header registration keeps the watch alive at block granularity.
    async fn register_scripts(&self, scripts: &[ScriptBuf]) -> Result<(), ElectrumError> {
        let statuses: Result<Vec<Option<String>>, ElectrumError> =
            self.call_many(Method::ScripthashSubscribe, &script_hashes(scripts)).await;
        match statuses {
            Ok(_) => Ok(()),
            Err(ElectrumError::Rpc { endpoint, code, message, .. }) => {
                tracing::debug!(
                    target: TARGET,
                    endpoint = %endpoint,
                    code,
                    message,
                    "a script registration was refused; watching headers only"
                );
                Ok(())
            }
            Err(other) => Err(other),
        }
    }

    /// Checks this socket's blocks against the chain's, once per socket.
    ///
    /// Blocks are hashed rather than trusted, because `server.features` is a claim
    /// and a header is a proof. The claim is still read, and a server whose own
    /// block zero contradicts it is refused, since that is what a proxy stitching two
    /// backends together looks like.
    pub(crate) async fn prove(&self) -> Result<(), ElectrumError> {
        if self.is_proved.load(Ordering::Acquire) {
            return Ok(());
        }
        let heights: Vec<u32> = self.chain.proofs().map(|proof| proof.height).collect();
        let (features, headers) = tokio::join!(
            self.call::<_, ServerFeatures>(Method::ServerFeatures, &[(); 0]),
            self.block_headers(&heights)
        );
        let advertised = match features {
            Ok(features) => features.genesis_hash,
            Err(error) => {
                tracing::debug!(
                    target: TARGET,
                    endpoint = %self.endpoint(),
                    %error,
                    "the server does not answer server.features"
                );
                None
            }
        };
        // One header per height, block zero first.
        let served_hashes: Vec<BlockHash> =
            headers?.iter().map(block::Header::block_hash).collect();
        if let Some(claim) = advertised
            && let Some(genesis) = served_hashes.first()
            && !claim.trim().eq_ignore_ascii_case(&genesis.to_string())
        {
            return Err(ElectrumError::protocol(
                self.endpoint(),
                Method::ServerFeatures,
                "the advertised genesis is not the one block zero hashes to",
            ));
        }
        for (proof, found) in self.chain.proofs().zip(served_hashes) {
            if found != proof.hash {
                return Err(ElectrumError::WrongChain {
                    endpoint: self.endpoint().clone(),
                    height: proof.height,
                    expected: Box::new(proof.hash),
                    found: Box::new(found),
                });
            }
        }
        self.is_proved.store(true, Ordering::Release);
        Ok(())
    }

    /// Headers for `heights`, in the order asked for.
    ///
    /// Consecutive heights must chain, so a server cannot answer a range with
    /// unrelated blocks.
    pub(crate) async fn block_headers(
        &self,
        heights: &[u32],
    ) -> Result<Vec<block::Header>, ElectrumError> {
        let params: Vec<(u32,)> = heights.iter().map(|height| (*height,)).collect();
        let hexes: Vec<String> = self.call_many(Method::BlockHeader, &params).await?;
        let headers = hexes
            .iter()
            .map(|text| self.header(Method::BlockHeader, text))
            .collect::<Result<Vec<_>, _>>()?;
        self.check_linkage(heights, &headers)?;
        Ok(headers)
    }

    pub(crate) async fn histories(
        &self,
        scripts: &[ScriptBuf],
    ) -> Result<Vec<Vec<HistoryEntry>>, ElectrumError> {
        self.call_many(Method::GetHistory, &script_hashes(scripts)).await
    }

    pub(crate) async fn unspents(
        &self,
        scripts: &[ScriptBuf],
    ) -> Result<Vec<Vec<Unspent>>, ElectrumError> {
        self.call_many(Method::ListUnspent, &script_hashes(scripts)).await
    }

    /// The transactions named by `txids`, in the order asked for. Each is hashed
    /// and checked against its txid, so a server cannot substitute another one.
    pub(crate) async fn txs(&self, txids: &[Txid]) -> Result<Vec<ServedTx>, ElectrumError> {
        let params: Vec<(Txid,)> = txids.iter().map(|txid| (*txid,)).collect();
        let hexes: Vec<String> = self.call_many(Method::GetTransaction, &params).await?;
        let mut txs = Vec::with_capacity(hexes.len());
        for (text, wanted) in hexes.iter().zip(txids) {
            let served = self.decode_tx(text)?;
            if served.tx.compute_txid() != *wanted {
                return Err(ElectrumError::protocol(
                    self.endpoint(),
                    Method::GetTransaction,
                    "the server answered with a different transaction",
                ));
            }
            txs.push(served);
        }
        Ok(txs)
    }

    /// A refusal is classified here into a [`jelly_core::Rejection`], so no other
    /// crate reads the node's message.
    pub(crate) async fn broadcast(&self, tx: &Transaction) -> Result<Txid, ElectrumError> {
        let answered: String = match self.call(Method::Broadcast, &(serialize_hex(tx),)).await {
            Ok(answered) => answered,
            Err(ElectrumError::Rpc { endpoint, message, .. }) => {
                return Err(ElectrumError::Rejected {
                    endpoint,
                    rejection: classify_rejection(&message),
                });
            }
            Err(other) => return Err(other),
        };
        let reported = answered.trim().parse::<Txid>().map_err(|_| {
            ElectrumError::protocol(
                self.endpoint(),
                Method::Broadcast,
                "the answer is not a transaction id",
            )
        })?;
        if reported == tx.compute_txid() {
            return Ok(reported);
        }
        Err(ElectrumError::protocol(
            self.endpoint(),
            Method::Broadcast,
            "the server answered with a different transaction id",
        ))
    }

    /// `blockchain.estimatefee` for confirmation within `target_blocks`.
    ///
    /// The mode only reaches the wire on protocol 1.6 and above, because older
    /// servers refuse a second argument outright and the whole quote would be
    /// lost. A target with no estimate comes back as [`FeeReading::Absent`], never
    /// as a rate.
    pub(crate) async fn estimate_fee(
        &self,
        target_blocks: u16,
        mode: EstimationMode,
    ) -> Result<FeeReading, ElectrumError> {
        let raw: Box<RawValue> = if self.protocol() >= ProtocolVersion::V1_6 {
            self.call(Method::EstimateFee, &(target_blocks, mode)).await?
        } else {
            self.call(Method::EstimateFee, &(target_blocks,)).await?
        };
        read_btc_per_kvb(raw.get()).ok_or_else(|| {
            ElectrumError::protocol(
                self.endpoint(),
                Method::EstimateFee,
                "the estimate is not a number this build can read",
            )
        })
    }

    pub(crate) async fn fee_histogram(&self) -> Result<Vec<HistogramBucket>, ElectrumError> {
        self.call(Method::FeeHistogram, &[(); 0]).await
    }

    /// The relay floors. A server that negotiated below 1.6 fails with a named
    /// error rather than an unknown-method one, since this build does not fall
    /// back to `blockchain.relayfee`.
    pub(crate) async fn mempool_info(&self) -> Result<MempoolInfo, ElectrumError> {
        if self.protocol() < ProtocolVersion::V1_6 {
            return Err(ElectrumError::MethodUnavailable {
                endpoint: self.endpoint().clone(),
                method: Method::MempoolInfo,
                since: ProtocolVersion::V1_6,
                negotiated: self.protocol(),
            });
        }
        self.call(Method::MempoolInfo, &[(); 0]).await
    }

    fn header(&self, method: Method, text: &str) -> Result<block::Header, ElectrumError> {
        header_from_hex(text, self.chain.is_merge_mined).ok_or_else(|| {
            ElectrumError::protocol(
                self.endpoint(),
                method,
                "a header is not eighty bytes of hex and the chain's own framing",
            )
        })
    }

    fn decode_tx(&self, text: &str) -> Result<ServedTx, ElectrumError> {
        let bytes = hex::decode(text.trim()).map_err(|_| {
            ElectrumError::protocol(
                self.endpoint(),
                Method::GetTransaction,
                "a transaction is not hex",
            )
        })?;
        tx_from_bytes(&bytes).ok_or_else(|| {
            ElectrumError::protocol(
                self.endpoint(),
                Method::GetTransaction,
                "a transaction does not decode",
            )
        })
    }

    fn check_linkage(
        &self,
        heights: &[u32],
        headers: &[block::Header],
    ) -> Result<(), ElectrumError> {
        for (adjacent_heights, adjacent_headers) in heights.windows(2).zip(headers.windows(2)) {
            if let ([lower, height], [parent, header]) = (adjacent_heights, adjacent_headers)
                && lower.checked_add(1) == Some(*height)
                && header.prev_blockhash != parent.block_hash()
            {
                return Err(ElectrumError::protocol(
                    self.endpoint(),
                    Method::BlockHeader,
                    "two consecutive headers do not chain",
                ));
            }
        }
        Ok(())
    }

    async fn call<P: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        params: &P,
    ) -> Result<T, ElectrumError> {
        let raw = self.socket.request(method, params).await?;
        decode(self.endpoint(), method, &raw)
    }

    /// Splits requests into sequential batches of at most [`MAX_BATCH`],
    /// preserving result order. Bounded batches avoid server resource limits,
    /// and a batch whose answers together pass the server's response limit is
    /// asked again in halves, so only an answer too large on its own fails.
    /// Empty input sends nothing.
    async fn call_many<P: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        params: &[P],
    ) -> Result<Vec<T>, ElectrumError> {
        let mut results = Vec::with_capacity(params.len());
        let mut batches: Vec<&[P]> = params.chunks(MAX_BATCH).rev().collect();
        while let Some(batch) = batches.pop() {
            match self.socket.batch(method, batch).await {
                Ok(raws) => {
                    for raw in raws {
                        results.push(decode(self.endpoint(), method, &raw)?);
                    }
                }
                Err(ElectrumError::Rpc { ref message, .. })
                    if batch.len() > 1 && message.starts_with(OVERSIZED_ANSWER) =>
                {
                    let (head, tail) = batch.split_at(batch.len().div_ceil(2));
                    batches.push(tail);
                    batches.push(head);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(results)
    }
}

/// `server.features`, of which only the genesis hash is read.
///
/// The genesis hash is a claim to cross-check, not a proof. It stays the raw
/// string, so a claim that is not even a hash contradicts block zero instead of
/// failing the call and going unchecked.
#[derive(Debug, Deserialize)]
struct ServerFeatures {
    genesis_hash: Option<String>,
}

/// `blockchain.headers.subscribe` as it arrives, before the header is framed.
#[derive(Debug, Deserialize)]
struct TipWire {
    height: u32,
    hex: String,
}

/// The range offered is this build's rather than a caller's, because what a
/// client can parse is a property of the code.
async fn negotiate(socket: &RpcSocket) -> Result<ServerVersion, ElectrumError> {
    let endpoint = socket.endpoint();
    let offered = [ProtocolVersion::MINIMUM.to_string(), ProtocolVersion::MAXIMUM.to_string()];
    let raw = socket.request(Method::ServerVersion, &(CLIENT_NAME, offered)).await?;
    let (software, reported): (String, String) = decode(endpoint, Method::ServerVersion, &raw)?;
    let Some(protocol) = ProtocolVersion::parse(&reported) else {
        return Err(ElectrumError::UnreadableVersion {
            endpoint: endpoint.clone(),
            found: reported,
        });
    };
    if protocol < ProtocolVersion::MINIMUM {
        return Err(ElectrumError::VersionTooOld {
            endpoint: endpoint.clone(),
            found: protocol,
            minimum: ProtocolVersion::MINIMUM,
        });
    }
    if protocol > ProtocolVersion::MAXIMUM {
        return Err(ElectrumError::VersionTooNew {
            endpoint: endpoint.clone(),
            found: protocol,
            maximum: ProtocolVersion::MAXIMUM,
        });
    }
    Ok(ServerVersion { software, protocol })
}

fn decode<T: DeserializeOwned>(
    endpoint: &EndpointLabel,
    method: Method,
    raw: &RawValue,
) -> Result<T, ElectrumError> {
    serde_json::from_str(raw.get()).map_err(|source| {
        // By position, because serde's message quotes the answer back.
        tracing::debug!(
            target: TARGET,
            endpoint = %endpoint,
            method = method.as_str(),
            line = source.line(),
            column = source.column(),
            "an answer does not parse"
        );
        ElectrumError::protocol(endpoint, method, "the result is not the shape that was asked for")
    })
}

fn script_hashes(scripts: &[ScriptBuf]) -> Vec<(ElectrumScriptHash,)> {
    scripts.iter().map(|script| (ElectrumScriptHash::of(script),)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use bitcoin::absolute::LockTime;
    use bitcoin::consensus::serialize;
    use bitcoin::hashes::Hash as _;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, OutPoint, TxIn, TxOut};
    use jelly_net::Socket;
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream, duplex};
    use tokio::task::JoinSet;

    use super::*;
    use crate::frame::{FrameReader, encode_line};

    /// `ElectrumX`'s default `MAX_SEND` for a coin that is not merge-mined, which it
    /// hands to aiorpcx as the largest response it will send.
    const ELECTRUMX_MAX_SEND: usize = 1_000_000;

    /// An `ElectrumX` server as aiorpcx answers a batch: it keeps a running total of
    /// the answers' sizes, and every element past `ELECTRUMX_MAX_SEND` gets an
    /// oversized-response error in place of its result.
    #[derive(Debug)]
    struct Electrumx {
        endpoint: EndpointLabel,
        txs: Arc<BTreeMap<String, String>>,
        servers: Mutex<JoinSet<()>>,
    }

    #[async_trait]
    impl SocketSource for Electrumx {
        fn endpoint(&self) -> &EndpointLabel {
            &self.endpoint
        }

        async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError> {
            let (client, server) = duplex(65_536);
            self.servers.lock().spawn(serve(server, Arc::clone(&self.txs)));
            Ok(Box::new(client))
        }
    }

    async fn serve(mut stream: DuplexStream, txs: Arc<BTreeMap<String, String>>) {
        let mut lines = FrameReader::new(usize::MAX);
        let mut chunk = vec![0u8; 4096];
        loop {
            let read_bytes = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read_bytes) => read_bytes,
            };
            lines.extend(&chunk[..read_bytes]);
            while let Some(line) = lines.take_line() {
                let request: Value = serde_json::from_slice(&line).unwrap();
                let answer = match request.as_array() {
                    Some(batch) => answer_batch(batch, &txs),
                    None => answer_one(&request, &txs),
                };
                stream.write_all(&encode_line(&answer)).await.unwrap();
            }
        }
    }

    fn answer_one(request: &Value, txs: &BTreeMap<String, String>) -> String {
        let result = match request.get("method").and_then(Value::as_str) {
            Some("server.version") => json!(["ElectrumX 1.18.0", "1.4.3"]),
            Some("blockchain.transaction.get") => request
                .pointer("/params/0")
                .and_then(Value::as_str)
                .and_then(|txid| txs.get(txid))
                .map_or(Value::Null, |hex_text| json!(hex_text)),
            _ => Value::Null,
        };
        json!({"jsonrpc": "2.0", "result": result, "id": request.get("id")}).to_string()
    }

    fn answer_batch(batch: &[Value], txs: &BTreeMap<String, String>) -> String {
        let mut size = 0usize;
        let parts: Vec<String> = batch
            .iter()
            .map(|request| {
                let part = answer_one(request, txs);
                size = size.saturating_add(part.len()).saturating_add(2);
                if size <= ELECTRUMX_MAX_SEND {
                    return part;
                }
                let message = format!("response too large (over {ELECTRUMX_MAX_SEND} bytes");
                json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": message},
                    "id": request.get("id"),
                })
                .to_string()
            })
            .collect();
        format!("[{}]", parts.join(", "))
    }

    /// About thirty kilobytes, the size of a large consolidation, and different for
    /// every `seed`.
    fn bulky_tx(seed: u8) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::all_zeros(), u32::from(seed)),
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: ScriptBuf::from_bytes(vec![seed; 30_000]),
            }],
        }
    }

    #[tokio::test]
    async fn a_batch_too_large_for_electrumx_as_a_whole_is_asked_again_in_halves() {
        let bulky: Vec<Transaction> = (0..20).map(bulky_tx).collect();
        let txids: Vec<Txid> = bulky.iter().map(Transaction::compute_txid).collect();
        let served: BTreeMap<String, String> = bulky
            .iter()
            .map(|tx| (tx.compute_txid().to_string(), hex::encode(serialize(tx))))
            .collect();
        let largest = served.values().map(String::len).max().unwrap();
        assert!(largest * 10 < ELECTRUMX_MAX_SEND, "each answer alone is far under the limit");

        let sockets = Electrumx {
            endpoint: EndpointLabel::new("ssl", "electrum.invalid", Some(50002)),
            txs: Arc::new(served),
            servers: Mutex::default(),
        };
        let chain = ElectrumChain {
            genesis: BlockHash::all_zeros(),
            checkpoint: None,
            is_merge_mined: false,
        };
        let connection = Connection::open(&sockets, chain).await.unwrap();

        let fetched = connection.txs(&txids).await;
        assert!(
            fetched.as_ref().is_ok_and(|served| served.len() == txids.len()),
            "expected all {} transactions, each answer about {largest} bytes, to be fetched from an \
             ElectrumX server whose limit is {ELECTRUMX_MAX_SEND} bytes per response, but the \
             fetch ended with {:?}",
            txids.len(),
            fetched.map(|served| served.len()),
        );
    }
}
