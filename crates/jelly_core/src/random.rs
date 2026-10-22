//! The workspace's one entry point to OS entropy. A failure is propagated, never replaced
//! by a userspace fallback, because a key or nonce that is not random is worse than one
//! that was never made.
//!
//! The exception is `jelly_monero`, whose Monero library takes a `rand_core` 0.6
//! RNG, so it passes `rand_core_06::OsRng`. That reads the same OS source but cannot
//! report failure through here.

use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[error("the os entropy source failed")]
pub struct EntropyError(#[source] getrandom::Error);

#[expect(clippy::disallowed_methods, reason = "this is the workspace's only entropy source")]
pub fn random_fill(target: &mut [u8]) -> Result<(), EntropyError> {
    getrandom::fill(target).map_err(EntropyError)
}

/// `N` fresh bytes in a buffer that wipes on drop.
pub fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>, EntropyError> {
    let mut bytes = Zeroizing::new([0u8; N]);
    random_fill(bytes.as_mut())?;
    Ok(bytes)
}
