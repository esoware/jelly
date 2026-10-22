use jelly_core::{
    Amount, AssetId, Clock, ClockError, FeeSelection, SendAmount, TokenId, TxRequest, random_bytes,
};
use tonlib_core::types::TonAddress;

use crate::address::{TonDestination, non_bounceable, ton_destination};
use crate::client::{AccountState, TonClient};
use crate::config::{CHAIN, MAX_COMMENT_BYTES};
use crate::error::TonError;
use crate::jetton::{Jetton, asset_jetton, master};
use crate::payload::{Payment, Transfer};
use crate::state::States;
use crate::watched::Watched;

// A payment is built on the node's view of the sender, so that view must be recent, and
// the lead allows for a local clock that runs slightly behind the network.
const MAX_NODE_LAG_SECONDS: i64 = 120;
const MAX_NODE_LEAD_SECONDS: i64 = 60;
const VALIDITY_SECONDS: u32 = 300;

const CHOOSE_SOURCE: &str = "Choose a TON source address.";

pub(crate) struct Inputs<'a> {
    pub(crate) client: &'a TonClient,
    pub(crate) states: &'a States,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) watched: &'a [Watched],
}

/// What every payment from one sender starts from.
struct Sender<'a> {
    watched: &'a Watched,
    balance_nanogram: u64,
    before_lt: u64,
    seqno: u32,
    is_deploy: bool,
    valid_until: u32,
    tip: u64,
}

impl Sender<'_> {
    fn payment(&self, to: String, is_max: bool, transfer: Transfer) -> Payment {
        Payment {
            from: non_bounceable(&self.watched.address),
            to,
            pubkey: self.watched.pubkey,
            version: self.watched.version,
            fee_nanogram: 1,
            seqno: self.seqno,
            before_lt: self.before_lt,
            valid_until: self.valid_until,
            is_deploy: self.is_deploy,
            is_max,
            transfer,
        }
    }
}

/// The payment `request` asks for, built on the node's latest block, with the id of the
/// address it sends from. Quotes and builds share it, so the user approves the numbers
/// the payment is built with.
pub(crate) async fn resolve(
    inputs: &Inputs<'_>,
    request: &TxRequest,
) -> Result<(i64, Payment), TonError> {
    let Inputs { client, states, clock, watched } = *inputs;
    states.ensure_readable()?;
    if request.destination_tag.is_some() {
        return Err(TonError::Invalid("Only XRP payments use a destination tag."));
    }
    if !matches!(request.fee, FeeSelection::Level { .. }) {
        return Err(TonError::Invalid("TON takes no custom fee."));
    }
    let jetton = asset_jetton(request.asset)?;
    let source_id = request.sole_source().ok_or(TonError::Invalid(CHOOSE_SOURCE))?;
    let source = watched
        .iter()
        .find(|entry| entry.address_id == source_id)
        .ok_or(TonError::Invalid(CHOOSE_SOURCE))?;
    if states.is_held(source_id) {
        return Err(TonError::Busy);
    }
    let mut destination = ton_destination(&request.to)
        .map_err(|_| TonError::Invalid("Enter a valid TON mainnet address or payment request."))?;
    if let Some(comment) = request.comment.as_ref().filter(|comment| !comment.is_empty()) {
        if comment.len() > MAX_COMMENT_BYTES {
            return Err(TonError::Invalid("Use a comment of at most 1024 bytes."));
        }
        if destination.comment.is_some() {
            return Err(TonError::Invalid(
                "The payment request already includes a comment. Clear this field to use it.",
            ));
        }
        destination.comment = Some(comment.clone());
    }
    if destination.address == source.address {
        return Err(TonError::Invalid("Choose a different recipient address."));
    }
    let tip = client.tip().await?;
    let account = client.account(&source.address, tip).await?;
    let now_unix_secs = clock.now_secs()?;
    let sync_unix_secs = i64::from(account.sync_utime);
    if now_unix_secs.saturating_sub(sync_unix_secs) > MAX_NODE_LAG_SECONDS
        || sync_unix_secs.saturating_sub(now_unix_secs) > MAX_NODE_LEAD_SECONDS
    {
        return Err(TonError::Invalid("The TON node is not up to date. Try again shortly."));
    }
    let (seqno, is_deploy) = account.sequence(&source.pubkey, source.version)?;
    let balance_nanogram = account.balance_nanogram()?;
    let valid_until = u32::try_from(now_unix_secs)
        .ok()
        .and_then(|seconds| seconds.checked_add(VALIDITY_SECONDS))
        .ok_or(TonError::Clock(ClockError::OutOfRange))?;
    let sender = Sender {
        watched: source,
        balance_nanogram,
        before_lt: account.last_transaction_id.lt,
        seqno,
        is_deploy,
        valid_until,
        tip,
    };
    let payment = match jetton {
        None => native_payment(client, &sender, request.amount, destination).await?,
        Some(jetton) => {
            jetton_payment(client, states, &sender, request.amount, destination, jetton).await?
        }
    };
    payment.validate()?;
    Ok((source_id, payment))
}

