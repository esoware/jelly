use std::sync::Arc;

use jelly_core::{ChainId, Clock, Transport};

use crate::connect::Connect;
use crate::endpoint::Endpoint;
use crate::health::HealthTracker;
use crate::pacer::{Pacer, PacerPolicy};
use crate::pool::Pool;
use crate::registry::{ChainEntries, EndpointRegistry};

/// The bookkeeping every pool shares, one clock, health tracker, pacer, and endpoint
/// registry.
///
/// The clock, health tracker, and pacer live together because they must agree on time.
/// Splitting them would let a pool rotate on one clock and record on another. The
/// registry rides along so every pool and the interface read one list.
#[derive(Debug)]
pub struct NetRuntime {
    clock: Arc<dyn Clock>,
    health: Arc<HealthTracker>,
    pacer: Arc<Pacer>,
    registry: EndpointRegistry,
}

impl NetRuntime {
    pub fn new(clock: Arc<dyn Clock>, chains: ChainEntries) -> Arc<Self> {
        Self::build(clock, PacerPolicy::DEFAULT, chains)
    }

    /// A runtime that serves no chain, for tests that build their pools by hand.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_pacing(clock: Arc<dyn Clock>, pacing: PacerPolicy) -> Arc<Self> {
        Self::build(clock, pacing, &[])
    }

    /// A runtime that serves `chains`, for tests of what a chain builds over the registry.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_chains(
        clock: Arc<dyn Clock>,
        pacing: PacerPolicy,
        chains: ChainEntries,
    ) -> Arc<Self> {
        Self::build(clock, pacing, chains)
    }

    fn build(clock: Arc<dyn Clock>, pacing: PacerPolicy, chains: ChainEntries) -> Arc<Self> {
        let health = Arc::new(HealthTracker::new(Arc::clone(&clock)));
        let pacer = Arc::new(Pacer::new(pacing, Arc::clone(&clock)));
        Arc::new(Self { clock, health, pacer, registry: EndpointRegistry::new(chains) })
    }

    pub(crate) const fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub const fn health(&self) -> &Arc<HealthTracker> {
        &self.health
    }

    pub(crate) const fn pacer(&self) -> &Arc<Pacer> {
        &self.pacer
    }

    pub const fn registry(&self) -> &EndpointRegistry {
        &self.registry
    }

    /// A pool over `endpoints`. The caller decides what an empty rotation means for its
    /// chain, so nothing is read from the registry here.
    pub fn pool<C: Send + Sync + 'static>(
        &self,
        chain: ChainId,
        transport: Transport,
        connector: Arc<dyn Connect<C>>,
        endpoints: Vec<Endpoint>,
    ) -> Pool<C> {
        Pool::new(chain, transport, connector, endpoints, self)
    }
}
