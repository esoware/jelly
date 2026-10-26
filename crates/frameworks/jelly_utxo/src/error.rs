use bitcoin::{OutPoint, Txid, Weight};
use jelly_core::{
    AddressError, Amount, AssetId, ChainError, ChainId, CoreError, DecimalError, EndpointLabel,
    FailureKind, FeeRate, KeyError, Rejection,
};
use jelly_electrum::ElectrumError;
use jelly_net::{NetError, Outcome};

/// No arithmetic wraps: a step whose overflow is an error reports
/// [`UtxoError::Overflow`], and a step that clamps saturates. No variant carries
/// a secret, only public facts about a transaction.
///
/// Electrum and network failures are carried whole rather than flattened into a
/// string, so the pool's classifier and the domain error read a value. Every
/// endpoint inside is an [`EndpointLabel`], so a `Debug` print is safe to log.
#[derive(Debug, thiserror::Error)]
pub enum UtxoError {
    #[error("insufficient funds: need {needed}, have {available}")]
    InsufficientFunds { needed: Amount, available: Amount },
    #[error("{context} overflowed")]
    Overflow { context: &'static str },
    #[error(
        "The amount of {} exceeds the network's limit of {} in one transaction.",
        chain.format_amount(*value),
        chain.format_amount(*maximum)
    )]
    AboveMaxMoney { value: Amount, maximum: Amount, chain: ChainId },
    #[error(
        "The output of {} is below the network minimum of {}.",
        chain.format_amount(*value),
        chain.format_amount(*threshold)
    )]
    Dust { value: Amount, threshold: Amount, chain: ChainId },
    #[error(
        "The fee rate of {} {unit} is below the network minimum of {} {unit}.",
        rate.format_sat_per_vb(),
        floor.format_sat_per_vb()
    )]
    BelowRelayFloor { rate: FeeRate, floor: FeeRate, unit: &'static str },
    #[error(
        "The fee of {} exceeds Jelly's limit of {}. Choose a lower fee rate.",
        chain.format_amount(*fee),
        chain.format_amount(*ceiling)
    )]
    FeeCeiling { fee: Amount, ceiling: Amount, chain: ChainId },
    #[error(
        "The transaction weighs {} weight units. The network limit is {} weight units.",
        weight.to_wu(),
        maximum.to_wu()
    )]
    TooHeavy { weight: Weight, maximum: Weight },
    /// [`UtxoError::TooHeavy`] on a chain without segregated witness, whose limit
    /// is a size in bytes.
    #[error("The transaction is {size_bytes} bytes. The network limit is {maximum_bytes} bytes.")]
    TooLarge { size_bytes: u64, maximum_bytes: u64 },
    #[error("{} sends cannot be sped up.", chain.display_name())]
    NoBumpMethod { chain: ChainId },
    #[error("Transaction {txid} did not enable fee replacement.")]
    NotReplaceable { txid: Txid },
    #[error(
        "Transaction {txid} already pays {} {unit}. Choose a higher fee rate.",
        old.format_sat_per_vb()
    )]
    RateNotIncreased { txid: Txid, old: FeeRate, unit: &'static str },
    #[error("The transaction has no output for the saved recipient.")]
    RecipientNotFound,
    #[error("Transaction {txid} has no change output, so no second transaction can pay for it.")]
    NoChange { txid: Txid },
    #[error(
        "The change of transaction {txid} is already spent. Speed up the transaction that spends it instead."
    )]
    ChangeSpent { txid: Txid },
    #[error(
        "Transaction {txid} waits on too many unconfirmed transactions for another to be relayed after it."
    )]
    AncestorLimit { txid: Txid },
    #[error("Output {outpoint} is reserved by another transaction awaiting broadcast.")]
    OutpointClaimed { outpoint: OutPoint },
    #[error("Jelly cannot spend this output type.")]
    UnspendableScript,
    #[error("the signing key belongs to a different address")]
    WrongKey,
    #[error("input {index} has no known previous output")]
    MissingPrevout { index: usize },
    #[error("the unsigned transaction payload is malformed: {context}")]
    Payload { context: &'static str },
    /// A transaction a server served pays out more than the outputs it spends
    /// hold, so it has no fee a replacement could be measured against.
    #[error("{txid} spends more than its inputs hold")]
    OriginalOverspends { txid: Txid },
    #[error("{context}")]
    Signing { context: &'static str },
    #[error("{height} is not a block height a locktime can name")]
    InvalidTip { height: u32 },
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Electrum(#[from] ElectrumError),
    #[error(transparent)]
    Net(NetError),
    /// Named at every call site rather than derived, so no other failure that
    /// happens to be a [`CoreError`] can be reported as a storage one.
    #[error("the cursor store failed")]
    Cursor(#[source] CoreError),
    #[error("stored UTXO state is unreadable")]
    UnreadableState,
    /// The server answered and the answer does not add up. Anything the engine
    /// refuses about data that arrived from an endpoint lands here, so it is
    /// attributed to that endpoint and the walk rotates.
    #[error("{endpoint}: the answer cannot be used: {source}")]
    Server {
        endpoint: EndpointLabel,
        #[source]
        source: Box<Self>,
    },
    /// The server has no such transaction. Unreachable while the Electrum client
    /// refuses a short batch, and still an arm rather than an assumption.
    #[error("the server does not have {txid}")]
    UnknownTx { txid: Txid },
    /// An address form the chain's table refuses by name, such as a Litecoin MWEB
    /// address. There is no output script to pay and no scripthash to watch, so
    /// this is a refusal rather than a limitation of this build, and the table's
    /// message reaches the user as written.
    #[error("{message}")]
    RefusedAddress { message: &'static str },
    #[error("{} does not belong to {}", asset.key(), chain.display_name())]
    ForeignAsset { asset: AssetId, chain: ChainId },
    #[error(
        "This is a {} wallet operation, not a {} operation.",
        expected.display_name(),
        found.display_name()
    )]
    ForeignChain { expected: ChainId, found: ChainId },
    #[error("This address is no longer available in this wallet.")]
    NotWatched { address_id: i64 },
    #[error("Choose an address to send from.")]
    NoSource,
    #[error("Choose a {} fee rate.", chain.display_name())]
    ForeignFeeSelection { chain: ChainId },
    #[error("Enter a fee rate in {unit}.")]
    FeeRate {
        unit: &'static str,
        #[source]
        source: DecimalError,
    },
    #[error("Enter a 64-digit hexadecimal transaction ID.")]
    NotATxid,
    #[error("those bytes do not decode as a transaction")]
    NotATransaction,
    /// None of the transaction's inputs spends from a watched address, so there is
    /// no address to fund a replacement and no key to sign it.
    #[error("Transaction {txid} does not spend from this wallet.")]
    NotOurs { txid: Txid },
    /// The last pass saw the transaction in a block, so the network could only
    /// refuse a replacement, and refusing here saves the user approving one that
    /// cannot land.
    #[error("Transaction {txid} is already confirmed.")]
    AlreadyConfirmed { txid: Txid },
    /// A replacement's recipient output pays a script with no address form, so
    /// the planner cannot be told who to keep paying.
    #[error("{txid} pays a script this build cannot name an address for")]
    UnnameableRecipient { txid: Txid },
    /// A mempool transaction a replacement would evict has no fee the last pass
    /// could work out, so BIP-125 rule 3 has no floor to name.
    #[error("the fee of {txid}, which a replacement would evict, is unknown")]
    UnpricedDescendant { txid: Txid },
}

impl UtxoError {
    pub(crate) const fn overflow(context: &'static str) -> Self {
        Self::Overflow { context }
    }

    pub(crate) const fn payload(context: &'static str) -> Self {
        Self::Payload { context }
    }

    pub(crate) const fn signing(context: &'static str) -> Self {
        Self::Signing { context }
    }

    /// Recovers a failure of ours from a `NetError` rather than wrapping it twice,
    /// and recovers an Electrum failure raised inside the connector too. It is a
    /// named function rather than a `From` so every site that reclassifies is
    /// greppable.
    pub(crate) fn from_net(error: NetError) -> Self {
        match error.into_source_of::<Self>() {
            Ok(inner) => inner,
            Err(error) => match error.into_source_of::<ElectrumError>() {
                Ok(inner) => Self::Electrum(inner),
                Err(error) => Self::Net(error),
            },
        }
    }

    /// A failure about data an endpoint served, labelled with that endpoint. An
    /// Electrum failure stays itself, so an endpoint failure has one
    /// representation, and anything else becomes [`Self::Server`].
    pub(crate) fn from_server(endpoint: &EndpointLabel, error: Self) -> Self {
        if let Self::Electrum(inner) = error {
            return Self::Electrum(inner);
        }
        Self::Server { endpoint: endpoint.clone(), source: Box::new(error) }
    }

    const fn label(&self) -> Option<&EndpointLabel> {
        match *self {
            Self::Electrum(ref inner) => inner.label(),
            Self::Net(ref inner) => inner.label(),
            Self::Server { ref endpoint, .. } => Some(endpoint),
            Self::InsufficientFunds { .. }
            | Self::Overflow { .. }
            | Self::AboveMaxMoney { .. }
            | Self::Dust { .. }
            | Self::BelowRelayFloor { .. }
            | Self::FeeCeiling { .. }
            | Self::TooHeavy { .. }
            | Self::TooLarge { .. }
            | Self::NoBumpMethod { .. }
            | Self::NotReplaceable { .. }
            | Self::RateNotIncreased { .. }
            | Self::RecipientNotFound
            | Self::NoChange { .. }
            | Self::ChangeSpent { .. }
            | Self::AncestorLimit { .. }
            | Self::OutpointClaimed { .. }
            | Self::UnspendableScript
            | Self::WrongKey
            | Self::MissingPrevout { .. }
            | Self::Payload { .. }
            | Self::OriginalOverspends { .. }
            | Self::Signing { .. }
            | Self::InvalidTip { .. }
            | Self::Address(_)
            | Self::Key(_)
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::UnknownTx { .. }
            | Self::RefusedAddress { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignChain { .. }
            | Self::NotWatched { .. }
            | Self::NoSource
            | Self::ForeignFeeSelection { .. }
            | Self::FeeRate { .. }
            | Self::NotATxid
            | Self::NotATransaction
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::UnnameableRecipient { .. }
            | Self::UnpricedDescendant { .. } => None,
        }
    }

    /// The class the health tracker records. Nothing re-derives it from a
    /// message.
    const fn kind(&self) -> FailureKind {
        match *self {
            Self::Electrum(ref inner) => inner.kind(),
            Self::Net(ref inner) => inner.kind(),
            Self::Server { .. } | Self::UnknownTx { .. } | Self::OriginalOverspends { .. } => {
                FailureKind::Protocol
            }
            Self::InsufficientFunds { .. }
            | Self::Overflow { .. }
            | Self::AboveMaxMoney { .. }
            | Self::Dust { .. }
            | Self::BelowRelayFloor { .. }
            | Self::FeeCeiling { .. }
            | Self::TooHeavy { .. }
            | Self::TooLarge { .. }
            | Self::NoBumpMethod { .. }
            | Self::NotReplaceable { .. }
            | Self::RateNotIncreased { .. }
            | Self::RecipientNotFound
            | Self::NoChange { .. }
            | Self::ChangeSpent { .. }
            | Self::AncestorLimit { .. }
            | Self::OutpointClaimed { .. }
            | Self::UnspendableScript
            | Self::WrongKey
            | Self::MissingPrevout { .. }
            | Self::Payload { .. }
            | Self::Signing { .. }
            | Self::InvalidTip { .. }
            | Self::Address(_)
            | Self::Key(_)
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::RefusedAddress { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignChain { .. }
            | Self::NotWatched { .. }
            | Self::NoSource
            | Self::ForeignFeeSelection { .. }
            | Self::FeeRate { .. }
            | Self::NotATxid
            | Self::NotATransaction
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::UnnameableRecipient { .. }
            | Self::UnpricedDescendant { .. } => FailureKind::Client,
        }
    }

    /// What the failover walk does with this failure.
    ///
    /// A settled refusal and a request this build will not make both stop the
    /// walk, and anything about one server's answer rotates.
    const fn outcome(&self) -> Outcome {
        match *self {
            Self::Electrum(ref inner) => inner.outcome(),
            Self::Net(_)
            | Self::Server { .. }
            | Self::UnknownTx { .. }
            | Self::OriginalOverspends { .. } => Outcome::Rotate,
            Self::InsufficientFunds { .. }
            | Self::Overflow { .. }
            | Self::AboveMaxMoney { .. }
            | Self::Dust { .. }
            | Self::BelowRelayFloor { .. }
            | Self::FeeCeiling { .. }
            | Self::TooHeavy { .. }
            | Self::TooLarge { .. }
            | Self::NoBumpMethod { .. }
            | Self::NotReplaceable { .. }
            | Self::RateNotIncreased { .. }
            | Self::RecipientNotFound
            | Self::NoChange { .. }
            | Self::ChangeSpent { .. }
            | Self::AncestorLimit { .. }
            | Self::OutpointClaimed { .. }
            | Self::UnspendableScript
            | Self::WrongKey
            | Self::MissingPrevout { .. }
            | Self::Payload { .. }
            | Self::Signing { .. }
            | Self::InvalidTip { .. }
            | Self::Address(_)
            | Self::Key(_)
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::RefusedAddress { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignChain { .. }
            | Self::NotWatched { .. }
            | Self::NoSource
            | Self::ForeignFeeSelection { .. }
            | Self::FeeRate { .. }
            | Self::NotATxid
            | Self::NotATransaction
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::UnnameableRecipient { .. }
            | Self::UnpricedDescendant { .. } => Outcome::Fail,
        }
    }

    /// The node's classified refusal of a broadcast, when this is one.
    pub(crate) const fn rejection(&self) -> Option<&Rejection> {
        if let Self::Electrum(ref inner) = *self { inner.rejection() } else { None }
    }

    /// `chain` comes from the caller because the engine is identical across
    /// Electrum coins.
    ///
    /// The match is exhaustive so a new variant fails to compile until someone
    /// decides how the interface treats it.
    pub(crate) fn into_chain_error(self, chain: ChainId) -> ChainError {
        match self {
            Self::InsufficientFunds { needed, available } => {
                ChainError::InsufficientFunds { asset: AssetId::Native(chain), needed, available }
            }
            Self::Electrum(inner) => inner.into_chain_error(chain),
            Self::Net(inner) => inner.into(),
            Self::Cursor(source) => ChainError::internal("utxo cursor storage", source),
            error @ Self::UnreadableState => ChainError::internal("utxo stored state", error),
            Self::Address(inner) => ChainError::invalid_address(inner),
            Self::Key(inner) => ChainError::signing(inner),
            error @ (Self::WrongKey
            | Self::Signing { .. }
            | Self::Payload { .. }
            | Self::MissingPrevout { .. }
            | Self::NotATransaction) => ChainError::signing(error),
            error @ Self::Overflow { .. } => ChainError::internal("utxo arithmetic", error),
            error @ (Self::InvalidTip { .. }
            | Self::Server { .. }
            | Self::UnknownTx { .. }
            | Self::OriginalOverspends { .. }
            | Self::UnpricedDescendant { .. }) => ChainError::rpc(error),
            error @ (Self::NoBumpMethod { .. }
            | Self::ForeignAsset { .. }
            | Self::UnnameableRecipient { .. }) => ChainError::unsupported(error),
            error @ (Self::RefusedAddress { .. }
            | Self::AboveMaxMoney { .. }
            | Self::Dust { .. }
            | Self::BelowRelayFloor { .. }
            | Self::FeeCeiling { .. }
            | Self::TooHeavy { .. }
            | Self::TooLarge { .. }
            | Self::NotReplaceable { .. }
            | Self::RateNotIncreased { .. }
            | Self::RecipientNotFound
            | Self::NoChange { .. }
            | Self::ChangeSpent { .. }
            | Self::AncestorLimit { .. }
            | Self::OutpointClaimed { .. }
            | Self::UnspendableScript
            | Self::ForeignChain { .. }
            | Self::NotWatched { .. }
            | Self::NoSource
            | Self::ForeignFeeSelection { .. }
            | Self::FeeRate { .. }
            | Self::NotATxid
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }) => ChainError::invalid_input(error),
        }
    }
}

/// Puts a failure into the shape the pool classifies, keeping the original as
/// the source so the typed value survives the round trip.
impl From<UtxoError> for NetError {
    fn from(error: UtxoError) -> Self {
        if let UtxoError::Net(net) = error {
            return net;
        }
        let Some(endpoint) = error.label().cloned() else {
            return Self::config("a utxo failure with no endpoint", error);
        };
        let kind = error.kind();
        Self::endpoint(&endpoint, kind, error)
    }
}

/// The classifier every read is walked with.
pub(crate) fn classify(error: &NetError) -> Outcome {
    error
        .source_of::<UtxoError>()
        .map_or_else(|| jelly_electrum::classify(error), UtxoError::outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn units(value: u128) -> Amount {
        Amount::from_base_units(value)
    }

    #[test]
    fn money_in_a_refusal_is_shown_in_the_chains_coin_rather_than_base_units() {
        let cases = [
            (
                UtxoError::Dust {
                    value: units(100),
                    threshold: units(294),
                    chain: ChainId::Bitcoin,
                },
                "The output of 0.000001 BTC is below the network minimum of 0.00000294 BTC.",
            ),
            (
                UtxoError::FeeCeiling {
                    fee: units(2_000_001),
                    ceiling: units(2_000_000),
                    chain: ChainId::Litecoin,
                },
                "The fee of 0.02000001 LTC exceeds Jelly's limit of 0.02 LTC. Choose a lower fee rate.",
            ),
            (
                UtxoError::AboveMaxMoney {
                    value: units(2_100_000_000_000_001),
                    maximum: units(2_100_000_000_000_000),
                    chain: ChainId::Bitcoin,
                },
                "The amount of 21000000.00000001 BTC exceeds the network's limit of 21000000 BTC in one transaction.",
            ),
        ];
        for (error, copy) in cases {
            assert_eq!(error.to_string(), copy);
            assert!(
                matches!(error.into_chain_error(ChainId::Bitcoin), ChainError::InvalidInput(..)),
                "{copy}"
            );
        }
    }
}
