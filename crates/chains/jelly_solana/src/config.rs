use jelly_core::{AssetId, ChainId};

pub(crate) const CHAIN: ChainId = ChainId::Solana;

pub(crate) const NATIVE: AssetId = AssetId::Native(CHAIN);

/// Tuning for one [`crate::SolanaAdapter`]. The defaults suit public mainnet
/// endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolanaConfig {
    /// Signatures per page. The protocol caps it at 1000.
    pub signature_page_size: u32,
    /// Signature pages one pass may pull for each walked account, for its forward walk
    /// and its backfill separately.
    ///
    /// A pass fetches each walked signature's transaction, one request apiece, before
    /// the cursor moves past it, so this bounds the work a pass takes on.
    pub page_budget: u32,
    /// Calls per variable-size batch, kept modest because providers have undocumented
    /// limits. A balance batch never splits one owner's reads, and fixed-size batches
    /// such as the tip and the build reads use at most six calls.
    pub max_batch: u32,
    /// The most a fee level's priority fee may cost for one transaction. A custom fee
    /// isn't held to this, because the user chose it.
    pub max_level_priority_fee_lamports: u64,
}

impl Default for SolanaConfig {
    fn default() -> Self {
        Self {
            signature_page_size: 100,
            page_budget: 3,
            max_batch: 26,
            max_level_priority_fee_lamports: 10_000_000,
        }
    }
}

impl SolanaConfig {
    /// The protocol's own ceiling on a signature page.
    const MAX_SIGNATURE_PAGE: u32 = 1_000;

    pub(crate) const fn page_size(&self) -> u32 {
        if self.signature_page_size == 0 {
            1
        } else if self.signature_page_size > Self::MAX_SIGNATURE_PAGE {
            Self::MAX_SIGNATURE_PAGE
        } else {
            self.signature_page_size
        }
    }

    pub(crate) const fn pages_per_pass(&self) -> u32 {
        if self.page_budget == 0 { 1 } else { self.page_budget }
    }

    pub(crate) fn batch_size(&self) -> usize {
        usize::try_from(self.max_batch.max(1)).unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_budget_is_clamped_into_the_range_the_protocol_allows() {
        let zeroed = SolanaConfig {
            signature_page_size: 0,
            page_budget: 0,
            max_batch: 0,
            ..SolanaConfig::default()
        };
        assert_eq!(zeroed.page_size(), 1);
        assert_eq!(zeroed.pages_per_pass(), 1);
        assert_eq!(zeroed.batch_size(), 1);

        let huge = SolanaConfig { signature_page_size: u32::MAX, ..SolanaConfig::default() };
        assert_eq!(huge.page_size(), SolanaConfig::MAX_SIGNATURE_PAGE);
    }
}
