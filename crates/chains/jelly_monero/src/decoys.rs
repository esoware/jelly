use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Bound, RangeBounds};

use curve25519_dalek::edwards::CompressedEdwardsY;
use curve25519_dalek::traits::IsIdentity as _;
use monero_interface::{
    EvaluateUnlocked, InterfaceError, ProvidesBlockchainMeta, ProvidesUnvalidatedDecoys,
    TransactionsError,
};
use monero_wallet::ed25519::Point;
use monero_wallet::ringct::clsag::Decoys;
use monero_wallet::{OutputWithDecoys, WalletOutput};
use parking_lot::Mutex;
use rand_core_06::OsRng;
use zeroize::Zeroizing;

use crate::error::MoneroError;
use crate::node::MoneroNode;
use crate::params::RING_LEN;

const TARGET: &str = "jelly::monero::decoys";

/// The cumulative `RingCT` output distribution, the height it covers, and the
/// endpoint that served it.
///
/// Decoy selection needs the whole distribution from block zero, tens of
/// megabytes on mainnet, so one is reused for a fixed window and each build
/// records the height its ring was chosen against.
///
/// The ring and the transaction built on it go back to that endpoint, which is kept
/// as its canonical URL because only the URL tells two endpoints on one host apart.
/// It can carry credentials, so `Debug` leaves it out.
pub struct Distribution {
    cumulative_outputs: Vec<u64>,
    height: u64,
    endpoint: String,
}

impl fmt::Debug for Distribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Distribution")
            .field("height", &self.height)
            .field("blocks", &self.cumulative_outputs.len())
            .finish_non_exhaustive()
    }
}

impl Distribution {
    pub fn new(cumulative_outputs: Vec<u64>, height: u64, endpoint: &str) -> Self {
        Self { cumulative_outputs, height, endpoint: endpoint.to_owned() }
    }

    /// The block the distribution runs to, which is the block a ring built from
    /// it is chosen against.
    pub(crate) const fn height(&self) -> u64 {
        self.height
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// How many outputs the chain held at [`Distribution::height`].
    fn output_count(&self) -> u64 {
        self.cumulative_outputs.last().copied().unwrap_or_default()
    }
}

/// An output a send may spend, with the ring an earlier build offered it with.
///
/// No `Debug`, because the output carries its key offset.
pub(crate) struct Candidate {
    pub output: WalletOutput,
    pub key_image: [u8; 32],
    pub ring: Option<Vec<u64>>,
}

/// The output's key (32), key offset (32), and commitment (40), which precede the
/// decoys in the encoding `OutputWithDecoys::read` takes.
const OUTPUT_DATA_BYTES: usize = 104;

/// Feeds `monero-wallet` decoy selection the cached output distribution and ring
/// candidates from one endpoint.
///
/// The original error is stashed separately because `InterfaceError` can't carry
/// it.
struct RingSource<'a> {
    node: &'a dyn MoneroNode,
    distribution: &'a Distribution,
    height: usize,
    failure: Mutex<Option<MoneroError>>,
}

impl RingSource<'_> {
    fn stash(&self, error: MoneroError) -> InterfaceError {
        let text = error.to_string();
        *self.failure.lock() = Some(error);
        InterfaceError::InterfaceError(text)
    }
}

impl ProvidesBlockchainMeta for RingSource<'_> {
    fn latest_block_number(&self) -> impl Send + Future<Output = Result<usize, InterfaceError>> {
        async move { Ok(self.height) }
    }
}

impl ProvidesUnvalidatedDecoys for RingSource<'_> {
    fn ringct_output_distribution(
        &self,
        range: impl Send + RangeBounds<usize>,
    ) -> impl Send + Future<Output = Result<Vec<u64>, InterfaceError>> {
        async move {
            let cumulative_outputs = &self.distribution.cumulative_outputs;
            let end = match range.end_bound() {
                Bound::Included(value) => Some(*value),
                Bound::Excluded(value) => value.checked_sub(1),
                Bound::Unbounded => None,
            };
            let block_count =
                end.and_then(|end| end.checked_add(1)).unwrap_or(cumulative_outputs.len());
            Ok(cumulative_outputs.iter().take(block_count).copied().collect())
        }
    }

    fn unlocked_ringct_outputs(
        &self,
        indexes: &[u64],
        _evaluate_unlocked: EvaluateUnlocked,
    ) -> impl Send + Future<Output = Result<Vec<Option<[Point; 2]>>, TransactionsError>> {
        async move {
            self.node
                .ring_members(self.distribution.endpoint(), indexes)
                .await
                .map_err(|error| TransactionsError::InterfaceError(self.stash(error)))
        }
    }
}

