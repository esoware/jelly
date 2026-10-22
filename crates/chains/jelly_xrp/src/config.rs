use jelly_core::ChainId;

pub(crate) const CHAIN: ChainId = ChainId::Xrp;
pub(crate) const MAX_DROPS: u64 = 100_000_000_000_000_000;
pub(crate) const MAX_FEE_DROPS: u64 = 100_000;
// An account deletion pays the owner reserve as its fee. The network has never set that
// reserve above 5 XRP, so a server quoting more is not believed.
pub(crate) const MAX_DELETION_FEE_DROPS: u64 = 5_000_000;
pub(crate) const RIPPLE_EPOCH_UNIX_SECONDS: i64 = 946_684_800;
// About a minute and a half of ledgers, after which the payment can no longer be included
// and its expiry can be proven.
pub(crate) const VALIDITY_LEDGERS: u32 = 20;
