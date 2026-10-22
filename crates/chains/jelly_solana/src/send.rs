use jelly_core::{
    Amount, FeeLevel, FeeQuote, FeeSelection, LevelFee, SendAmount, TxRequest, UnsignedTx,
};
use solana_hash::Hash;
use solana_pubkey::Pubkey;

use crate::client::{Balances, SolanaClient};
use crate::config::{NATIVE, SolanaConfig};
use crate::error::SolanaError;
use crate::fee::{self, ComputeUnits, Pricing, UnitCache};
use crate::payload::SolanaPayload;
use crate::rent::{RentCache, RentQuote};
use crate::route::{Leg, Route};
use crate::state::States;
use crate::token::{self, SolanaToken};
use crate::watched::Watched;
use crate::{address, preview, sign};

const TARGET: &str = "jelly::solana::send";

/// What a request asks its priority fee to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chosen {
    Level(FeeLevel),
    Lamports(u64),
}

/// Another chain's selection is refused rather than ignored.
fn chosen_priority_fee(selection: &FeeSelection) -> Result<Chosen, SolanaError> {
    match *selection {
        FeeSelection::Level { level } => Ok(Chosen::Level(level)),
        FeeSelection::Solana { ref priority_fee_lamports } => {
            let lamports = priority_fee_lamports
                .trim()
                .parse::<u64>()
                .map_err(|_| SolanaError::NotAnInteger { field: "the priority fee in lamports" })?;
            if lamports > fee::MAX_CHOSEN_PRIORITY_FEE_LAMPORTS {
                return Err(SolanaError::PriorityFeeAboveMaximum);
            }
            Ok(Chosen::Lamports(lamports))
        }
        FeeSelection::Rate { .. } | FeeSelection::Evm { .. } | FeeSelection::Total { .. } => {
            Err(SolanaError::ForeignFeeSelection)
        }
    }
}

const fn level_pricing(market: [Pricing; 3], level: FeeLevel) -> Pricing {
    let [slow, normal, fast] = market;
    match level {
        FeeLevel::Slow => slow,
        FeeLevel::Normal => normal,
        FeeLevel::Fast => fast,
    }
}

/// Everything a quote or a build reads besides the request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Inputs<'a> {
    pub(crate) client: &'a SolanaClient,
    pub(crate) config: &'a SolanaConfig,
    pub(crate) states: &'a States,
    pub(crate) watched: &'a [Watched],
    pub(crate) rent: &'a RentCache,
    pub(crate) units: &'a UnitCache,
}

/// Reads the blockhash, balances and account existence in one request, with the
/// optional estimates fetched alongside. Only shapes not yet measured this
/// session are simulated.
///
/// Quotes and builds share this, and each build reads a fresh blockhash.
pub(crate) async fn resolve(inputs: &Inputs<'_>, request: &TxRequest) -> Result<Plan, SolanaError> {
    let token = token::asset_token(request.asset)?;
    let chosen_fee = chosen_priority_fee(&request.fee)?;
    let payer = sender(inputs, request.sole_source().ok_or(SolanaError::NotOneSource)?)?;
    let recipient = address::parse("recipient", &request.to)?;

    let mut route = match token {
        None => Route::native(payer, recipient),
        Some(token) => Route::token(payer, recipient, token),
    };
    let (reads, optional) =
        tokio::join!(inputs.client.build_reads(&route), inputs.client.optional_reads(&route));
    let reads = reads?;
    match (&mut route.leg, reads.destination) {
        (Leg::Token { destination, .. }, Some(found)) => {
            *destination = found.account;
            route.has_destination_account = found.is_open;
        }
        (Leg::Native | Leg::Token { .. }, _) => {
            route.has_destination_account = reads.recipient_owner.is_some();
        }
    }

    let balances = reads.balances;
    let account_rent = inputs.rent.quote(0, optional.account_rent_lamports);
    let token_account_rent = token.filter(|_| route.is_account_creation()).map(|token| {
        inputs.rent.quote(token.account_bytes(), optional.token_account_rent_lamports)
    });

    let probe = probe_amount(request, &balances, token);
    let units = compute_units(inputs, &route, probe).await?;
    let market = FeeLevel::ALL.map(|level| {
        Pricing::sampled(
            &optional.fee_samples,
            level,
            units.limit(),
            inputs.config.max_level_priority_fee_lamports,
        )
    });
    let pricing = match chosen_fee {
        Chosen::Lamports(lamports) => Pricing::chosen(lamports, units.limit()),
        Chosen::Level(level) => level_pricing(market, level),
    };

    let mut plan = Plan {
        route,
        amount: 0,
        should_send_max: request.amount == SendAmount::Max,
        units,
        pricing,
        market,
        blockhash: reads.blockhash.blockhash,
        expiry_height: reads.blockhash.last_valid_block_height,
        balances,
        account_rent,
        token_account_rent,
        recipient_program: reads.recipient_owner,
    };
    plan.amount = match request.amount {
        SendAmount::Max => send_max_amount(&plan)?,
        SendAmount::Exact { value } => {
            u64::try_from(value.base_units()).map_err(|_| SolanaError::AmountTooLarge)?
        }
    };
    Ok(plan)
}

