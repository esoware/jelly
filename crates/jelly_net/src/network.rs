use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;
use jelly_core::{BuildError, ChainId, ChainSpec, Clock, PinStore, ProxySetting, Transport};

use crate::connect::{Connect, NoIdentity};
use crate::dialer::Dialer;
use crate::endpoint::Endpoint;
use crate::error::NetError;
use crate::health::HealthTracker;
use crate::http::{HttpClient, HttpConnector, HttpEndpoint};
use crate::pool::{Pool, TimeoutPolicy};
use crate::registry::{ChainEntries, EndpointRegistry};
use crate::runtime::NetRuntime;
use crate::ws::{WsClient, WsConnector};

const TARGET: &str = "jelly::net::network";

/// The shared client and the route it was built for. They are swapped as one, so a
/// reader never pairs the new client with the old route.
pub struct PreparedProxy {
    http: HttpClient,
    proxy: Option<ProxySetting>,
}

impl fmt::Debug for PreparedProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedProxy")
            .field("is_proxied", &self.proxy.is_some())
            .finish_non_exhaustive()
    }
}

/// The network layer: the shared HTTP client, the TLS pin store, the proxy route, and the
/// runtime every pool is built from.
///
/// The shell owns the live route. An adapter build takes its own copy of the route with
/// [`Network::with_proxy`] and shares the runtime and pins.
pub struct Network {
    runtime: Arc<NetRuntime>,
    pins: Arc<dyn PinStore>,
    egress: ArcSwap<PreparedProxy>,
}

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Network")
            .field("is_proxied", &self.egress.load().proxy.is_some())
            .finish_non_exhaustive()
    }
}

impl Network {
    /// A failure here aborts startup, because there's no fallback client.
    pub fn start(
        clock: Arc<dyn Clock>,
        pins: Arc<dyn PinStore>,
        proxy: Option<ProxySetting>,
        chains: ChainEntries,
    ) -> Result<Arc<Self>, NetError> {
        let http = HttpClient::new(proxy.as_ref())?;
        Ok(Arc::new(Self {
            runtime: NetRuntime::new(clock, chains),
            pins,
            egress: ArcSwap::from_pointee(PreparedProxy { http, proxy }),
        }))
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        self.runtime.clock()
    }

    pub fn health(&self) -> &Arc<HealthTracker> {
        self.runtime.health()
    }

    pub fn registry(&self) -> &EndpointRegistry {
        self.runtime.registry()
    }

    pub fn http(&self) -> HttpClient {
        self.egress.load().http.clone()
    }

    pub fn proxy(&self) -> Option<ProxySetting> {
        self.egress.load().proxy.clone()
    }

    /// Builds a route without publishing it or making a request, so its setting
    /// can be persisted before any traffic uses it.
    pub fn prepare_proxy(&self, proxy: Option<ProxySetting>) -> Result<PreparedProxy, NetError> {
        let current = self.egress.load();
        let http = if current.proxy == proxy {
            current.http.clone()
        } else {
            HttpClient::new(proxy.as_ref())?
        };
        Ok(PreparedProxy { http, proxy })
    }

    /// Publishes a prepared route, resetting pacing if the setting changed. Existing pools
    /// keep their captured clients until the caller rebuilds them.
    pub fn apply_proxy(&self, prepared: PreparedProxy) {
        if self.egress.load().proxy == prepared.proxy {
            return;
        }
        self.egress.store(Arc::new(prepared));
        self.runtime.pacer().reset();
    }

    /// A copy routed through `proxy` for an adapter build, leaving the shared route alone.
    pub fn with_proxy(&self, proxy: Option<ProxySetting>) -> Result<Self, NetError> {
        Ok(Self {
            runtime: Arc::clone(&self.runtime),
            pins: Arc::clone(&self.pins),
            egress: ArcSwap::from_pointee(self.prepare_proxy(proxy)?),
        })
    }

    /// The raw TLS dialer for the websocket transport, with full CA validation and the
    /// same proxy. A pool captures it at construction, so a proxy change needs a pool
    /// rebuild.
    pub fn ws_dialer(&self) -> Result<Dialer, NetError> {
        Dialer::ca_validated(
            self.proxy(),
            TimeoutPolicy::for_transport(Transport::WsJsonrpc).connect,
        )
    }

    /// The raw TLS dialer for `chain`'s Electrum servers, with the same proxy, captured
    /// by a pool as [`Network::ws_dialer`] is.
    ///
    /// Most public Electrum servers are self-signed, so it trusts a server's key on first
    /// use and refuses a changed one unless the replacement chains to the compiled-in
    /// roots. There's no accept-anything mode.
    pub fn electrum_dialer(&self, chain: ChainId) -> Result<Dialer, NetError> {
        Dialer::pinned(
            Arc::clone(&self.pins),
            Arc::clone(self.clock()),
            chain,
            self.proxy(),
            TimeoutPolicy::for_transport(Transport::ElectrumSsl).connect,
        )
    }

    /// The pool over the chain's primary transport, which every adapter needs. No enabled
    /// endpoint is [`BuildError::NoEndpoints`], not a failure.
    pub fn primary_pool<C, K>(
        &self,
        spec: &ChainSpec,
        connector: K,
    ) -> Result<Arc<Pool<C>>, BuildError>
    where
        C: Send + Sync + 'static,
        K: Connect<C> + 'static,
    {
        let transport = spec.primary_transport;
        let endpoints = self.registry().enabled(spec.chain, transport)?;
        if endpoints.is_empty() {
            return Err(BuildError::NoEndpoints { chain: spec.chain, transport });
        }
        Ok(Arc::new(self.runtime.pool(spec.chain, transport, Arc::new(connector), endpoints)))
    }

    /// A pool for an optional REST service, or `None` when none is configured and the
    /// adapter falls back to its own protocol. These services serve no network, so nothing
    /// checks their identity.
    pub fn rest_pool(
        &self,
        chain: ChainId,
        transport: Transport,
    ) -> Result<Option<Arc<Pool<HttpEndpoint>>>, BuildError> {
        self.optional_pool(chain, transport, || {
            Ok(HttpConnector::new(self.http(), Arc::new(NoIdentity)))
        })
    }

    /// `None` when no websocket endpoint is configured, and the chain then relies
    /// on its poll timer.
    pub fn ws_pool(
        &self,
        chain: ChainId,
        build_connector: impl FnOnce() -> Result<WsConnector, NetError>,
    ) -> Result<Option<Arc<Pool<WsClient>>>, BuildError> {
        self.optional_pool(chain, Transport::WsJsonrpc, build_connector)
    }

    /// `None` when `transport` has no enabled endpoint. The connector is built only when
    /// there's something to connect to.
    fn optional_pool<C, K>(
        &self,
        chain: ChainId,
        transport: Transport,
        build_connector: impl FnOnce() -> Result<K, NetError>,
    ) -> Result<Option<Arc<Pool<C>>>, BuildError>
    where
        C: Send + Sync + 'static,
        K: Connect<C> + 'static,
    {
        let endpoints: Vec<Endpoint> = self.registry().enabled(chain, transport)?;
        if endpoints.is_empty() {
            tracing::debug!(
                target: TARGET,
                chain = chain.as_str(),
                transport = transport.as_str(),
                "no endpoint is configured for this transport"
            );
            return Ok(None);
        }
        let connector = build_connector()?;
        Ok(Some(Arc::new(self.runtime.pool(chain, transport, Arc::new(connector), endpoints))))
    }
}