async fn native_payment(
    client: &TonClient,
    sender: &Sender<'_>,
    amount: SendAmount,
    destination: TonDestination,
) -> Result<Payment, TonError> {
    let balance_nanogram = sender.balance_nanogram;
    let funds = |needed_nanogram: u64| TonError::Funds {
        asset: AssetId::Native(CHAIN),
        needed: Amount::from(needed_nanogram),
        available: Amount::from(balance_nanogram),
    };
    let amount_nanogram = match amount {
        SendAmount::Exact { value } => u64::try_from(value.base_units())
            .map_err(|_| TonError::Invalid("The Gram amount is too large."))?,
        SendAmount::Max => balance_nanogram,
    };
    if amount_nanogram == 0 || amount_nanogram > balance_nanogram {
        return Err(funds(amount_nanogram.max(1)));
    }
    let TonDestination { address: recipient, is_bounceable, comment } = destination;
    // A raw address carries no bounce flag, so the recipient's state decides one: a
    // deployed contract can bounce what it refuses, and an account that is not deployed
    // yet must keep the payment rather than send it back.
    let is_bounceable = if is_bounceable == Some(false) {
        false
    } else {
        let is_active = client.account(&recipient, sender.tip).await?.state == AccountState::Active;
        if is_bounceable == Some(true) && !is_active {
            return Err(TonError::Invalid(
                "This recipient is not active. Ask for its non-bounceable address before sending.",
            ));
        }
        is_active
    };
    let is_max = amount == SendAmount::Max;
    let mut payment = sender.payment(
        recipient.to_base64_url_flags(!is_bounceable, false),
        is_max,
        Transfer::Native { amount_nanogram, is_bounceable, comment },
    );
    payment.fee_nanogram = client.fee_nanogram(&payment).await?;
    let amount_nanogram = if is_max {
        let remaining_nanogram = balance_nanogram.saturating_sub(payment.fee_nanogram);
        if let Transfer::Native { amount_nanogram: ref mut paid, .. } = payment.transfer {
            *paid = remaining_nanogram;
        }
        remaining_nanogram
    } else {
        amount_nanogram
    };
    let needed_nanogram = amount_nanogram
        .checked_add(payment.fee_nanogram)
        .ok_or(TonError::Internal("a Gram payment total overflowed"))?;
    if amount_nanogram == 0 || needed_nanogram > balance_nanogram {
        return Err(funds(needed_nanogram.max(payment.fee_nanogram.saturating_add(1))));
    }
    Ok(payment)
}

async fn jetton_payment(
    client: &TonClient,
    states: &States,
    sender: &Sender<'_>,
    amount: SendAmount,
    destination: TonDestination,
    jetton: Jetton,
) -> Result<Payment, TonError> {
    let wallet = jetton_wallet(client, states, sender.watched, jetton.token, sender.tip).await?;
    let balance = if client.account(&wallet, sender.tip).await?.state == AccountState::Active {
        let owner = &sender.watched.address;
        client.jetton_balance(&wallet, owner, &master(jetton.token)?, sender.tip).await?
    } else {
        Amount::ZERO
    };
    let value = match amount {
        SendAmount::Exact { value } => value,
        SendAmount::Max => balance,
    };
    if value.is_zero() || value > balance {
        return Err(TonError::Funds {
            asset: AssetId::Token(jetton.token),
            needed: value.max(Amount::from_base_units(1)),
            available: balance,
        });
    }
    let query_id = u64::from_be_bytes(
        *random_bytes::<8>()
            .map_err(|_| TonError::Internal("secure random bytes are unavailable"))?,
    );
    let mut payment = sender.payment(
        destination.spelling(),
        amount == SendAmount::Max,
        Transfer::Jetton {
            token: jetton.token,
            wallet: wallet.to_hex(),
            amount: value,
            query_id,
            attached_nanogram: jetton.attached_nanogram,
            comment: destination.comment,
        },
    );
    payment.fee_nanogram = client.fee_nanogram(&payment).await?;
    let needed_nanogram = payment.cost_nanogram()?;
    if needed_nanogram > sender.balance_nanogram {
        return Err(TonError::Funds {
            asset: AssetId::Native(CHAIN),
            needed: Amount::from(needed_nanogram),
            available: Amount::from(sender.balance_nanogram),
        });
    }
    Ok(payment)
}

/// The jetton wallet sync already found for the sender, or the one the master names.
async fn jetton_wallet(
    client: &TonClient,
    states: &States,
    watched: &Watched,
    token: TokenId,
    tip: u64,
) -> Result<TonAddress, TonError> {
    let cursor = states.cursor(watched.address_id).await?;
    if let Some(known) = cursor.jettons.iter().find(|jetton| jetton.token == token) {
        return known.address();
    }
    client.jetton_wallet(&master(token)?, &watched.address, Some(tip)).await
}