/// The owner of the watched address a request spends from.
fn sender(inputs: &Inputs<'_>, address_id: i64) -> Result<Pubkey, SolanaError> {
    inputs.states.ensure_readable()?;
    inputs
        .watched
        .iter()
        .find(|entry| entry.address_id == address_id)
        .map(|entry| entry.owner)
        .ok_or(SolanaError::NotWatched { address_id })
}

/// The cached measurement of this shape, else a fresh simulation that is
/// cached, else the compiled figure.
async fn compute_units(
    inputs: &Inputs<'_>,
    route: &Route,
    probe: u64,
) -> Result<ComputeUnits, SolanaError> {
    let shape = route.shape();
    if let Some(cached) = inputs.units.get(shape) {
        return Ok(cached);
    }
    let compiled = ComputeUnits::compiled(shape);
    // The simulation replaces the blockhash, so the probe carries none.
    let message = route.compile(probe, compiled, 0, Hash::default())?;
    match inputs
        .client
        .simulate(&message)
        .await
        .and_then(|consumed| consumed.map(ComputeUnits::measured).transpose())
    {
        Ok(Some(measured)) => {
            inputs.units.put(shape, measured);
            Ok(measured)
        }
        Ok(None) => {
            tracing::debug!(
                target: TARGET,
                "the node would not simulate this transfer; the compiled limit is used"
            );
            Ok(compiled)
        }
        Err(error) => {
            tracing::warn!(
                target: TARGET,
                %error,
                "a compute unit measurement failed; the compiled limit is used and the \
                 preview says so"
            );
            Ok(compiled)
        }
    }
}

/// A provisional amount to measure compute units with.
///
/// Compute units depend on the accounts touched, not the value moved, so any
/// amount measures the same. It is clamped to the balance, because simulating an
/// overdraft fails and loses the measurement.
fn probe_amount(request: &TxRequest, balances: &Balances, token: Option<SolanaToken>) -> u64 {
    let held = match token {
        Some(_) => balances.token_spendable.unwrap_or(0),
        None => balances.lamports,
    };
    let wanted = match request.amount {
        SendAmount::Exact { value } => u64::try_from(value.base_units()).unwrap_or(u64::MAX),
        SendAmount::Max => 1,
    };
    wanted.min(held).max(1)
}

/// One transfer resolved against the chain, shared by the quote and the build.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) route: Route,
    /// Base units of the asset the route moves. A send-max resolves it from the
    /// rest of the plan, so it is the last field settled.
    pub(crate) amount: u64,
    pub(crate) should_send_max: bool,
    pub(crate) units: ComputeUnits,
    pub(crate) pricing: Pricing,
    /// What each level would pay over the same compute units, in [`FeeLevel::ALL`]
    /// order.
    pub(crate) market: [Pricing; 3],
    pub(crate) blockhash: Hash,
    pub(crate) expiry_height: u64,
    pub(crate) balances: Balances,
    /// The rent-exempt minimum for a bare account, which is what a remainder is
    /// judged against.
    pub(crate) account_rent: RentQuote,
    /// The deposit a new token account costs, when this transfer creates one.
    pub(crate) token_account_rent: Option<RentQuote>,
    /// The program that owns the recipient's own account, when it exists.
    pub(crate) recipient_program: Option<Pubkey>,
}

impl Plan {
    /// Excludes any rent deposit.
    pub(crate) fn fee(&self) -> Result<u64, SolanaError> {
        fee::total_fee(self.pricing.micro_lamports_per_unit, self.units.limit())
            .ok_or(SolanaError::Overflow { what: "the fee" })
    }

    pub(crate) fn priority_fee(&self) -> Result<u64, SolanaError> {
        fee::priority_fee(self.pricing.micro_lamports_per_unit, self.units.limit())
            .ok_or(SolanaError::Overflow { what: "the priority fee" })
    }

    /// A quote's custom fee names the priority fee in lamports, so a custom fee
    /// starts from the level's.
    pub(crate) fn quote(&self) -> Result<FeeQuote, SolanaError> {
        let fee = self.fee()?;
        let max_amount = self.should_send_max.then(|| Amount::from(self.amount));
        let levels = FeeLevel::ALL
            .into_iter()
            .zip(self.market)
            .map(|(level, pricing)| {
                fee::total_fee(pricing.micro_lamports_per_unit, self.units.limit())
                    .map(|lamports| LevelFee { level, fee: Amount::from(lamports) })
                    .ok_or(SolanaError::Overflow { what: "the fee" })
            })
            .collect::<Result<_, _>>()?;
        Ok(FeeQuote {
            fee: Amount::from(fee),
            custom: Some(FeeSelection::Solana {
                priority_fee_lamports: self.priority_fee()?.to_string(),
            }),
            max_amount,
            levels,
        })
    }

