//! Every crate's path to OS entropy. A failure propagates and never falls back to a
//! userspace RNG, because a key or nonce that isn't random is worse than none at all.
//!
//! The exception is `jelly_monero`: its Monero library takes a `rand_core` 0.6 RNG, so it
//! passes `rand_core_06::OsRng`, which reads the same OS source but can't report a failure
//! through here.

use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
#[error("the OS entropy source failed")]
pub struct EntropyError(#[source] getrandom::Error);

#[expect(clippy::disallowed_methods, reason = "the only entropy source")]
pub fn random_fill(target: &mut [u8]) -> Result<(), EntropyError> {
    getrandom::fill(target).map_err(EntropyError)
}

/// `N` fresh bytes in a buffer that wipes on drop.
pub fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>, EntropyError> {
    let mut bytes = Zeroizing::new([0u8; N]);
    random_fill(bytes.as_mut())?;
    Ok(bytes)
}
