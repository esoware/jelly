use jelly_core::{Amount, AssetId, ChainError, ClockError, CoreError, KeyError};
use jelly_net::NetError;
use tonlib_core::cell::TonCellError;
use tonlib_core::message::TonMessageError;

#[derive(Debug, thiserror::Error)]
pub enum TonError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the TON node returned an invalid response")]
    Protocol,
    #[error("Wait for the pending TON payment to finish before sending again.")]
    Busy,
    #[error("insufficient funds for the payment and network fee")]
    Funds { asset: AssetId, needed: Amount, available: Amount },
    #[error("TON key operation failed")]
    Key(#[from] KeyError),
    #[error("TON state storage failed")]
    Storage(#[from] CoreError),
    #[error("the system clock is unusable")]
    Clock(#[from] ClockError),
    #[error("invalid TON cell encoding")]
    Cell(#[from] TonCellError),
    #[error("invalid TON message encoding")]
    Message(#[from] TonMessageError),
    #[error("internal: {0}")]
    Internal(&'static str),
}

impl From<TonError> for ChainError {
    fn from(error: TonError) -> Self {
        match error {
            TonError::Net(source) => source.into(),
            error @ (TonError::Invalid(_) | TonError::Busy) => Self::invalid_input(error),
            error @ (TonError::Protocol | TonError::Cell(_) | TonError::Message(_)) => {
                Self::rpc(error)
            }
            TonError::Funds { asset, needed, available } => {
                Self::InsufficientFunds { asset, needed, available }
            }
            TonError::Key(source) => Self::internal("TON key operation failed", source),
            TonError::Storage(source) => Self::internal("TON state storage failed", source),
            TonError::Clock(source) => Self::internal("TON clock", source),
            TonError::Internal(context) => Self::internal_without_source(context),
        }
    }
}
