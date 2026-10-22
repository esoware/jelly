use std::io;

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
        reason = "the trait names this `buf`, which this workspace's naming rules disallow"
    )]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
