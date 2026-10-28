use serde::{Deserialize, Serialize};

use crate::amount::Amount;
use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How soon a fee is priced to confirm. Every chain reads each level from its own
    /// market when the transaction is built, so a level names a speed and never a price.
    pub enum FeeLevel {
        Slow => "slow",
        Normal => "normal",
        Fast => "fast",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// What a custom fee names on a chain, and so which [`FeeSelection`] carries it.
    pub enum CustomFee {
        /// A rate in the chain's fee rate unit, carried by [`FeeSelection::Rate`].
        Rate => "rate",
        /// An EIP-1559 fee cap, tip, and gas limit, carried by [`FeeSelection::Evm`].
        FeeCap => "fee_cap",
        /// A gas price and gas limit, carried by [`FeeSelection::Evm`] with the cap and
        /// the tip both at the gas price, for a network whose base fee is zero.
        GasPrice => "gas_price",
        /// A priority fee in lamports, carried by [`FeeSelection::Solana`].
        PriorityFee => "priority_fee",
        /// The whole fee in the chain's coin, carried by [`FeeSelection::Total`].
        Total => "total",
    }
}

/// One of the chain's fee levels, or a custom fee in the chain's own unit.
///
/// Every chain takes a level, and a custom fee only in the shape its spec's
/// `custom_fee` names. Fee amounts cross IPC as strings, never floats.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeeSelection {
    /// Priced from the market when the transaction is built. A chain whose fee the
    /// protocol fixes pays that fee at every level.
    #[serde(rename_all = "camelCase")]
    Level { level: FeeLevel },
    /// A rate in the chain's fee rate unit, as decimal text.
    #[serde(rename_all = "camelCase")]
    Rate { rate: String },
    /// An absent field takes the normal level's value, and an absent gas limit is
    /// estimated. The fields are integers, written in decimal or as `0x`-prefixed hex.
    #[serde(rename_all = "camelCase")]
    Evm {
        /// The most gas the transaction may use.
        gas_limit: Option<String>,
        /// The most wei per gas the transaction pays, base fee and tip together.
        max_fee_per_gas: Option<String>,
        /// The tip in wei per gas.
        max_priority_fee_per_gas: Option<String>,
    },
    /// The whole priority fee in lamports, spread over the measured compute units.
    #[serde(rename_all = "camelCase")]
    Solana { priority_fee_lamports: String },
    /// The whole fee in the base units of the chain's coin.
    #[serde(rename_all = "camelCase")]
    Total { fee: Amount },
}

impl FeeSelection {
    /// What a send pays unless the user picks otherwise.
    pub const NORMAL: Self = Self::Level { level: FeeLevel::Normal };
}

/// What a transaction would pay at the selection it was quoted with.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeQuote {
    pub fee: Amount,
    /// The quoted price as a custom fee, with every market read filled in, for a custom fee
    /// to start from. Absent on a chain without custom fees.
    pub custom: Option<FeeSelection>,
    /// What a send-max moves at this fee. Present only when the request is a send-max.
    pub max_amount: Option<Amount>,
    /// What the same transaction would pay at each of the chain's fee levels, slowest
    /// first. Empty on a chain whose fee is fixed, and when the market couldn't be read.
    pub levels: Vec<LevelFee>,
}

/// What one fee level would pay.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelFee {
    pub level: FeeLevel,
    pub fee: Amount,
}
