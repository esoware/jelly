use std::fmt;

use crate::chain::ChainId;
use crate::error::CoreError;

/// SHA-256 of the DER `SubjectPublicKeyInfo`, including its header. Records a host's first
/// key for comparison on later handshakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpkiPin {
    sha256: [u8; 32],
    first_seen_unix_secs: i64,
}

impl SpkiPin {
    pub const fn new(sha256: [u8; 32], first_seen_unix_secs: i64) -> Self {
        Self { sha256, first_seen_unix_secs }
    }

    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub const fn first_seen_unix_secs(&self) -> i64 {
        self.first_seen_unix_secs
    }
}

impl fmt::Display for SpkiPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.sha256))
    }
}

/// TLS trust-on-first-use pin storage. The certificate verifier calls it synchronously, so
/// implementations must answer from memory without blocking I/O.
///
/// `put` replaces the existing key. The verifier decides whether replacement is allowed.
/// Store hosts exactly as given. Callers lowercase them first.
pub trait PinStore: Send + Sync + fmt::Debug {
    fn get(&self, chain: ChainId, host: &str) -> Result<Option<SpkiPin>, CoreError>;

    fn put(&self, chain: ChainId, host: &str, pin: SpkiPin) -> Result<(), CoreError>;
}
