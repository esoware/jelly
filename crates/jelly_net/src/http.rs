use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{EndpointLabel, FailureKind, ProxySetting};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::redirect::Policy as RedirectPolicy;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use crate::connect::{Connect, Identity, IdentityCheck};
use crate::endpoint::Endpoint;
use crate::error::NetError;
use crate::jsonrpc::{JsonRpcBatch, JsonRpcRequest, json_rpc_decode, json_rpc_decode_batch};
use crate::proxy;
use crate::retry_after::RetryAfter;
use crate::tls;

/// reqwest dials the HTTP transports itself, so this is their only dial bound, and the
/// pool's connect budget for the transports it dials is the same value.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str = "jelly";

/// The cap on a response body when the request sets none.
///
/// reqwest decodes gzip, so without one a few compressed megabytes could unpack into
/// gigabytes. A request that expects more, such as a Monero block batch, sets its own.
const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

/// This is the workspace's only HTTP client.
///
/// Cloning shares the connection pool, HTTP/2 sessions, and TLS session cache, so the
/// process needs only one per proxy setting.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

/// reqwest's own `Debug` names the proxy host, which this crate keeps out of logs.
impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient").finish_non_exhaustive()
    }
}

impl HttpClient {
    /// There is no default client to fall back to, so a failure is returned to the
    /// caller.
    #[expect(
        clippy::disallowed_methods,
        reason = "this is the one place the workspace builds a reqwest client"
    )]
    pub fn new(proxy: Option<&ProxySetting>) -> Result<Self, NetError> {
        let config = tls::ca_validated_client_config()?;
        // The plaintext policy and the pools judged the configured address alone, so a
        // redirect may move along that origin but never to another scheme, host, or port.
        let hop_limit = RedirectPolicy::default();
        let redirect = RedirectPolicy::custom(move |attempt| {
            let is_same_origin = attempt
                .previous()
                .first()
                .is_some_and(|first| first.origin() == attempt.url().origin());
            if is_same_origin { hop_limit.redirect(attempt) } else { attempt.stop() }
        });
        let builder = reqwest::Client::builder()
            .tls_backend_preconfigured((*config).clone())
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(redirect);
        let inner = proxy::route_client(builder, proxy)?
            .build()
            .map_err(|source| NetError::config("the http client could not be built", source))?;
        Ok(Self { inner })
    }

    pub async fn send(
        &self,
        endpoint: &Endpoint,
        path: &str,
        request: HttpRequest<'_>,
    ) -> Result<HttpResponse, NetError> {
        let url = endpoint.join(path, "");
        self.send_to(endpoint.label(), &url, request).await
    }

    /// The same exchange against a whole address.
    ///
    /// For the price feed, which has no [`Endpoint`] to join a path onto, and for
    /// [`HttpEndpoint::send_query`], which has already merged its query.
    pub async fn send_to(
        &self,
        label: &EndpointLabel,
        url: &str,
        request: HttpRequest<'_>,
    ) -> Result<HttpResponse, NetError> {
        let mut builder = match request.method {
            HttpMethod::Get => self.inner.get(url),
            HttpMethod::Post => self.inner.post(url),
        };
        if let Some(HttpBody { content_type, bytes }) = request.body {
            builder = builder.header(CONTENT_TYPE, content_type).body(bytes.to_vec());
        }
        for (name, value) in request.headers {
            builder = builder.header(*name, *value);
        }
        let response = builder
            .timeout(request.timeout)
            .send()
            .await
            .map_err(|source| transport_error(label, request.timeout, source))?;

        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(RetryAfter::parse);
        let body = read_body(label, response, request.timeout, request.max_bytes).await?;
        Ok(HttpResponse { label: label.clone(), status, body, retry_after })
    }
}

