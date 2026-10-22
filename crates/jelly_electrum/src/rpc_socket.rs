use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use jelly_core::EndpointLabel;
use jelly_net::{JsonRpcBatch, JsonRpcRequest, Socket};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, split};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::error::ElectrumError;
use crate::frame::{Frame, FrameReader, Message, encode_line, parse_frame};
use crate::method::Method;

/// Long enough for a batch of transactions off a loaded server, short enough
/// that a stalled socket is replaced well inside one sync pass.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const READ_CHUNK_BYTES: usize = 65_536;
/// How many request lines may be queued for the writer before a caller waits.
const QUEUE_DEPTH: usize = 32;
/// A batch of transactions is the largest legitimate line a server sends. Past
/// this the line is an attack rather than an answer.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// One socket carrying JSON-RPC lines, with a bounded request queue and
/// per-request replies.
///
/// A timeout retires the socket, marking it dead and failing every waiter, so a
/// half-answered connection is never reused. The stream and both tasks are torn
/// down when the last handle drops. Negotiation and the meaning of each answer
/// belong to `Connection`.
pub(crate) struct RpcSocket {
    inner: Arc<Inner>,
    outgoing: mpsc::Sender<Vec<u8>>,
    next_id: AtomicU64,
    tasks: [JoinHandle<()>; 2],
}

