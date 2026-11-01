use std::sync::Arc;

use jelly_core::{ChainId, Clock, Transport};

use crate::connect::Connect;
use crate::endpoint::Endpoint;
use crate::health::HealthTracker;
use crate::pacer::{Pacer, PacerPolicy};
use crate::pool::Pool;

/// The clock, health tracker, and pacer every pool shares, so all pools agree on time and
/// on how each endpoint is behaving.
#[derive(Debug)]
pub struct NetRuntime {
    clock: Arc<dyn Clock>,
    health: HealthTracker,
    pacer: Pacer,
}

impl NetRuntime {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::build(clock, PacerPolicy::DEFAULT)
    }

    /// A runtime paced by `pacing`, for tests that build their pools by hand.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_pacing(clock: Arc<dyn Clock>, pacing: PacerPolicy) -> Arc<Self> {
        Self::build(clock, pacing)
    }

    fn build(clock: Arc<dyn Clock>, pacing: PacerPolicy) -> Arc<Self> {
        let health = HealthTracker::new(Arc::clone(&clock));
        let pacer = Pacer::new(pacing, Arc::clone(&clock));
        Arc::new(Self { clock, health, pacer })
    }

    pub(crate) const fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub const fn health(&self) -> &HealthTracker {
        &self.health
    }

    pub(crate) const fn pacer(&self) -> &Pacer {
        &self.pacer
    }

    /// A pool over `endpoints`. The caller decides what an empty list means for its chain.
    pub fn pool<C: Send + Sync + 'static>(
        self: &Arc<Self>,
        chain: ChainId,
        transport: Transport,
        connector: Arc<dyn Connect<C>>,
        endpoints: Vec<Endpoint>,
    ) -> Pool<C> {
        Pool::new(Arc::clone(self), chain, transport, connector, endpoints)
    }
}
