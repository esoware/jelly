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

    /// A `u16` length and that many bytes of utf-8, refused with `not_text` when
    /// they aren't.
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