/// Reads the whole body, refusing to grow past `max_bytes`.
///
/// The cap is checked as chunks arrive, so a hostile or broken endpoint cannot
/// make the process allocate more than the caller allowed.
async fn read_body(
    label: &EndpointLabel,
    mut response: reqwest::Response,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Vec<u8>, NetError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) =
        response.chunk().await.map_err(|source| transport_error(label, timeout, source))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(NetError::TooLarge { endpoint: label.clone(), max_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpMethod {
    Get,
    Post,
}

#[derive(Clone, Copy)]
struct HttpBody<'a> {
    content_type: &'static str,
    bytes: &'a [u8],
}

/// One request, with no address of its own.
///
/// [`HttpClient::send`] appends a path to an [`Endpoint`]'s canonical URL and
/// [`HttpClient::send_to`] takes a whole URL. A caller holding an [`Endpoint`] sends
/// through the first rather than assemble an address, so a query string that carries an
/// API key is never dropped.
#[derive(Clone)]
pub struct HttpRequest<'a> {
    method: HttpMethod,
    body: Option<HttpBody<'a>>,
    headers: &'a [(&'a str, &'a str)],
    timeout: Duration,
    max_bytes: usize,
}

/// Header values and the body can carry an API key, so only their names and size show.
impl fmt::Debug for HttpRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self.headers.iter().map(|&(name, _)| name).collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("timeout", &self.timeout)
            .field("max_bytes", &self.max_bytes)
            .field("header_names", &header_names)
            .field("body_bytes", &self.body.map(|body| body.bytes.len()))
            .finish_non_exhaustive()
    }
}

