use std::collections::{HashMap, HashSet};

use jelly_core::{ChainEntry, ChainId, ChainSpec, EndpointOverride, Transport};
use parking_lot::RwLock;

use crate::defaults::DefaultEndpoints;
use crate::endpoint::{Endpoint, EndpointOrigin, PlaintextPolicy, UrlRejection, canonicalize};
use crate::error::NetError;

/// The entries of every chain the network layer serves. The registry reads each chain's
/// transports from its spec and its curated defaults from its `endpoints.json`.
pub type ChainEntries = &'static [&'static dyn ChainEntry];

const TARGET: &str = "jelly::net::registry";

/// An endpoint and whether it is enabled, in configured order.
///
/// Whether the row is pinned is [`Endpoint::origin`] and is deliberately not
/// repeated here, so the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointStatus {
    pub endpoint: Endpoint,
    pub is_enabled: bool,
}

/// The single owner of which endpoints exist. Curated defaults and user overrides merge
/// here, so a pool and the interface never show different lists.
#[derive(Debug)]
pub struct EndpointRegistry {
    chains: ChainEntries,
    /// Every chain's curated defaults, read once from the entries, or why its file is
    /// unusable. All Jelly knows about where to find a chain until the user adds
    /// endpoints, and nothing is fetched at runtime.
    defaults: HashMap<ChainId, Result<DefaultEndpoints, &'static str>>,
    overrides: RwLock<HashMap<ChainId, Vec<EndpointOverride>>>,
}

impl EndpointRegistry {
    pub fn new(chains: ChainEntries) -> Self {
        let defaults = chains
            .iter()
            .map(|entry| {
                let chain = entry.spec().chain;
                (chain, DefaultEndpoints::parse(chain, entry.default_endpoints()))
            })
            .collect();
        Self { chains, defaults, overrides: RwLock::default() }
    }

