//! The little-endian framing shared by the two persisted blobs and the signing payload.

use std::io;

use crate::error::MoneroError;

/// Little-endian fields read off the front of a byte slice. Running out of bytes
/// raises the format's own `truncated` error.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    truncated: fn() -> MoneroError,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8], truncated: fn() -> MoneroError) -> Self {
        Self { bytes, truncated }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) const fn into_rest(self) -> &'a [u8] {
        self.bytes
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], MoneroError> {
        let (head, rest) = self.bytes.split_at_checked(length).ok_or_else(self.truncated)?;
        self.bytes = rest;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, MoneroError> {
        self.fixed::<1>().map(|[byte]| byte)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, MoneroError> {
        Ok(u16::from_le_bytes(self.fixed()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, MoneroError> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, MoneroError> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    pub(crate) fn array32(&mut self) -> Result<[u8; 32], MoneroError> {
        self.fixed()
    }

    /// A `u16` length and that many bytes of utf-8, as [`write_text`] writes them,
    /// refused with `not_text` when they aren't utf-8.
    pub(crate) fn text(&mut self, not_text: MoneroError) -> Result<String, MoneroError> {
        let length = usize::from(self.u16()?);
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| not_text)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], MoneroError> {
        let (head, rest) = self.bytes.split_first_chunk::<N>().ok_or_else(self.truncated)?;
        self.bytes = rest;
        Ok(*head)
    }
}

/// Writes `text` behind its `u16` length, refused with `too_long` when the length
/// doesn't fit.
pub(crate) fn write_text(
    encoded: &mut Vec<u8>,
    text: &str,
    too_long: MoneroError,
) -> Result<(), MoneroError> {
    let length = u16::try_from(text.len()).map_err(|_| too_long)?;
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(text.as_bytes());
    Ok(())
}

/// An `io::Write` that keeps nothing and only counts.
///
/// A buffer that will hold secret bytes is measured with this first and
/// allocated at its final size, because a buffer that grows leaves a copy of
/// what it held in the allocation it moved out of.
#[derive(Debug, Default)]
pub(crate) struct ByteCount(usize);

impl ByteCount {
    pub(crate) const fn total(&self) -> usize {
        self.0
    }
}

impl io::Write for ByteCount {
    #[expect(
        clippy::renamed_function_params,
        reason = "the trait calls it `buf`, a name this workspace bans"
    )]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
