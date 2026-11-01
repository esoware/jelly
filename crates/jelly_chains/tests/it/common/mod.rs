use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jelly_net::{
    EndpointRegistry, NetError, Network,
    test_util::{self, LoopbackHttp},
};

pub(crate) type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// A registry over every chain's shipped defaults.
pub(crate) fn registry() -> EndpointRegistry {
    EndpointRegistry::new(jelly_chains::chains().entries())
}

/// A direct network serving every chain, on a fake clock with no pins stored.
pub(crate) fn direct_network() -> Result<Arc<Network>, NetError> {
    test_util::direct_network(jelly_chains::chains().entries())
}

/// A loopback HTTP server that answers each request with the next of `replies`, as
/// `(status, body)`, and with a 503 once they run out.
pub(crate) async fn serve(replies: Vec<(&'static str, &'static str)>) -> Fallible<LoopbackHttp> {
    let next = AtomicUsize::new(0);
    Ok(LoopbackHttp::start(move |_| {
        let (status, body) = replies
            .get(next.fetch_add(1, Ordering::SeqCst))
            .copied()
            .unwrap_or(("503 Service Unavailable", ""));
        (status, body.to_owned())
    })
    .await?)
}