impl<'a> HttpRequest<'a> {
    pub const fn get(timeout: Duration) -> Self {
        Self {
            method: HttpMethod::Get,
            body: None,
            headers: &[],
            timeout,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    pub const fn post_json(body: &'a str, timeout: Duration) -> Self {
        Self {
            method: HttpMethod::Post,
            body: Some(HttpBody { content_type: "application/json", bytes: body.as_bytes() }),
            headers: &[],
            timeout,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// A body with its own content type, such as the Monero daemon's epee routes or the raw
    /// transaction hex that Blockbook takes as plain text.
    pub const fn post_bytes(content_type: &'static str, body: &'a [u8], timeout: Duration) -> Self {
        Self {
            method: HttpMethod::Post,
            body: Some(HttpBody { content_type, bytes: body }),
            headers: &[],
            timeout,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    #[must_use]
    pub const fn with_headers(mut self, headers: &'a [(&'a str, &'a str)]) -> Self {
        self.headers = headers;
        self
    }

    /// Refuses a response body larger than `max_bytes` in place of the default cap.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

/// One HTTP exchange that completed, whatever its status.
///
/// What a non-success status means is the caller's decision, so the body and the status
/// are both here, and only [`HttpResponse::into_bytes`] and [`HttpResponse::into_body`]
/// interpret the status.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    label: EndpointLabel,
    status: u16,
    body: Vec<u8>,
    retry_after: Option<RetryAfter>,
}

impl HttpResponse {
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// The body, or the failure the status implies. 429 and 503 carry the
    /// server's `Retry-After` through to the pacer.
    pub fn into_bytes(self) -> Result<Vec<u8>, NetError> {
        if matches!(self.status, 200..300) {
            return Ok(self.body);
        }
        if matches!(self.status, 429 | 503) {
            return Err(NetError::RateLimited {
                endpoint: self.label,
                retry_after: self.retry_after,
            });
        }
        Err(NetError::Status { endpoint: self.label, status: self.status })
    }

    /// The body as text. A body that is not UTF-8 is a protocol failure, because every
    /// text API this crate speaks to is JSON.
    pub fn into_body(self) -> Result<String, NetError> {
        let label = self.label.clone();
        let bytes = self.into_bytes()?;
        String::from_utf8(bytes)
            .map_err(|_| NetError::protocol(&label, "the response is not utf-8"))
    }
}

/// One endpoint bound to the shared client.
///
/// This is what an HTTP chain's [`crate::Pool`] caches, so no chain crate builds
/// its own client, URL, or rotation.
#[derive(Debug, Clone)]
pub struct HttpEndpoint {
    client: HttpClient,
    endpoint: Endpoint,
}

impl HttpEndpoint {
    pub const fn new(client: HttpClient, endpoint: Endpoint) -> Self {
        Self { client, endpoint }
    }

    pub const fn label(&self) -> &EndpointLabel {
        self.endpoint.label()
    }

    pub async fn send(
        &self,
        path: &str,
        request: HttpRequest<'_>,
    ) -> Result<HttpResponse, NetError> {
        self.client.send(&self.endpoint, path, request).await
    }

    /// One request to a REST service that takes its arguments in the query
    /// string.
    ///
    /// `query` is merged into the endpoint's own query rather than replacing it, so a
    /// key there survives.
    pub async fn send_query(
        &self,
        path: &str,
        query: &str,
        request: HttpRequest<'_>,
    ) -> Result<HttpResponse, NetError> {
        let url = self.endpoint.join(path, query);
        self.client.send_to(self.label(), &url, request).await
    }

    /// One JSON-RPC call to the endpoint's own address, decoded straight into `T`.
    pub async fn rpc<T: DeserializeOwned>(
        &self,
        request: &JsonRpcRequest,
        timeout: Duration,
    ) -> Result<T, NetError> {
        let body =
            self.send("", HttpRequest::post_json(request.as_str(), timeout)).await?.into_body()?;
        json_rpc_decode(self.label(), &body)
    }

    /// One JSON-RPC batch to the endpoint's own address. The response is length-checked
    /// and reordered against the batch's own request ids before any element is handed
    /// back.
    pub async fn rpc_batch(
        &self,
        batch: &JsonRpcBatch,
        timeout: Duration,
    ) -> Result<Vec<Box<RawValue>>, NetError> {
        let body =
            self.send("", HttpRequest::post_json(batch.as_str(), timeout)).await?.into_body()?;
        let borrowed = json_rpc_decode_batch(self.label(), &body, batch.ids())?;
        Ok(borrowed.into_iter().map(RawValue::to_owned).collect())
    }
}

/// Hands every endpoint of an HTTP transport the shared client, and runs the chain's
/// identity check once before the endpoint's first request.
#[derive(Debug, Clone)]
pub struct HttpConnector {
    client: HttpClient,
    check: Arc<dyn IdentityCheck<HttpEndpoint>>,
}

impl HttpConnector {
    pub const fn new(client: HttpClient, check: Arc<dyn IdentityCheck<HttpEndpoint>>) -> Self {
        Self { client, check }
    }
}

#[async_trait]
impl Connect<HttpEndpoint> for HttpConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<HttpEndpoint>, NetError> {
        Ok(Arc::new(HttpEndpoint::new(self.client.clone(), endpoint.clone())))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        self.check.identify(endpoint, connection).await
    }
}

/// Classifies a reqwest failure for the whole workspace.
///
/// The error is stripped of its URL, because reqwest's `Display` embeds the full
/// address, query string included.
fn transport_error(label: &EndpointLabel, timeout: Duration, source: reqwest::Error) -> NetError {
    if source.is_timeout() {
        return NetError::timed_out(label, timeout);
    }
    let kind = if source.is_connect() {
        if is_tls_failure(&source) { FailureKind::Tls } else { FailureKind::Connect }
    } else if source.is_decode() || source.is_body() {
        FailureKind::Protocol
    } else {
        FailureKind::Transport
    };
    NetError::endpoint(label, kind, source.without_url())
}

/// Whether a failed connection failed in its TLS handshake, which reqwest reports as a
/// connect failure like any other.
///
/// The rustls error sits inside `io::Error` wrappers, and an `io::Error`'s `source` skips
/// the error it wraps, so each wrapper is opened with `get_ref` instead.
fn is_tls_failure(error: &reqwest::Error) -> bool {
    let mut cause: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = cause {
        if current.is::<rustls::Error>() {
            return true;
        }
        cause = match current.downcast_ref::<io::Error>().and_then(io::Error::get_ref) {
            Some(wrapped) => Some(wrapped),
            None => current.source(),
        };
    }
    false
}
