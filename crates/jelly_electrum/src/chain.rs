use std::iter;

use bitcoin::BlockHash;

/// A block a chain's server must serve at a height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub height: u32,
    pub hash: BlockHash,
}

/// The blocks a server must serve to prove its chain, and how that chain frames headers.
///
/// Every coin speaks the same protocol on the same ports, so the blocks a server serves
/// are the only way to tell which chain answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectrumChain {
    pub genesis: BlockHash,
    /// A block after the split from a chain that shares this one's block zero. Without
    /// it, Bitcoin and Bitcoin Cash would each accept the other's servers.
    pub checkpoint: Option<Checkpoint>,
    /// Whether a header may carry its parent chain's proof of work after the eighty
    /// bytes, as Dogecoin's do. The proof isn't checked, since only a header's hash,
    /// parent, and time are read.
    pub is_merge_mined: bool,
}

impl ElectrumChain {
    /// The blocks a server proves, block zero first.
    pub(crate) fn proofs(self) -> impl Iterator<Item = Checkpoint> {
        iter::once(Checkpoint { height: 0, hash: self.genesis }).chain(self.checkpoint)
    }
}
