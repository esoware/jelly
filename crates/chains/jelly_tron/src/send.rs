//! Resolves a request and builds its bytes, for quotes and sends alike. The
//! preview fee is read from the final transaction.
//!
//! For a send-max, the amount's varint width affects the bandwidth cost, so each
//! width is priced once, from the widest down, rather than chasing a fee that
//! depends on its own amount.

use jelly_core::{
    Amount, AssetId, FeeQuote, FeeSelection, SendAmount, TokenId, TxRequest, UnsignedTx,
};

use crate::abi;
use crate::address;
use crate::client::TronClient;
use crate::config::{TRX, TronConfig};
use crate::error::TronError;
use crate::fee::{self, Charge, TxCost};
use crate::params::ChainParams;
use crate::payload::TronPayload;
use crate::preview;
use crate::raw::{self, Header};
use crate::shape::Resources;
use crate::token::asset_token;
use crate::watched::Watched;

/// One request, resolved against the chain and built.
///
/// What a transaction states about itself is read back off [`Plan::payload`]
/// through [`crate::payload::Facts`], so this holds only what the bytes cannot
/// state, such as the charge and the prices.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) params: ChainParams,
    pub(crate) cost: TxCost,
    /// The `raw_data` bytes that will be signed.
    pub(crate) payload: TronPayload,
}

impl Plan {
    /// The unsigned transaction, previewed from the bytes it carries.
    pub(crate) fn unsigned(&self) -> Result<UnsignedTx, TronError> {
        let facts = self.payload.facts()?;
        Ok(UnsignedTx { preview: preview::of(self, &facts), payload: self.payload.encode() })
    }