    /// The spec of a chain this registry serves. A chain with no entry has no endpoints.
    fn spec(&self, chain: ChainId) -> Result<&'static ChainSpec, NetError> {
        self.chains
            .iter()
            .map(|entry| entry.spec())
            .find(|spec| spec.chain == chain)
            .ok_or(NetError::NoEndpoints { chain })
    }

    /// The curated defaults for `chain`, in shipped order, most preferred first.
    pub fn defaults(&self, chain: ChainId) -> Result<&DefaultEndpoints, NetError> {
        match self.defaults.get(&chain) {
            Some(Ok(file)) => Ok(file),
            Some(Err(detail)) => Err(NetError::Defaults { chain, detail }),
            None => Err(NetError::NoEndpoints { chain }),
        }
    }

    /// Stored overrides retain their plaintext opt-in but must still name a transport
    /// the chain uses.
    pub fn parse_stored(&self, chain: ChainId, input: &str) -> Result<Endpoint, NetError> {
        self.parse_for_chain(chain, input, PlaintextPolicy::Allowed)
    }

    /// Reads `input` under the transport [`ChainSpec::transport_for`] picks, which must be
    /// one the chain uses.
    fn parse_for_chain(
        &self,
        chain: ChainId,
        input: &str,
        plaintext: PlaintextPolicy,
    ) -> Result<Endpoint, NetError> {
        let spec = self.spec(chain)?;
        let transport = spec.transport_for(input);
        if !spec.transports.contains(&transport) {
            return Err(NetError::InvalidUrl { reason: UrlRejection::UnusedTransport });
        }
        Endpoint::parse_with(chain, transport, input, plaintext)
    }

    /// Replaces every chain's overrides at once, which is what a re-read of the
    /// database means.
    pub fn load_overrides(&self, rows: impl IntoIterator<Item = (ChainId, EndpointOverride)>) {
        let mut by_chain: HashMap<ChainId, Vec<EndpointOverride>> = HashMap::new();
        for (chain, row) in rows {
            if self.is_usable(chain, &row) {
                by_chain.entry(chain).or_default().push(row);
            }
        }
        *self.overrides.write() = by_chain;
    }

    /// Every endpoint the user could reach on `chain`, disabled ones included, with
    /// overrides merged into the shipped endpoints by canonical URL.
    ///
    /// Orders pinned endpoints first, then custom endpoints, then defaults. Order
    /// within each group is stable. Pools apply health ranking per request.
    pub fn configured(&self, chain: ChainId) -> Result<Vec<EndpointStatus>, NetError> {
        let overrides = self.overrides.read();
        let rows = overrides.get(&chain).map_or(&[][..], Vec::as_slice);
        let by_url: HashMap<&str, &EndpointOverride> =
            rows.iter().map(|row| (row.url.as_str(), row)).collect();
        let curated = &self.defaults(chain)?.endpoints;
        let curated_urls: HashSet<&str> = curated.iter().map(|entry| entry.url.as_str()).collect();

        let mut ordered: Vec<(EndpointOrigin, usize, EndpointStatus)> =
            Vec::with_capacity(rows.len().saturating_add(curated.len()));

        for (index, row) in rows.iter().enumerate() {
            if curated_urls.contains(row.url.as_str()) {
                continue;
            }
            let origin = if row.is_pinned { EndpointOrigin::Pinned } else { EndpointOrigin::User };
            // `load_overrides` kept only rows this same call accepts, so it cannot fail
            // here.
            let endpoint =
                self.parse_stored(chain, &row.url)?.with_input(&row.display).with_origin(origin);
            ordered.push((origin, index, EndpointStatus { endpoint, is_enabled: row.is_enabled }));
        }

        for (index, entry) in curated.iter().enumerate() {
            let row = by_url.get(entry.url.as_str()).copied();
            let is_pinned = row.is_some_and(|row| row.is_pinned);
            let origin = if is_pinned { EndpointOrigin::Pinned } else { EndpointOrigin::Default };
            let endpoint = Endpoint::parse(chain, entry.transport, &entry.url)?
                .with_origin(origin)
                .with_note(entry.note.clone());
            ordered.push((
                origin,
                rows.len().saturating_add(index),
                EndpointStatus { endpoint, is_enabled: row.is_none_or(|row| row.is_enabled) },
            ));
        }

        ordered.sort_by_key(|(origin, index, _)| (*origin, *index));
        Ok(ordered.into_iter().map(|(_, _, status)| status).collect())
    }

    /// The endpoints a pool walks for one transport, in configured order.
    ///
    /// Not health order, because a pool re-reads health and sorts on every walk. A chain
    /// with both an RPC pool and a REST pool builds each from its own call here.
    pub fn enabled(&self, chain: ChainId, transport: Transport) -> Result<Vec<Endpoint>, NetError> {
        Ok(self
            .configured(chain)?
            .into_iter()
            .filter(|row| row.is_enabled && row.endpoint.transport() == transport)
            .map(|row| row.endpoint)
            .collect())
    }

    /// Resolves a URL the user typed to the one canonical endpoint the pools,
    /// the health tracker and the database all key on.
    ///
    /// An address already in the list resolves under that entry's own transport.
    /// Anything else is read as the chain's primary transport, or as the websocket
    /// transport when the scheme names it, which a chain with no websocket pool
    /// refuses.
    pub fn resolve(
        &self,
        chain: ChainId,
        url: &str,
        plaintext: PlaintextPolicy,
    ) -> Result<Endpoint, NetError> {
        if let Some(endpoint) = self.find(chain, url)? {
            return Ok(endpoint);
        }
        Ok(self.parse_for_chain(chain, url, plaintext)?.with_origin(EndpointOrigin::User))
    }

    /// The configured endpoint, disabled or not, that any spelling of `url` names.
    pub fn find(&self, chain: ChainId, url: &str) -> Result<Option<Endpoint>, NetError> {
        let wanted = self.spec(chain)?.transport_for(url);
        for row in self.configured(chain)? {
            let transport = row.endpoint.transport();
            // Canonicalizing under a row's transport folds the scheme into that
            // transport's own, so a websocket address would match the same host's HTTP
            // row and the reverse. The scheme the user typed decides which they meant.
            if (transport == Transport::WsJsonrpc) != (wanted == Transport::WsJsonrpc) {
                continue;
            }
            if canonicalize(url, transport, PlaintextPolicy::Allowed)
                .is_ok_and(|candidate| candidate == row.endpoint.url())
            {
                return Ok(Some(row.endpoint));
            }
        }
        Ok(None)
    }

    /// Unusable rows must not prevent valid endpoints from loading. The database
    /// loader deletes these rows; other callers still get the usable subset.
    fn is_usable(&self, chain: ChainId, row: &EndpointOverride) -> bool {
        let Err(error) = self.parse_stored(chain, &row.url) else {
            return true;
        };
        tracing::warn!(
            target: TARGET,
            chain = chain.as_str(),
            %error,
            "an unusable stored endpoint was skipped"
        );
        false
    }
}
