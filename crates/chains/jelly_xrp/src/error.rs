use jelly_core::{Amount, AssetId, ChainError, ClockError, CoreError, KeyError};
use jelly_net::NetError;

use crate::config::{CHAIN, MAX_FEE_DROPS};
use crate::preview::xrp;

#[derive(Debug, thiserror::Error)]
pub enum XrpError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the XRP node returned an invalid response")]
    Protocol,
    #[error("an XRP payment from this address is still pending")]
    Busy,
    #[error("insufficient XRP after the account reserve")]
    Funds { needed_drops: u64, available_drops: u64 },
    #[error(
        "The fee of {} is below the network minimum of {}.",
        xrp(*fee_drops),
        xrp(*minimum_drops)
    )]
    FeeBelowMinimum { fee_drops: u64, minimum_drops: u64 },
    #[error("The fee can be at most {}.", xrp(MAX_FEE_DROPS))]
    FeeAboveMaximum,
    #[error("XRP signing failed")]
    Key(#[from] KeyError),
    #[error("XRP state storage failed")]
    Storage(#[from] CoreError),
    #[error("the system clock is unusable")]
    Clock(#[from] ClockError),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl From<XrpError> for ChainError {
    fn from(error: XrpError) -> Self {
        match error {
            XrpError::Net(source) => source.into(),
            error @ (XrpError::Invalid(_)
            | XrpError::FeeBelowMinimum { .. }
            | XrpError::FeeAboveMaximum) => Self::invalid_input(error),
            error @ XrpError::Protocol => Self::rpc(error),
            error @ XrpError::Busy => Self::InvalidInput(
                String::from("Wait for the pending XRP payment to finish before sending again."),
                Box::new(error),
            ),
            XrpError::Funds { needed_drops, available_drops } => Self::InsufficientFunds {
                asset: AssetId::Native(CHAIN),
                needed: Amount::from(needed_drops),
                available: Amount::from(available_drops),
            },
            XrpError::Key(source) => Self::internal("XRP signing failed", source),
            XrpError::Storage(source) => Self::internal("XRP state storage failed", source),
            XrpError::Clock(source) => Self::internal("XRP clock", source),
            XrpError::Internal(context) => Self::internal_without_source(context),
        }
    }
}
