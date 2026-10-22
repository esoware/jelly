use jelly_core::TxPayload;
use monero_wallet::send::SignableTransaction;

use crate::byte_count::ByteCount;
use crate::error::MoneroError;
use crate::reader::Reader;
use crate::subaddress::Subaddress;

/// Identifies this adapter's signing payload.
const MAGIC: &[u8; 8] = b"JLYXMRTX";

// The layout is little endian. Magic (8), the account's public
// spend key (32), the height the ring was chosen against (8), the sending
// subaddress major (4) and minor (4), the endpoint length (2) and its utf-8
// bytes, then `SignableTransaction::write` for the rest.

/// Everything in the frame before the endpoint's bytes.
const HEADER_BYTES: usize = 58;

/// Unsigned Monero payment plus signing context. The signable transaction owns inputs,
/// ring, outputs, and fee.
///
/// Additional fields identify the source account, ring-selection height, selecting
/// endpoint, and sending subaddress. They prevent cross-wallet signing, reject stale rings,
/// send the transaction to the selecting node, and attribute pending sends.
pub(crate) struct UnsignedPayload {
    pub signable: SignableTransaction,
    pub public_spend: [u8; 32],
    pub build_height: u64,
    pub subaddress: Subaddress,
    pub endpoint: String,
}

impl UnsignedPayload {
    /// The signable transaction carries the outgoing view key, so it is written
    /// straight into a frame sized for it up front, which never grows and leaves
    /// no copy behind.
    pub(crate) fn encode(&self) -> Result<TxPayload, MoneroError> {
        let endpoint = self.endpoint.as_bytes();
        let length = u16::try_from(endpoint.len()).map_err(|_| MoneroError::Internal {
            context: "the ring endpoint does not fit in an unsigned transaction",
        })?;
        let mut counted = ByteCount::default();
        self.signable.write(&mut counted).map_err(|_| unserialisable())?;
        let mut framed = Vec::with_capacity(
            HEADER_BYTES.saturating_add(endpoint.len()).saturating_add(counted.total()),
        );
        framed.extend_from_slice(MAGIC);
        framed.extend_from_slice(&self.public_spend);
        framed.extend_from_slice(&self.build_height.to_le_bytes());
        framed.extend_from_slice(&self.subaddress.major.to_le_bytes());
        framed.extend_from_slice(&self.subaddress.minor.to_le_bytes());
        framed.extend_from_slice(&length.to_le_bytes());
        framed.extend_from_slice(endpoint);
        self.signable.write(&mut framed).map_err(|_| unserialisable())?;
        Ok(TxPayload::from_bytes(framed))
    }

    pub(crate) fn decode(payload: &TxPayload) -> Result<Self, MoneroError> {
        let mut reader = Reader::new(payload.as_bytes(), truncated);
        if reader.take(MAGIC.len())? != MAGIC.as_slice() {
            return Err(MoneroError::Payload { detail: "it lacks this build's header" });
        }
        let public_spend = reader.array32()?;
        let build_height = reader.u64()?;
        let subaddress = Subaddress::new(reader.u32()?, reader.u32()?);
        let endpoint = reader.text(MoneroError::Payload { detail: "the endpoint is not text" })?;
        let mut body = reader.into_rest();
        let signable = SignableTransaction::read(&mut body)
            .map_err(|_| MoneroError::Payload { detail: "the transaction does not decode" })?;
        if !body.is_empty() {
            return Err(MoneroError::Payload { detail: "it has trailing bytes" });
        }
        Ok(Self { signable, public_spend, build_height, subaddress, endpoint })
    }
}

const fn truncated() -> MoneroError {
    MoneroError::Payload { detail: "it is truncated" }
}

const fn unserialisable() -> MoneroError {
    MoneroError::Internal { context: "a signable transaction did not serialise" }
}