/// Both tasks are aborted here, so no reader or writer outlives the socket that
/// owns it.
impl Drop for RpcSocket {
    fn drop(&mut self) {
        self.inner.retire(RetireReason::Gone);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl RpcSocket {
    /// Spawns the reader and the writer over `socket`.
    pub(crate) fn open(socket: Box<dyn Socket>, endpoint: EndpointLabel) -> Self {
        let (outgoing, queue) = mpsc::channel(QUEUE_DEPTH);
        let (notices, _) = watch::channel(0u64);
        let inner = Arc::new(Inner {
            endpoint,
            is_live: AtomicBool::new(true),
            pending: Mutex::default(),
            notices,
        });
        let (reader, writer) = split(socket);
        let tasks = [
            tokio::spawn(read_loop(reader, Arc::clone(&inner))),
            tokio::spawn(write_loop(writer, queue, Arc::clone(&inner))),
        ];
        Self { inner, outgoing, next_id: AtomicU64::new(1), tasks }
    }

    pub(crate) fn endpoint(&self) -> &EndpointLabel {
        &self.inner.endpoint
    }

    pub(crate) fn is_live(&self) -> bool {
        self.inner.is_live.load(Ordering::Acquire)
    }

    /// Bumped once per server push on this socket, and once when the socket is
    /// retired so a waiter wakes and finds [`RpcSocket::is_live`] false.
    pub(crate) fn notices(&self) -> watch::Receiver<u64> {
        self.inner.notices.subscribe()
    }

    /// The unparsed `result` of one call.
    pub(crate) async fn request<P: Serialize + Sync>(
        &self,
        method: Method,
        params: &P,
    ) -> Result<Box<RawValue>, ElectrumError> {
        let id = self.next_id();
        let request = JsonRpcRequest::new(id, method.as_str(), params)?;
        let raws = self.dispatch(method, id, vec![id], encode_line(request.as_str())).await?;
        raws.into_iter().next().ok_or_else(|| {
            ElectrumError::protocol(self.endpoint(), method, "the answer has no result")
        })
    }

    /// The unparsed `result` of each call in one JSON-RPC batch, in the order
    /// asked for. Empty input sends nothing.
    pub(crate) async fn batch<P: Serialize + Sync>(
        &self,
        method: Method,
        params: &[P],
    ) -> Result<Vec<Box<RawValue>>, ElectrumError> {
        let mut ids = Vec::with_capacity(params.len());
        let mut requests = Vec::with_capacity(params.len());
        for entry in params {
            let id = self.next_id();
            requests.push(JsonRpcRequest::new(id, method.as_str(), entry)?);
            ids.push(id);
        }
        let Some(group_id) = ids.first().copied() else { return Ok(Vec::new()) };
        let body = JsonRpcBatch::new(&requests);
        self.dispatch(method, group_id, ids, encode_line(body.as_str())).await
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Registers the answers before the line goes out, so a reply cannot arrive
    /// before there is somewhere to put it.
    async fn dispatch(
        &self,
        method: Method,
        group_id: u64,
        ids: Vec<u64>,
        line: Vec<u8>,
    ) -> Result<Vec<Box<RawValue>>, ElectrumError> {
        let wait = self.inner.register(method, group_id, ids)?;
        if self.outgoing.send(line).await.is_err() {
            self.inner.abandon(group_id);
            return Err(ElectrumError::closed(self.endpoint()));
        }
        match timeout(REQUEST_TIMEOUT, wait).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(ElectrumError::closed(self.endpoint())),
            Err(_) => {
                self.inner.abandon(group_id);
                self.inner.retire(RetireReason::Gone);
                Err(ElectrumError::timed_out(self.endpoint(), method, REQUEST_TIMEOUT))
            }
        }
    }
}

/// Why the socket stopped serving requests.
#[derive(Debug, Clone, Copy)]
enum RetireReason {
    /// The peer went away, a write failed, or this side gave up. Nothing in
    /// flight was answered in any of the three, so they are one outcome.
    Gone,
    /// The server sent a line that is not JSON-RPC.
    Malformed,
    /// The server sent an unterminated line past the frame limit.
    Oversize,
}

impl RetireReason {
    /// What a request on `method` that was still in flight fails with.
    fn error(self, endpoint: &EndpointLabel, method: Method) -> ElectrumError {
        match self {
            Self::Gone => ElectrumError::closed(endpoint),
            Self::Malformed => ElectrumError::protocol(
                endpoint,
                method,
                "the server sent a line that is not json-rpc",
            ),
            Self::Oversize => ElectrumError::protocol(
                endpoint,
                method,
                "the server sent a line past the frame limit",
            ),
        }
    }
}

type Answer = Result<Vec<Box<RawValue>>, ElectrumError>;

struct Inner {
    endpoint: EndpointLabel,
    is_live: AtomicBool,
    pending: Mutex<Pending>,
    /// Counts server pushes and the socket's retirement. A push is only a hint
    /// to ask again, never data to act on.
    notices: watch::Sender<u64>,
}

impl Inner {
    fn register(
        &self,
        method: Method,
        group_id: u64,
        ids: Vec<u64>,
    ) -> Result<oneshot::Receiver<Answer>, ElectrumError> {
        let mut pending = self.pending.lock();
        // Read under the lock, so a group is either refused here or drained by
        // `retire`, which clears the flag before it takes the lock.
        if !self.is_live.load(Ordering::Acquire) {
            return Err(ElectrumError::closed(&self.endpoint));
        }
        let (reply, wait) = oneshot::channel();
        for (slot, id) in ids.iter().enumerate() {
            pending.routes.insert(*id, Route { group_id, slot });
        }
        let remaining = ids.len();
        let slots = vec![None; remaining];
        pending.groups.insert(group_id, Group { method, ids, slots, remaining, reply });
        Ok(wait)
    }

    fn abandon(&self, group_id: u64) {
        let _ = self.pending.lock().take(group_id);
    }

    /// Marks the socket dead and fails every waiter. The stream itself stays open
    /// until the last handle drops.
    fn retire(&self, reason: RetireReason) {
        if !self.is_live.swap(false, Ordering::AcqRel) {
            return;
        }
        let mut pending = self.pending.lock();
        pending.routes.clear();
        while let Some((_, group)) = pending.groups.pop_first() {
            let _ = group.reply.send(Err(reason.error(&self.endpoint, group.method)));
        }
        drop(pending);
        // Wake every notification waiter so it observes the dead socket instead
        // of waiting on a channel nothing will ever bump again.
        self.notices.send_modify(|count| *count = count.wrapping_add(1));
    }

    fn deliver(&self, line: &[u8]) -> Result<(), RetireReason> {
        match parse_frame(line) {
            Err(_) => Err(RetireReason::Malformed),
            Ok(Frame::Single(message)) => self.deliver_single(message),
            Ok(Frame::Batch(messages)) => self.deliver_batch(messages),
        }
    }

    fn deliver_single(&self, message: Message) -> Result<(), RetireReason> {
        // A line with no id is a server push. Its content is not read, because
        // the pass it triggers re-fetches everything it acts on, so delivery is
        // one bump of the counter a watcher waits on.
        let Some(id) = message.id else {
            self.notices.send_modify(|count| *count = count.wrapping_add(1));
            return Ok(());
        };
        let mut pending = self.pending.lock();
        // An id nobody waits on is an answer that arrived after its caller gave
        // up, so it is dropped.
        let Some(route) = pending.routes.get(&id).copied() else { return Ok(()) };
        pending.apply(&self.endpoint, route, message)
    }

    fn deliver_batch(&self, messages: Vec<Message>) -> Result<(), RetireReason> {
        let Some(first) = messages.first().and_then(|message| message.id) else {
            return Err(RetireReason::Malformed);
        };
        let mut pending = self.pending.lock();
        let Some(route) = pending.routes.get(&first).copied() else {
            return Err(RetireReason::Malformed);
        };
        let Some((method, requested)) =
            pending.groups.get(&route.group_id).map(|group| (group.method, group.ids.len()))
        else {
            return Ok(());
        };
        // A short batch would leave slots unfilled until the request timed out
        // and retired the socket, so a length mismatch fails the group at once
        // with a typed error.
        if messages.len() != requested {
            let failure = ElectrumError::BatchLength {
                endpoint: self.endpoint.clone(),
                method,
                requested,
                received: messages.len(),
            };
            pending.finish(route.group_id, Err(failure));
            return Ok(());
        }
        for message in messages {
            let Some(id) = message.id else { return Err(RetireReason::Malformed) };
            let Some(element) = pending.routes.get(&id).copied() else {
                return Err(RetireReason::Malformed);
            };
            if element.group_id != route.group_id {
                return Err(RetireReason::Malformed);
            }
            pending.apply(&self.endpoint, element, message)?;
            if !pending.groups.contains_key(&route.group_id) {
                return Ok(());
            }
        }
        Err(RetireReason::Malformed)
    }
}

#[derive(Default)]
struct Pending {
    groups: BTreeMap<u64, Group>,
    routes: HashMap<u64, Route>,
}

impl Pending {
    fn apply(
        &mut self,
        endpoint: &EndpointLabel,
        route: Route,
        message: Message,
    ) -> Result<(), RetireReason> {
        if let Some(error) = message.error {
            let Some(method) = self.groups.get(&route.group_id).map(|group| group.method) else {
                return Ok(());
            };
            let failure = ElectrumError::Rpc {
                endpoint: endpoint.clone(),
                method,
                code: error.code(),
                message: error.message().to_owned(),
            };
            self.finish(route.group_id, Err(failure));
            return Ok(());
        }
        let Some(result) = message.result else { return Err(RetireReason::Malformed) };
        let Some(group) = self.groups.get_mut(&route.group_id) else { return Ok(()) };
        if group.fill(route.slot, result)
            && let Some(group) = self.take(route.group_id)
        {
            let values: Vec<Box<RawValue>> = group.slots.into_iter().flatten().collect();
            let _ = group.reply.send(Ok(values));
        }
        Ok(())
    }

    fn finish(&mut self, group_id: u64, outcome: Answer) {
        if let Some(group) = self.take(group_id) {
            let _ = group.reply.send(outcome);
        }
    }

    fn take(&mut self, group_id: u64) -> Option<Group> {
        let group = self.groups.remove(&group_id)?;
        for id in &group.ids {
            self.routes.remove(id);
        }
        Some(group)
    }
}

/// The requests that went out in one line, answered together.
struct Group {
    method: Method,
    ids: Vec<u64>,
    slots: Vec<Option<Box<RawValue>>>,
    remaining: usize,
    reply: oneshot::Sender<Answer>,
}

impl Group {
    /// True once every slot holds an answer.
    fn fill(&mut self, slot: usize, value: Box<RawValue>) -> bool {
        if let Some(entry) = self.slots.get_mut(slot)
            && entry.is_none()
        {
            *entry = Some(value);
            self.remaining = self.remaining.saturating_sub(1);
        }
        self.remaining == 0
    }
}

/// Where the answer to one request id goes.
#[derive(Debug, Clone, Copy)]
struct Route {
    group_id: u64,
    slot: usize,
}

async fn read_loop<R: AsyncRead + Unpin>(mut reader: R, inner: Arc<Inner>) {
    let reason = read_frames(&mut reader, &inner).await;
    inner.retire(reason);
}

async fn read_frames<R: AsyncRead + Unpin>(reader: &mut R, inner: &Inner) -> RetireReason {
    let mut frames = FrameReader::new(MAX_FRAME_BYTES);
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let read_bytes = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return RetireReason::Gone,
            Ok(read_bytes) => read_bytes,
        };
        let Some(slice) = chunk.get(..read_bytes) else { return RetireReason::Gone };
        frames.extend(slice);
        while let Some(line) = frames.take_line() {
            if let Err(reason) = inner.deliver(&line) {
                return reason;
            }
        }
        if frames.is_oversized() {
            return RetireReason::Oversize;
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut queue: mpsc::Receiver<Vec<u8>>,
    inner: Arc<Inner>,
) {
    while let Some(line) = queue.recv().await {
        if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
    inner.retire(RetireReason::Gone);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_batch_with_duplicate_or_unknown_ids_fails_without_waiting() {
        for line in [
            r#"[{"id":1,"result":[]},{"id":1,"result":[]}]"#,
            r#"[{"id":1,"result":[]},{"id":3,"result":[]}]"#,
            r#"[{"id":3,"result":[]},{"id":2,"result":[]}]"#,
            r#"[{"id":3,"result":[]},{"id":4,"result":[]}]"#,
        ] {
            let (notices, _) = watch::channel(0);
            let inner = Inner {
                endpoint: EndpointLabel::new("ssl", "electrum.invalid", Some(50002)),
                is_live: AtomicBool::new(true),
                pending: Mutex::default(),
                notices,
            };
            let mut wait = inner.register(Method::ListUnspent, 1, vec![1, 2]).unwrap();

            let reason = inner.deliver(line.as_bytes()).expect_err("the batch is malformed");
            inner.retire(reason);

            assert!(matches!(
                wait.try_recv(),
                Ok(Err(ElectrumError::Protocol { method: Method::ListUnspent, .. }))
            ));
        }
    }
}