    /// What the transfer burns. TRON has no fee market, so a transfer costs what
    /// the resource model says at the published prices and there is no price to
    /// choose.
    ///
    /// A send-max quote also names the amount the plan settled on.
    pub(crate) fn quote(&self, amount: SendAmount) -> Result<FeeQuote, TronError> {
        let max_amount = match amount {
            SendAmount::Max => Some(self.payload.facts()?.amount),
            SendAmount::Exact { .. } => None,
        };
        Ok(FeeQuote {
            fee: Amount::from(self.cost.burn_sun),
            custom: None,
            max_amount,
            levels: Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Inputs<'a> {
    pub(crate) client: &'a TronClient,
    pub(crate) config: &'a TronConfig,
    pub(crate) params: ChainParams,
}

/// Reads what a send needs from the chain and plans the transaction.
///
/// The reads are sequential, because TRON's HTTP API has no batch form and the
/// endpoint pacer admits only a small burst. Four round trips in order for TRX,
/// or five for a token, beat the same number at once being turned away.
pub(crate) async fn resolve(
    inputs: &Inputs<'_>,
    request: &TxRequest,
    sender: &Watched,
) -> Result<Plan, TronError> {
    let token = asset_token(request.asset)?;
    if !matches!(request.fee, FeeSelection::Level { .. }) {
        return Err(TronError::ForeignFeeSelection);
    }

    let to_body = address::body("recipient", &request.to)?;
    // java-tron's TransferActuator refuses a TransferContract whose recipient is
    // its owner. A TRC-20 transfer to the same address is an ordinary call.
    if token.is_none() && to_body == sender.body {
        return Err(TronError::TrxToSelf);
    }

    let tip = inputs.client.now_block().await?;
    let resources = inputs.client.resources(&sender.address).await?;
    let account = inputs.client.account(&sender.address).await?;
    let header = Header::from_block(&tip, inputs.config.expiration)?;

    let common = Common {
        asset: request.asset,
        from: &sender.address,
        from_body: sender.body,
        to_body,
        amount: request.amount,
        params: inputs.params,
        resources,
        trx_balance_sun: account.balance_sun(),
    };
    match token {
        None => native_transfer(inputs, &common, header).await,
        Some(token) => token_transfer(inputs, &common, header, token).await,
    }
}

/// What a TRX transfer and a token transfer both read before they part.
#[derive(Debug)]
struct Common<'a> {
    asset: AssetId,
    from: &'a str,
    from_body: [u8; address::BODY_BYTES],
    to_body: [u8; address::BODY_BYTES],
    amount: SendAmount,
    params: ChainParams,
    resources: Resources,
    trx_balance_sun: u64,
}

async fn native_transfer(
    inputs: &Inputs<'_>,
    common: &Common<'_>,
    header: Header,
) -> Result<Plan, TronError> {
    let recipient = inputs.client.account(&address::display(&common.to_body)).await?;
    let charge = Charge {
        is_activation: !recipient.is_activated(),
        energy: None,
        params: common.params,
        resources: common.resources,
    };

    let built = match common.amount {
        SendAmount::Max => settle_send_max(common, &header, &charge)?,
        SendAmount::Exact { value } => {
            let amount = requested_amount(value)?;
            let sun = u64::try_from(amount).map_err(|_| TronError::AmountTooLarge)?;
            let raw = raw::transfer(&header, &common.from_body, &common.to_body, sun)?;
            let cost = charge.of(&raw)?;
            Built { amount, raw, cost }
        }
    };

    let needed = built
        .amount
        .checked_add(u128::from(built.cost.burn_sun))
        .ok_or(TronError::Overflow { what: "a transfer total" })?;
    if needed > u128::from(common.trx_balance_sun) {
        return Err(TronError::InsufficientFunds {
            asset: common.asset,
            needed: Amount::from_base_units(needed),
            available: Amount::from(common.trx_balance_sun),
        });
    }
    Ok(plan(common, built))
}

async fn token_transfer(
    inputs: &Inputs<'_>,
    common: &Common<'_>,
    header: Header,
    token: TokenId,
) -> Result<Plan, TronError> {
    let held = inputs.client.token_balance(common.from, &common.from_body, token).await?;
    let amount = match common.amount {
        SendAmount::Max => held,
        SendAmount::Exact { value } => requested_amount(value)?,
    };
    if amount == 0 {
        return Err(TronError::AmountZero);
    }
    if amount > held {
        return Err(TronError::InsufficientFunds {
            asset: common.asset,
            needed: Amount::from_base_units(amount),
            available: Amount::from_base_units(held),
        });
    }

    // The measurement is taken against the real recipient and the real amount,
    // because a transfer to an address that holds none of the token writes a
    // fresh storage slot and costs roughly twice as much.
    let energy =
        inputs.client.measure_transfer(common.from, token, &common.to_body, amount).await?;
    let fee_limit_sun = fee::suggested_fee_limit(energy.needed, &common.params)?;
    let header = header.with_fee_limit(fee_limit_sun, common.params.max_fee_limit_sun)?;

    let charge = Charge {
        // A TRC-20 transfer never creates the recipient's account row, because
        // the balance lives in contract storage. The plain bandwidth rule applies
        // even to a recipient with no account.
        is_activation: false,
        energy: Some(energy),
        params: common.params,
        resources: common.resources,
    };
    let calldata = abi::encode_transfer(&common.to_body, amount);
    let raw = raw::trigger(
        &header,
        &common.from_body,
        &address::body("token contract", token.contract())?,
        calldata,
    );
    let cost = charge.of(&raw)?;

    // The bandwidth and energy a token transfer burns are paid in TRX, so an
    // account without enough of it is a broadcast that is certain to fail. The
    // surcharge can rise before execution by the margin the fee limit allows, and
    // a call that runs out of energy still burns what it used.
    let padded_sun = cost.padded_burn_sun(&common.params)?;
    if padded_sun > common.trx_balance_sun {
        return Err(TronError::InsufficientFunds {
            asset: TRX,
            needed: Amount::from(padded_sun),
            available: Amount::from(common.trx_balance_sun),
        });
    }
    Ok(plan(common, Built { amount, raw, cost }))
}

fn plan(common: &Common<'_>, built: Built) -> Plan {
    Plan { params: common.params, cost: built.cost, payload: TronPayload::new(built.raw) }
}

const fn requested_amount(amount: Amount) -> Result<u128, TronError> {
    let amount = amount.base_units();
    if amount == 0 {
        return Err(TronError::AmountZero);
    }
    Ok(amount)
}

/// One build of the transaction, with the amount it sends and what it costs.
#[derive(Debug)]
struct Built {
    amount: u128,
    raw: Vec<u8>,
    cost: TxCost,
}

/// Finds the largest send-max amount the balance pays for.
///
/// The bytes differ only in the amount's varint, so every amount of one varint
/// width costs the same and a wider one never costs less. The answer lies in the
/// widest width whose narrowest amount is affordable, and is the balance less
/// that width's fee, capped at the width's widest amount.
///
/// Each width is priced on its own, because a pool can cover a narrower transfer
/// whole and leave a wider one to burn in full, so one byte more can cost the
/// whole fee. That is one build per width tried, plus one for the answer when it
/// is not the amount its width was priced at. The caller checks
/// `amount + burn <= balance` either way.
fn settle_send_max(
    common: &Common<'_>,
    header: &Header,
    charge: &Charge,
) -> Result<Built, TronError> {
    let balance = common.trx_balance_sun;
    let build = |amount: u64| -> Result<Built, TronError> {
        let raw = raw::transfer(header, &common.from_body, &common.to_body, amount)?;
        let cost = charge.of(&raw)?;
        Ok(Built { amount: u128::from(amount), raw, cost })
    };

    let mut widest = balance;
    loop {
        let priced = build(widest)?;
        let burn = priced.cost.burn_sun;
        // A varint carries seven bits a byte, and a zero amount is never sent.
        let significant_bits = u64::BITS.saturating_sub(widest.leading_zeros());
        let narrowest = significant_bits
            .saturating_sub(1)
            .checked_div(7)
            .and_then(|extra_bytes| 1u64.checked_shl(extra_bytes.saturating_mul(7)))
            .ok_or(TronError::Overflow { what: "a send-max amount" })?;
        if let Some(affordable) = balance.checked_sub(burn).filter(|amount| *amount >= narrowest) {
            return if affordable >= widest { Ok(priced) } else { build(affordable) };
        }
        let Some(narrower) = narrowest.checked_sub(1).filter(|amount| *amount > 0) else {
            return Err(TronError::InsufficientFunds {
                asset: common.asset,
                needed: Amount::from_base_units(u128::from(burn).saturating_add(1)),
                available: Amount::from(balance),
            });
        };
        widest = narrower;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee::BandwidthSource;
    use crate::fixtures::{body, header, priced_params, resources};
    use crate::test_util::{OWNER, RECIPIENT};

    fn common(balance: u64, resources: Resources) -> Common<'static> {
        Common {
            asset: TRX,
            from: OWNER,
            from_body: body(OWNER),
            to_body: body(RECIPIENT),
            amount: SendAmount::Max,
            params: priced_params(),
            resources,
            trx_balance_sun: balance,
        }
    }

    fn charge(common: &Common<'_>, is_activation: bool) -> Charge {
        Charge { is_activation, energy: None, params: common.params, resources: common.resources }
    }

    #[test]
    fn send_max_on_a_covered_pool_sends_the_whole_balance() {
        let common = common(100_000_000, resources(0, 0, 0));
        let settled =
            settle_send_max(&common, &header(), &charge(&common, false)).expect("it settles");
        assert_eq!(settled.cost.bandwidth.source, BandwidthSource::Free);
        assert_eq!(settled.cost.burn_sun, 0);
        assert_eq!(settled.amount, 100_000_000, "nothing burns, so nothing is held back");
    }

    #[test]
    fn send_max_on_a_burned_transfer_leaves_exactly_the_burn_behind() {
        // No free allowance and no stake, so every point is burned and the amount
        // has to settle against a fee that depends on its own size.
        let common = common(100_000_000, resources(600, 0, 0));
        let settled =
            settle_send_max(&common, &header(), &charge(&common, false)).expect("it settles");
        assert_eq!(settled.cost.bandwidth.source, BandwidthSource::Burned);
        let spent = settled.amount.checked_add(u128::from(settled.cost.burn_sun)).expect("a total");
        assert_eq!(spent, 100_000_000, "the settled amount plus the burn is the whole balance");
        assert_eq!(
            raw::bandwidth_points(&settled.raw).expect("points"),
            settled.cost.bandwidth.points,
            "the cost is the cost of these exact bytes"
        );
    }

    #[test]
    fn send_max_settles_near_a_varint_boundary_without_ever_overspending() {
        // Balances across several varint widths. Every answer must be affordable
        // and leave at most one byte of bandwidth behind.
        let price = ChainParams::FALLBACK.bandwidth_price_sun;
        for balance in [
            266_000,
            266_001,
            267_000,
            300_000,
            16_383_999,
            16_384_000,
            2_097_152_000,
            100_000_000,
            1_000_000_000,
        ] {
            let common = common(balance, resources(600, 0, 0));
            let settled =
                settle_send_max(&common, &header(), &charge(&common, false)).expect("it settles");
            let spent =
                settled.amount.checked_add(u128::from(settled.cost.burn_sun)).expect("a total");
            assert!(spent <= u128::from(balance), "overspent at {balance}");
            let left = u128::from(balance).saturating_sub(spent);
            assert!(left <= u128::from(price), "left {left} behind at {balance}");
        }
    }

    #[test]
    fn a_balance_that_cannot_pay_its_own_bandwidth_is_refused() {
        let common = common(1_000, resources(600, 0, 0));
        let error = settle_send_max(&common, &header(), &charge(&common, false))
            .expect_err("a transfer that cannot pay for itself");
        assert!(matches!(error, TronError::InsufficientFunds { .. }), "{error:?}");
    }

    #[test]
    fn send_max_settles_on_an_amount_the_free_allowance_covers_when_a_wider_one_would_burn() {
        // Amounts below 2^28 encode in four varint bytes and 2^28 itself in five, so a
        // free allowance sized for the shorter transfer covers every amount below 2^28
        // and none at or above it.
        const BOUNDARY: u64 = 268_435_456;
        let points = |amount: u64| {
            let raw = raw::transfer(&header(), &body(OWNER), &body(RECIPIENT), amount)
                .expect("a transfer builds");
            raw::bandwidth_points(&raw).expect("points")
        };
        let short_points = points(BOUNDARY.saturating_sub(1));
        assert!(points(BOUNDARY) > short_points, "the wider amount costs one more byte");
        let balance = BOUNDARY.saturating_add(500);
        let common = common(balance, resources(600u64.saturating_sub(short_points), 0, 0));

        let settled =
            settle_send_max(&common, &header(), &charge(&common, false)).expect("it settles");
        let spent = settled.amount.checked_add(u128::from(settled.cost.burn_sun)).expect("a total");
        let left = u128::from(balance).saturating_sub(spent);
        let price = u128::from(ChainParams::FALLBACK.bandwidth_price_sun);
        assert!(
            left <= price,
            "expected send-max to leave at most one byte's bandwidth cost ({price} sun) behind, \
             because {} sun fits the free allowance and costs nothing, but it sent {} sun with a \
             {} sun burn and left {left} sun behind",
            BOUNDARY.saturating_sub(1),
            settled.amount,
            settled.cost.burn_sun
        );
    }

    #[test]
    fn send_max_on_an_activation_leaves_exactly_the_flat_charge_behind() {
        let common = common(100_000_000, resources(600, 0, 0));
        let settled =
            settle_send_max(&common, &header(), &charge(&common, true)).expect("it settles");
        // The flat account-creation charge does not depend on the size, so the
        // whole remainder is sendable.
        assert_eq!(settled.cost.burn_sun, 1_100_000);
        assert_eq!(settled.amount, 98_900_000);
    }
}