    /// The token-account rent deposit, the only lamports besides the fee and a
    /// native amount that leave the wallet.
    fn deposit(&self) -> u64 {
        self.token_account_rent.map_or(0, RentQuote::lamports)
    }

    /// The fee and deposit, plus the amount when the transfer is native.
    pub(crate) fn lamports_out(&self) -> Result<u64, SolanaError> {
        let mut total = self
            .fee()?
            .checked_add(self.deposit())
            .ok_or(SolanaError::Overflow { what: "the total cost" })?;
        if matches!(self.route.leg, Leg::Native) {
            total = total
                .checked_add(self.amount)
                .ok_or(SolanaError::Overflow { what: "the total cost" })?;
        }
        Ok(total)
    }

    pub(crate) fn remainder(&self) -> u64 {
        self.balances.lamports.saturating_sub(self.lamports_out().unwrap_or(u64::MAX))
    }

    fn payload(&self) -> Result<SolanaPayload, SolanaError> {
        let message = self.route.compile(
            self.amount,
            self.units,
            self.pricing.micro_lamports_per_unit,
            self.blockhash,
        )?;
        Ok(SolanaPayload::new(
            message,
            self.route.payer,
            self.expiry_height,
            self.route.leg.asset(),
            self.amount,
            self.route.recipient.to_string(),
        ))
    }
}

/// A native send-max is the balance minus the fee, because a Solana account may
/// be drained to exactly zero. A token send-max is the whole spendable token
/// balance, because the fee comes from lamports.
fn send_max_amount(plan: &Plan) -> Result<u64, SolanaError> {
    if let Leg::Token { .. } = plan.route.leg {
        return Ok(plan.balances.token_spendable.unwrap_or(0));
    }
    let reserved = plan
        .fee()?
        .checked_add(plan.deposit())
        .ok_or(SolanaError::Overflow { what: "the total cost" })?;
    plan.balances.lamports.checked_sub(reserved).ok_or_else(|| SolanaError::InsufficientFunds {
        asset: NATIVE,
        needed: Amount::from(reserved),
        available: Amount::from(plan.balances.lamports),
    })
}

/// Refuses a transfer the balances cannot cover, in the asset it falls short in.
fn check_affordable(plan: &Plan) -> Result<(), SolanaError> {
    if plan.amount == 0 {
        return Err(SolanaError::AmountZero);
    }
    if let Leg::Token { token, .. } = plan.route.leg {
        let held = plan.balances.token_spendable.unwrap_or(0);
        if plan.amount > held {
            return Err(SolanaError::InsufficientFunds {
                asset: token.asset(),
                needed: Amount::from(plan.amount),
                available: Amount::from(held),
            });
        }
    }
    let needed = plan.lamports_out()?;
    if plan.balances.lamports < needed {
        return Err(SolanaError::InsufficientFunds {
            asset: NATIVE,
            needed: Amount::from(needed),
            available: Amount::from(plan.balances.lamports),
        });
    }
    Ok(())
}

pub(crate) fn build(plan: &Plan) -> Result<UnsignedTx, SolanaError> {
    check_affordable(plan)?;
    let payload = plan.payload()?;
    sign::check_size(sign::signed_size(payload.message_bytes().len()))?;
    Ok(UnsignedTx { preview: preview::of(plan, &payload)?, payload: payload.encode() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fee_selection_from_another_chain_is_refused_and_a_chosen_fee_is_read() {
        for level in FeeLevel::ALL {
            assert_eq!(
                chosen_priority_fee(&FeeSelection::Level { level }).expect("a level"),
                Chosen::Level(level)
            );
        }
        let chosen = |text: &str| {
            chosen_priority_fee(&FeeSelection::Solana { priority_fee_lamports: text.to_owned() })
        };
        assert_eq!(chosen(" 7 ").expect("a chosen fee"), Chosen::Lamports(7));
        assert_eq!(chosen("0").expect("no priority fee at all"), Chosen::Lamports(0));
        assert_eq!(chosen("1000000000").expect("one SOL"), Chosen::Lamports(1_000_000_000));
        assert!(matches!(chosen("1000000001"), Err(SolanaError::PriorityFeeAboveMaximum)));
        assert!(matches!(chosen("1.5"), Err(SolanaError::NotAnInteger { .. })));
        assert!(matches!(
            chosen_priority_fee(&FeeSelection::Rate { rate: String::from("3") }),
            Err(SolanaError::ForeignFeeSelection)
        ));
    }
}
