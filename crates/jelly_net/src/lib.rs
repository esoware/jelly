//! Shared HTTP, TLS, websocket, and proxy code.
//!
//! TLS uses compiled-in Mozilla roots and `ring`, not the platform root store. Electrum
//! alone may fall back to trust-on-first-use pinning for self-signed servers.
//!
//! All transports follow the same proxy setting. Direct mode ignores system proxies.
//! Endpoint identity is checked before use, by a check each chain's crate supplies, so
//! this crate knows no network's identity. Logs and errors use redacted endpoint labels
//! because URLs can contain credentials.
//!
//! `Pool` orders endpoints by health and applies pacing and retry classification. Broadcast
//! gets one attempt because a timeout does not prove rejection. Websocket notifications
//! only wake sync, which re-fetches data before acting.

#![deny(clippy::float_arithmetic)]
mod connect;
mod credentials;
mod defaults;
mod dialler;
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
pub use crate::dialler::{Dialler, Socket};
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
