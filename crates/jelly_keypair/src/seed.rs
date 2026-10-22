use std::fmt;

use secrecy::{ExposeSecret as _, SecretBox};

/// A 64-byte BIP-39 seed, wiped on drop and deliberately not `Clone`.
///
/// Built in place on the heap so the finished seed is never copied through the stack.
pub struct Seed(SecretBox<[u8; SEED_BYTES]>);

const SEED_BYTES: usize = 64;

impl Seed {
    pub const LEN: usize = SEED_BYTES;

    /// Takes a seed already on the heap, so the caller's buffer is the only copy.
    pub fn from_boxed(bytes: Box<[u8; Self::LEN]>) -> Self {
        Self(SecretBox::new(bytes))
    }

    /// The closure keeps every use a visible call site and hands out no copy a caller
    /// could leave unwiped.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8; Self::LEN]) -> R) -> R {
        read(self.0.expose_secret())
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(<redacted>)")
    }
}