/// A ring for each candidate, against one endpoint's view of the chain.
///
/// Each ring's member positions go into `chosen` by the candidate's key image the
/// moment it's settled, so a failure on a later candidate loses none of them.
///
/// A candidate offered with a ring before gets that exact ring again, unless it no
/// longer holds on this chain, so whoever sees both learns nothing by comparing them.
/// All the members come from one node, because a ring stitched from two nodes'
/// answers tells each of them something about which member is real.
pub(crate) async fn select_rings(
    node: &dyn MoneroNode,
    distribution: &Distribution,
    candidates: &[Candidate],
    chosen: &mut BTreeMap<[u8; 32], Vec<u64>>,
) -> Result<Vec<OutputWithDecoys>, MoneroError> {
    let height = usize::try_from(distribution.height())
        .map_err(|_| MoneroError::HeightOutOfRange { height: distribution.height() })?;
    let source = RingSource { node, distribution, height, failure: Mutex::new(None) };
    let mut selected = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if let Some(positions) = &candidate.ring
            && let Some(reused) = reuse(node, distribution, &candidate.output, positions).await?
        {
            chosen.insert(candidate.key_image, positions.clone());
            selected.push(reused);
            continue;
        }
        let with_decoys =
            OutputWithDecoys::new(&mut OsRng, &source, RING_LEN, height, candidate.output.clone())
                .await
                .map_err(|error| {
                    source.failure.lock().take().unwrap_or_else(|| {
                        tracing::debug!(target: TARGET, %error, "ring selection failed");
                        MoneroError::Decoys
                    })
                })?;
        chosen.insert(candidate.key_image, with_decoys.decoys().positions());
        selected.push(with_decoys);
    }
    Ok(selected)
}

/// `output` with the ring at `positions`, or `None` when that ring no longer holds on
/// the chain `distribution` reads, as after a reorganization moved the output or a
/// member.
///
/// It holds while it has [`RING_LEN`] members that the chain had at the distribution's
/// height, `output` among them, and the node serves every member unlocked: `output`
/// exactly as the wallet scanned it, and every decoy a torsion-free point other than
/// the identity.
async fn reuse(
    node: &dyn MoneroNode,
    distribution: &Distribution,
    output: &WalletOutput,
    positions: &[u64],
) -> Result<Option<OutputWithDecoys>, MoneroError> {
    let Ok(signer) = positions.binary_search(&output.index_on_blockchain()) else {
        return Ok(None);
    };
    let is_shaped = positions.len() == usize::from(RING_LEN)
        && positions.is_sorted_by(|before, after| before < after)
        && positions.last().is_some_and(|last| *last < distribution.output_count());
    if !is_shaped {
        return Ok(None);
    }
    let members = node.ring_members(distribution.endpoint(), positions).await?;
    if members.len() != positions.len() {
        return Err(MoneroError::Decoys);
    }
    let real = [output.key(), output.commitment().commit()];
    let mut ring = Vec::with_capacity(members.len());
    for (index, member) in members.into_iter().enumerate() {
        let Some(member) = member else { return Ok(None) };
        let is_usable = if index == signer {
            member == real
        } else {
            member.iter().all(|point| {
                CompressedEdwardsY(point.compress().to_bytes())
                    .decompress()
                    .is_some_and(|point| point.is_torsion_free() && !point.is_identity())
            })
        };
        if !is_usable {
            return Ok(None);
        }
        ring.push(member);
    }
    let offsets = positions
        .iter()
        .scan(0u64, |previous, position| {
            let offset = position.saturating_sub(*previous);
            *previous = *position;
            Some(offset)
        })
        .collect();
    let Some(decoys) =
        u8::try_from(signer).ok().and_then(|signer| Decoys::new(offsets, signer, ring))
    else {
        return Ok(None);
    };
    let unassembled = || MoneroError::Internal { context: "a reused ring did not assemble" };
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        OUTPUT_DATA_BYTES.saturating_add(decoys.serialize().len()),
    ));
    bytes.extend_from_slice(&output.key().compress().to_bytes());
    output.key_offset().write(&mut *bytes).map_err(|_| unassembled())?;
    output.commitment().write(&mut *bytes).map_err(|_| unassembled())?;
    decoys.write(&mut *bytes).map_err(|_| unassembled())?;
    let mut reader = bytes.as_slice();
    let assembled = OutputWithDecoys::read(&mut reader).map_err(|_| unassembled())?;
    if !reader.is_empty() {
        return Err(unassembled());
    }
    Ok(Some(assembled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_distribution_never_prints_the_url_it_came_from() {
        let url = "https://user:secret@node.invalid:18081";
        let distribution = Distribution::new(vec![1, 2, 3], 2, url);
        let printed = format!("{distribution:?}");
        assert!(!printed.contains("user"), "{printed}");
        assert!(!printed.contains("secret"), "{printed}");
        assert_eq!(distribution.endpoint(), url, "the ring is still pinned to that node");
    }
}
