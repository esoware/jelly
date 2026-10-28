use jelly_core::{AssetId, ChainId};

pub(crate) const CHAIN: ChainId = ChainId::Solana;

pub(crate) const NATIVE: AssetId = AssetId::Native(CHAIN);

/// Tuning for one [`crate::SolanaAdapter`]. The defaults suit public mainnet
/// endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolanaConfig {
    /// Signature page size. The protocol caps it at 1000.
    pub signature_page_size: u32,
    /// Signature pages one pass may pull per address, for the forward walk and
    /// for the backfill each.
    ///
    /// Every signature a pass walks past has its transaction fetched in the same
    /// pass, so a cursor never advances over history that was not turned into
    /// records.
    pub page_budget: u32,
    /// Calls per variable-size batch, kept modest because providers impose
    /// undocumented limits. A balance batch never splits one owner's reads,
    /// and every fixed-size batch, such as the tip and the build reads,
    /// uses at most six calls.
    pub max_batch: u32,
    /// The most a fee level's priority fee may cost for one transaction. A custom
    /// fee is not clamped, because it is what the user asked for.
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
