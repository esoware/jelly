use jelly_core::{AssetId, Clock, FeeSelection, SendAmount, TxRequest};

use crate::address::{XrpDestination, xrp_address, xrp_destination};
use crate::client::{AccountRoot, Ledger, NetworkFee, XrpClient};
use crate::config::{
    CHAIN, MAX_DELETION_FEE_DROPS, MAX_FEE_DROPS, RIPPLE_EPOCH_UNIX_SECONDS, VALIDITY_LEDGERS,
};
use crate::error::XrpError;
use crate::payload::{Payment, SendKind};
use crate::state::States;
use crate::watched::Watched;

// A payment is built on the node's view of the sender, so that view must be recent, and
// the lead allows for a local clock that runs slightly behind the network.
const MAX_NODE_LAG_SECONDS: i64 = 120;
const MAX_NODE_LEAD_SECONDS: i64 = 60;

const CHOOSE_SOURCE: &str = "Choose an XRP source address.";

pub(crate) struct Inputs<'a> {
    pub(crate) client: &'a XrpClient,
    pub(crate) states: &'a States,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) watched: &'a [Watched],
}

/// A payment resolved against the validated ledger.
pub(crate) struct Resolved {
    /// The id of the address the payment sends from.
    pub(crate) source_id: i64,
    pub(crate) payment: Payment,
    /// What the normal level would pay, or `None` when the network asks more than a
    /// level may pay.
    pub(crate) normal_fee_drops: Option<u64>,
}

/// The fee `selection` names in drops, or `None` for a level, which pays the network's
/// own. Another chain's custom fee is refused rather than ignored.
fn chosen_fee_drops(selection: &FeeSelection) -> Result<Option<u64>, XrpError> {
    match *selection {
        FeeSelection::Level { .. } => Ok(None),
        FeeSelection::Total { fee } => {
            u64::try_from(fee.base_units()).map(Some).map_err(|_| XrpError::FeeAboveMaximum)
        }
        FeeSelection::Rate { .. } | FeeSelection::Evm { .. } | FeeSelection::Solana { .. } => {
            Err(XrpError::Invalid("Choose an XRP fee level or a fee in XRP."))
        }
    }
}

/// What a payment pays. A level pays the cost of the open ledger, and a custom fee has
/// to be enough to be queued for a later one.
const fn payment_fee_drops(
    network: NetworkFee,
    chosen_drops: Option<u64>,
) -> Result<u64, XrpError> {
    match chosen_drops {
        None if network.normal_drops > MAX_FEE_DROPS => {
            Err(XrpError::Invalid("The XRP network fee is unusually high. Try again later."))
        }
        None => Ok(network.normal_drops),
        Some(fee_drops) if fee_drops < network.minimum_drops => {
            Err(XrpError::FeeBelowMinimum { fee_drops, minimum_drops: network.minimum_drops })
        }
        Some(fee_drops) if fee_drops > MAX_FEE_DROPS => Err(XrpError::FeeAboveMaximum),
        Some(fee_drops) => Ok(fee_drops),
    }
}

