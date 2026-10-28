//! Shared HTTP, TLS, websocket, and proxy code.
//!
//! TLS runs on `ring` and trusts the compiled-in Mozilla roots, never the platform store.
//! Electrum alone pins each server's key on first use, since most public servers are
//! self-signed.
//!
//! Every transport follows the one proxy setting, and direct mode ignores system proxies.
//! Each chain's crate supplies the check for which network an endpoint serves, so this
//! crate knows nothing about any network. Logs and errors name an endpoint only by its
//! redacted label, because a URL can carry credentials.
//!
//! `Pool` tries endpoints in health order under per-endpoint pacing, and the caller's
//! classifier decides what each failure means. A broadcast gets one attempt, because a
//! timeout doesn't prove the node rejected it. Websocket pushes only wake sync, which
//! re-fetches before acting.

#![deny(clippy::float_arithmetic)]
mod connect;
mod credentials;
mod defaults;
mod dialer;
mod endpoint;
mod error;
mod health;
mod http;
mod jsonrpc;
mod network;
mod pacer;
mod pool;
mod probe;
mod proxy;
mod registry;
mod retry_after;
mod runtime;
mod spki;
#[cfg(feature = "test-util")]
pub mod test_util;
mod tls;
mod ws;

pub use crate::connect::{Connect, IDENTITY_TIMEOUT, Identity, IdentityCheck, NoIdentity};
pub use crate::defaults::{DefaultEndpoint, DefaultEndpoints};
pub use crate::dialer::{Dialer, Socket};
pub use crate::endpoint::{Endpoint, EndpointOrigin, PlaintextPolicy, UrlRejection};
pub use crate::error::NetError;
pub use crate::health::{HealthClass, HealthEntry, HealthTracker};
pub use crate::http::{HttpClient, HttpConnector, HttpEndpoint, HttpRequest, HttpResponse};
pub use crate::jsonrpc::{
    JsonRpcBatch, JsonRpcError, JsonRpcRequest, JsonRpcResponse, json_rpc_decode, json_rpc_outcome,
};
pub use crate::network::{Network, PreparedProxy};
pub use crate::pacer::PacerPolicy;
pub use crate::pool::{Outcome, Pool, Standing, TimeoutPolicy};
pub use crate::probe::{ProbeFailure, ProbeReport, probe, probe_reachable};
pub use crate::registry::{ChainEntries, EndpointRegistry, EndpointStatus};
pub use crate::retry_after::RetryAfter;
pub use crate::runtime::NetRuntime;
pub use crate::ws::{WsClient, WsConnector, WsSubscription};
