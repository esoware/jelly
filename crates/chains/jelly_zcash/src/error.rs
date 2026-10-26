use jelly_core::{Amount, AssetId, ChainError, ClockError, CoreError, KeyError};
use jelly_net::NetError;

use crate::config::{CHAIN, DUST_ZAT};
use crate::preview::zec;

#[derive(Debug, thiserror::Error)]
pub enum ZcashError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the Zcash endpoint returned inconsistent data")]
    Protocol,
    #[error("insufficient spendable Zcash")]
    Funds { needed_zat: u64, available_zat: u64 },
    #[error("the Zcash amount is below the minimum output amount")]
    Dust { amount_zat: u64 },
    #[error(
        "The fee of {} is below the ZIP 317 fee of {} for this payment.",
        zec(*fee_zat),
        zec(*minimum_zat)
    )]
    FeeBelowMinimum { fee_zat: u64, minimum_zat: u64 },
    #[error(
        "The fee can be at most {} for this payment. A higher fee confirms no sooner.",
        zec(*maximum_zat)
    )]
    FeeAboveMaximum { maximum_zat: u64 },
    #[error("Zcash key operation failed")]
    Key(#[from] KeyError),
    #[error("Zcash state storage failed")]
    Storage(#[from] CoreError),
    #[error("the system clock is unusable")]
    Clock(#[from] ClockError),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl From<ZcashError> for ChainError {
    fn from(error: ZcashError) -> Self {
        match error {
            ZcashError::Net(source) => source.into(),
            error @ (ZcashError::Invalid(_)
            | ZcashError::FeeBelowMinimum { .. }
            | ZcashError::FeeAboveMaximum { .. }) => Self::invalid_input(error),
            error @ ZcashError::Protocol => Self::rpc(error),
            ZcashError::Funds { needed_zat, available_zat } => Self::InsufficientFunds {
                asset: AssetId::Native(CHAIN),
                needed: Amount::from(needed_zat),
                available: Amount::from(available_zat),
            },
            error @ ZcashError::Dust { amount_zat } => Self::InvalidInput(
                format!(
                    "The amount of {} is below the minimum output amount of {}.",
                    zec(amount_zat),
                    zec(DUST_ZAT)
                ),
                Box::new(error),
            ),
            ZcashError::Key(source) => Self::internal("Zcash key operation failed", source),
            ZcashError::Storage(source) => Self::internal("Zcash state storage failed", source),
            ZcashError::Clock(source) => Self::internal("Zcash clock", source),
            ZcashError::Internal(context) => Self::internal_without_source(context),
        }
    }
}
