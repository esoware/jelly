use std::iter;

use bitcoin::BlockHash;

/// A block a chain's server must serve at a height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub height: u32,
    pub hash: BlockHash,
}

/// What a server must prove about the chain it serves, and how that chain frames its
/// headers.
///
/// The wire protocol is identical across coins and their server populations share
/// ports, so the blocks a server serves are the only thing that says which chain
/// answered. This crate knows no chain, so the caller supplies the blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectrumChain {
    pub genesis: BlockHash,
    /// A block past the last split from a chain that shares block zero, which a
    /// server of the other chain serves differently. Bitcoin and Bitcoin Cash share
    /// block zero, so without one each would take the other's servers.
    pub checkpoint: Option<Checkpoint>,
    /// Whether a header may carry its parent chain's proof of work after the eighty
    /// bytes, as Dogecoin's merge-mined headers do. The proof is not verified, because
    /// a header is only read for its hash, its parent, and its time.
    pub is_merge_mined: bool,
}

impl ElectrumChain {
    /// The blocks a server proves, block zero first.
    pub(crate) fn proofs(self) -> impl Iterator<Item = Checkpoint> {
        iter::once(Checkpoint { height: 0, hash: self.genesis }).chain(self.checkpoint)
    }
}