/// The payment `request` asks for, built on the validated ledger. Quotes and builds share
/// it, so the user approves the numbers the payment is built with.
pub(crate) async fn resolve(
    inputs: &Inputs<'_>,
    request: &TxRequest,
) -> Result<Resolved, XrpError> {
    let Inputs { client, states, clock, watched } = *inputs;
    states.ensure_readable()?;
    if request.asset != AssetId::Native(CHAIN) {
        return Err(XrpError::Invalid("Choose XRP."));
    }
    let chosen_drops = chosen_fee_drops(&request.fee)?;
    let source_id = request.sole_source().ok_or(XrpError::Invalid(CHOOSE_SOURCE))?;
    let from = watched
        .iter()
        .find(|entry| entry.address_id == source_id)
        .map(|entry| entry.address.clone())
        .ok_or(XrpError::Invalid(CHOOSE_SOURCE))?;
    let XrpDestination { account: destination, tag: embedded_tag } = xrp_destination(&request.to)
        .map_err(|_| {
        XrpError::Invalid("Enter a valid XRP mainnet address or payment request.")
    })?;
    if let (Some(embedded), Some(explicit)) = (embedded_tag, request.destination_tag)
        && embedded != explicit
    {
        return Err(XrpError::Invalid("The address and destination tag do not match."));
    }
    let destination_tag = embedded_tag.or(request.destination_tag);
    let to = xrp_address(&destination);
    if from == to {
        return Err(XrpError::Invalid("Choose a different XRP recipient address."));
    }
    let ledger = client.ledger().await?;
    let close_unix_secs =
        ledger.close_time.checked_add(RIPPLE_EPOCH_UNIX_SECONDS).ok_or(XrpError::Protocol)?;
    let now_unix_secs = clock.now_secs()?;
    if now_unix_secs.saturating_sub(close_unix_secs) > MAX_NODE_LAG_SECONDS
        || close_unix_secs.saturating_sub(now_unix_secs) > MAX_NODE_LEAD_SECONDS
    {
        return Err(XrpError::Invalid("The XRP node is not up to date. Try again shortly."));
    }
    if states.is_held(source_id) {
        return Err(XrpError::Busy);
    }
    let kind = send_kind(request)?;
    let (sender, recipient) =
        tokio::try_join!(client.account(&from, &ledger), client.account(&to, &ledger))?;
    let sender = sender.ok_or(XrpError::Invalid("Fund this XRP account before sending."))?;
    if sender.has_disabled_master_key() {
        return Err(XrpError::Invalid(
            "This XRP account has disabled its master key and cannot send from Jelly.",
        ));
    }
    if let Some(recipient) = &recipient {
        check_recipient(client, &from, recipient, destination_tag, &ledger).await?;
    }
    let (fee_drops, normal_fee_drops, reserve_drops) = match kind {
        SendKind::Payment => {
            let network = client.network_fee().await?;
            let fee_drops = payment_fee_drops(network, chosen_drops)?;
            let normal_drops = Some(network.normal_drops).filter(|drops| *drops <= MAX_FEE_DROPS);
            (fee_drops, normal_drops, sender.reserve_drops(&ledger)?)
        }
        SendKind::AccountDelete => {
            check_deletable(client, &from, &sender, recipient.is_some(), &ledger).await?;
            (ledger.reserve_inc, Some(ledger.reserve_inc), 0)
        }
    };
    let available_drops = sender.balance_drops()?.saturating_sub(reserve_drops);
    let amount_drops = match request.amount {
        SendAmount::Exact { value } => u64::try_from(value.base_units())
            .map_err(|_| XrpError::Invalid("The XRP amount is too large."))?,
        SendAmount::Max => available_drops.saturating_sub(fee_drops),
    };
    let needed_drops = amount_drops
        .checked_add(fee_drops)
        .ok_or(XrpError::Invalid("The XRP amount is too large."))?;
    if needed_drops > available_drops || amount_drops == 0 {
        return Err(XrpError::Funds {
            needed_drops: needed_drops.max(fee_drops.saturating_add(1)),
            available_drops,
        });
    }
    if recipient.is_none() && amount_drops < ledger.reserve_base {
        return Err(XrpError::Invalid(
            "The payment is below the reserve needed to activate the recipient's XRP account.",
        ));
    }
    let payment = Payment {
        kind,
        from,
        to,
        destination_tag,
        amount_drops,
        fee_drops,
        sequence: sender.sequence,
        first_ledger: ledger.seq.checked_add(1).ok_or(XrpError::Protocol)?,
        last_ledger: ledger.seq.checked_add(VALIDITY_LEDGERS).ok_or(XrpError::Protocol)?,
        reserve_drops,
        base_reserve_drops: ledger.reserve_base,
        is_activation: recipient.is_none(),
    };
    Ok(Resolved { source_id, payment, normal_fee_drops })
}

/// The transaction type `request` asks for. A deletion pays out everything the account
/// holds, so it takes no other amount, and pays the owner reserve as its fee, so it
/// takes no other fee.
fn send_kind(request: &TxRequest) -> Result<SendKind, XrpError> {
    if !request.should_delete_account {
        return Ok(SendKind::Payment);
    }
    if request.amount != SendAmount::Max {
        return Err(XrpError::Invalid(
            "An account deletion sends the whole balance. Choose the maximum amount.",
        ));
    }
    if !matches!(request.fee, FeeSelection::Level { .. }) {
        return Err(XrpError::Invalid(
            "An account deletion pays the owner reserve as its fee, so it takes no custom fee.",
        ));
    }
    Ok(SendKind::AccountDelete)
}

/// Refuses what the recipient's account would refuse: a payment without the tag it
/// requires, or one it has not authorized deposits from.
async fn check_recipient(
    client: &XrpClient,
    from: &str,
    recipient: &AccountRoot,
    destination_tag: Option<u32>,
    ledger: &Ledger,
) -> Result<(), XrpError> {
    if recipient.is_destination_tag_required() && destination_tag.is_none() {
        return Err(XrpError::Invalid("This recipient requires a destination tag."));
    }
    if recipient.is_deposit_auth_required()
        && !client.is_deposit_authorized(from, recipient, ledger).await?
    {
        return Err(XrpError::Invalid(
            "This recipient has not authorized deposits from your XRP account.",
        ));
    }
    Ok(())
}

/// Refuses a deletion rippled would apply only to charge its fee, which for an
/// `AccountDelete` is the whole owner reserve.
async fn check_deletable(
    client: &XrpClient,
    from: &str,
    sender: &AccountRoot,
    has_recipient: bool,
    ledger: &Ledger,
) -> Result<(), XrpError> {
    if !has_recipient {
        return Err(XrpError::Invalid(
            "An account deletion can only pay an XRP account that already exists.",
        ));
    }
    if sender.is_too_new_to_delete(ledger) {
        return Err(XrpError::Invalid(
            "This XRP account is too new to delete. Try again in about 20 minutes.",
        ));
    }
    if ledger.reserve_inc == 0 {
        return Err(XrpError::Protocol);
    }
    if ledger.reserve_inc > MAX_DELETION_FEE_DROPS {
        return Err(XrpError::Invalid(
            "The XRP account deletion fee is unusually high. Try again later.",
        ));
    }
    if client.has_deletion_blockers(from, ledger).await? {
        return Err(XrpError::Invalid(
            "This XRP account holds trust lines, escrows, payment channels, checks, or other objects that must be removed before it can be deleted.",
        ));
    }
    Ok(())
}
