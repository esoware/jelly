use alloy::primitives::Address;
use jelly_core::{Amount, AssetId, FeeQuote, LevelFee, RetryPolicy, SignedTx, TxId, UnsignedTx};

use crate::client::EvmClient;
use crate::error::EvmError;
use crate::fee::{FeeMarket, FeePair, GasLimit};
use crate::params::EvmParams;
use crate::payload::{self, EvmPayload, Intent, Shape};
use crate::token::{TokenMeta, TokenTable};
use crate::{preview, units};

const TARGET: &str = "jelly::evm::send";

/// One send resolved against the network, shared by the quote and the build.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) params: &'static EvmParams,
    pub(crate) from: Address,
    /// The recipient the user named.
    pub(crate) to: Address,
    pub(crate) asset: AssetId,
    pub(crate) token: Option<TokenMeta>,
    /// The transfer in the asset's base units, already resolved for a send-max.
    pub(crate) amount: Amount,
    pub(crate) should_send_max: bool,
    pub(crate) nonce: u64,
    pub(crate) is_nonce_pinned: bool,
    /// Absent when a pinned price was resolved against a node that would not
    /// serve the fee history.
    pub(crate) market: Option<FeeMarket>,
    pub(crate) fee: FeePair,
    pub(crate) gas: GasLimit,
    /// Calldata that replaces the generated transfer verbatim.
    pub(crate) calldata: Option<Vec<u8>>,
    pub(crate) native_balance_wei: u128,
    pub(crate) token_balance: Option<u128>,
    /// Present when this build reuses a nonce slot, which makes it a replacement.
    pub(crate) replaces: Option<FeePair>,
    /// A replacement that pays nothing to its own sender, so the transaction it
    /// replaces never runs.
    pub(crate) is_cancel: bool,
    /// The oracle's bound on the L1 data fee at the prices it read, and zero on a
    /// network that has none.
    pub(crate) data_fee_wei: u128,
}

impl Plan {
    pub(crate) const fn intent(&self) -> Intent {
        Intent { from: self.from, asset: self.asset, to: self.to, amount: self.amount }
    }

    pub(crate) fn worst_case_fee_wei(&self) -> Result<u128, EvmError> {
        reserved_wei(self.fee, self.gas.limit(), self.data_fee_wei)
    }

    pub(crate) fn expected_fee_wei(&self) -> Result<u128, EvmError> {
        self.expected_fee_wei_at(self.fee)
    }

    /// The unpadded gas estimate at the next base fee plus the tip, never above the
    /// cap. Without a market there is no base fee to price against, so the worst
    /// case is the only honest number.
    ///
    /// The data fee counts at the oracle's bound, without the headroom the worst
    /// case holds back.
    fn expected_fee_wei_at(&self, pair: FeePair) -> Result<u128, EvmError> {
        let gas_wei = match self.market {
            Some(market) => pair.expected_cost_wei(self.gas.expected(), market.base_fee_per_gas()),
            None => pair.max_cost_wei(self.gas.limit()),
        }?;
        gas_wei
            .checked_add(self.data_fee_wei)
            .ok_or(EvmError::FeeOverflow { field: "expected fee" })
    }

    /// The worst-case fee, plus the amount when the transfer is native.
    pub(crate) fn native_cost_wei(&self) -> Result<u128, EvmError> {
        let fee_wei = self.worst_case_fee_wei()?;
        if self.token.is_some() {
            return Ok(fee_wei);
        }
        self.amount
            .base_units()
            .checked_add(fee_wei)
            .ok_or(EvmError::FeeOverflow { field: "total cost" })
    }

    /// A quote prices the plan at its expected fee. The worst case is in the
    /// preview, and a send-max names the amount it settled on against it.
    ///
    /// Each level is priced at this plan's gas, raised to the slot's floor as the
    /// level itself would be, so picking one costs what its row says.
    pub(crate) fn quote(&self) -> Result<FeeQuote, EvmError> {
        let mut levels = Vec::new();
        if let Some(market) = self.market {
            for (level, pair) in market.levels() {
                let pair = match self.replaces {
                    Some(floor) => pair.raised_to(floor)?,
                    None => pair,
                };
                let fee_wei = self.expected_fee_wei_at(pair)?;
                levels.push(LevelFee { level, fee: Amount::from_base_units(fee_wei) });
            }
        }
        Ok(FeeQuote {
            fee: Amount::from_base_units(self.expected_fee_wei()?),
            custom: Some(self.fee.custom(self.gas.limit())),
            max_amount: self.should_send_max.then_some(self.amount),
            levels,
        })
    }
}

/// A native send-max is the balance minus the worst-case fee the node reserves.
/// A token send-max is the whole token balance, because gas comes from the native
/// balance.
pub(crate) fn send_max_amount(
    asset: AssetId,
    native_balance_wei: u128,
    token_balance: Option<u128>,
    fee: FeePair,
    gas_limit: u64,
    data_fee_wei: u128,
) -> Result<Amount, EvmError> {
    match asset {
        AssetId::Token(_) => Ok(Amount::from_base_units(token_balance.unwrap_or(0))),
        AssetId::Native(_) => {
            let reserved = reserved_wei(fee, gas_limit, data_fee_wei)?;
            native_balance_wei
                .checked_sub(reserved)
                .map(Amount::from_base_units)
                .ok_or_else(|| insufficient(asset, reserved, native_balance_wei))
        }
    }
}

/// Percent of the oracle's bound a send holds back for the L1 data fee.
///
/// Twice the bound, as the gas cap reserves twice the base fee, because the L1
/// prices the bound was read at can rise before the transaction lands.
const DATA_FEE_HEADROOM_PCT: u64 = 200;

/// What the balance must cover before execution. An OP Stack node refuses a
/// transaction whose sender cannot pay the gas and the data fee together.
fn reserved_wei(fee: FeePair, gas_limit: u64, data_fee_wei: u128) -> Result<u128, EvmError> {
    let data_wei = units::scale_pct("L1 data fee", data_fee_wei, DATA_FEE_HEADROOM_PCT)?;
    fee.max_cost_wei(gas_limit)?
        .checked_add(data_wei)
        .ok_or(EvmError::FeeOverflow { field: "max total fee" })
}

pub(crate) fn build(plan: &Plan, tokens: &TokenTable) -> Result<UnsignedTx, EvmError> {
    let worst_case_fee_wei = plan.worst_case_fee_wei()?;
    check_funding(plan, worst_case_fee_wei)?;

    let shape = Shape::of(plan.token, plan.to, plan.amount.base_units(), plan.calldata.as_deref());
    let tx = payload::transaction(plan.params, plan.nonce, plan.gas.limit(), plan.fee, &shape);
    let built = EvmPayload::new(tx, plan.intent());
    let preview = preview::of(plan, &built, tokens, worst_case_fee_wei)?;
    Ok(UnsignedTx { preview, payload: built.encode()? })
}

/// Gas is owed in the native coin, so a token send the token balance covers can
/// still fail on the fee. The refusal names the asset that is short.
fn check_funding(plan: &Plan, worst_case_fee_wei: u128) -> Result<(), EvmError> {
    if plan.token.is_some() {
        let held = plan.token_balance.unwrap_or(0);
        let wanted = plan.amount.base_units();
        if wanted > held {
            return Err(insufficient(plan.asset, wanted, held));
        }
        if worst_case_fee_wei > plan.native_balance_wei {
            return Err(insufficient(
                plan.params.native(),
                worst_case_fee_wei,
                plan.native_balance_wei,
            ));
        }
        return Ok(());
    }
    let needed_wei = plan.native_cost_wei()?;
    if needed_wei > plan.native_balance_wei {
        return Err(insufficient(plan.asset, needed_wei, plan.native_balance_wei));
    }
    Ok(())
}

const fn insufficient(asset: AssetId, needed: u128, available: u128) -> EvmError {
    EvmError::InsufficientFunds {
        asset,
        needed: Amount::from_base_units(needed),
        available: Amount::from_base_units(available),
    }
}

pub(crate) fn payload_of(tx: &UnsignedTx, params: &EvmParams) -> Result<EvmPayload, EvmError> {
    let chain = tx.chain();
    if chain != params.chain {
        return Err(EvmError::ForeignChain { chain });
    }
    let payload = EvmPayload::decode(tx.payload.as_bytes())?;
    if payload.chain_id() != params.chain_id {
        return Err(EvmError::ForeignChainId { chain_id: payload.chain_id() });
    }
    Ok(payload)
}

/// Returns the locally computed txid, and treats an already-known refusal as
/// success.
///
/// An endpoint's echoed hash is not trusted, because a different one would break
/// receipt lookup and replacement.
pub(crate) async fn deliver(client: &EvmClient, tx: &SignedTx) -> Result<TxId, EvmError> {
    match client.send_raw(tx.raw.as_bytes()).await {
        Ok(hash) => {
            if hash.to_string() != tx.txid.value() {
                tracing::warn!(
                    target: TARGET,
                    "the endpoint echoed a transaction hash that is not the one it was sent"
                );
            }
            Ok(tx.txid.clone())
        }
        Err(EvmError::Rejected(rejection)) if rejection.retry == RetryPolicy::AlreadyLanded => {
            Ok(tx.txid.clone())
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, FeeLevel};

    use super::*;
    use crate::fee::GasSource;
    use crate::fixtures::address;
    use crate::rpc::FeeHistory;
    use crate::test_util::ETHEREUM;

    const GWEI: u128 = 1_000_000_000;
    const ETH: u128 = 1_000_000_000_000_000_000;

    fn plan(market: Option<FeeMarket>, fee: FeePair) -> Plan {
        Plan {
            params: &ETHEREUM,
            from: address(1),
            to: address(2),
            asset: AssetId::Native(ChainId::Ethereum),
            token: None,
            amount: Amount::from_base_units(GWEI),
            should_send_max: false,
            nonce: 7,
            is_nonce_pinned: false,
            market,
            fee,
            gas: GasLimit::new(21_000, GasSource::UserOverride).expect("a limit"),
            calldata: None,
            native_balance_wei: 5 * GWEI,
            token_balance: None,
            replaces: None,
            is_cancel: false,
            data_fee_wei: 0,
        }
    }

    #[test]
    fn a_quote_prices_its_pair_at_the_expected_cost_and_names_the_custom_fee_that_buys_it() {
        let history: FeeHistory = serde_json::from_str(
            r#"{"baseFeePerGas":["0x2540be400","0x2540be400"],"reward":[["0x77359400"]]}"#,
        )
        .expect("the recorded answer decodes");
        let market = FeeMarket::from_history(&history, &ETHEREUM).expect("a market");
        let normal = market.level(FeeLevel::Normal);

        let automatic = plan(Some(market), normal).quote().expect("a quote");
        assert_eq!(
            automatic.fee.base_units(),
            (10 * GWEI + normal.max_priority_fee_per_gas()) * 21_000,
            "base fee plus the tip"
        );
        assert_eq!(automatic.custom, Some(normal.custom(21_000)));
        assert_eq!(automatic.max_amount, None);

        let chosen = FeePair::new(11 * GWEI, 3 * GWEI).expect("a pair");
        let capped = plan(Some(market), chosen).quote().expect("a quote");
        assert_eq!(
            capped.fee.base_units(),
            11 * GWEI * 21_000,
            "a cap below the base fee plus the tip is what is paid"
        );

        let unpriced = plan(None, normal).quote().expect("a quote");
        assert_eq!(
            unpriced.fee.base_units(),
            normal.max_fee_per_gas() * 21_000,
            "without a base fee the worst case is what is quoted"
        );
        assert!(unpriced.levels.is_empty(), "without a market there are no levels to price");

        let send_max = Plan { should_send_max: true, ..plan(Some(market), normal) };
        assert_eq!(
            send_max.quote().expect("a quote").max_amount,
            Some(send_max.amount),
            "a send-max names the amount it settled on"
        );
    }

    #[test]
    fn a_quote_prices_every_level_at_the_plans_gas_and_data_fee() {
        let history: FeeHistory = serde_json::from_str(
            r#"{"baseFeePerGas":["0x2540be400","0x2540be400"],
                "reward":[["0x3b9aca00","0x77359400","0xb2d05e00"]]}"#,
        )
        .expect("the recorded answer decodes");
        let market = FeeMarket::from_history(&history, &ETHEREUM).expect("a market");
        let quote = Plan { data_fee_wei: 5, ..plan(Some(market), market.level(FeeLevel::Fast)) }
            .quote()
            .expect("a quote");
        let levels: Vec<FeeLevel> = quote.levels.iter().map(|level| level.level).collect();
        assert_eq!(levels, FeeLevel::ALL);
        for row in &quote.levels {
            let tip = market.level(row.level).max_priority_fee_per_gas();
            assert_eq!(row.fee.base_units(), (10 * GWEI + tip) * 21_000 + 5, "{}", row.level);
        }
        assert_eq!(
            quote.levels.last().map(|row| row.fee),
            Some(quote.fee),
            "the level the plan pays is the one it quotes"
        );

        let floor = FeePair::new(50 * GWEI, 5 * GWEI).expect("a pair");
        let replacing = Plan { replaces: Some(floor), ..plan(Some(market), floor) };
        for row in replacing.quote().expect("a quote").levels {
            assert_eq!(
                row.fee.base_units(),
                (10 * GWEI + 5 * GWEI) * 21_000,
                "{} is raised to the slot's floor",
                row.level
            );
        }
    }

    #[test]
    fn an_l1_data_fee_is_reserved_with_the_gas_and_quoted_with_it() {
        let pair = FeePair::new(30 * GWEI, GWEI).expect("a pair");
        let plan = Plan { data_fee_wei: 7, ..plan(None, pair) };
        assert_eq!(
            plan.worst_case_fee_wei().expect("a fee"),
            30 * GWEI * 21_000 + 14,
            "the worst case holds back twice the bound"
        );
        assert_eq!(plan.quote().expect("a quote").fee.base_units(), 30 * GWEI * 21_000 + 7);

        let native = AssetId::Native(ChainId::Base);
        let max = send_max_amount(native, ETH, None, pair, 21_000, 7).expect("an amount");
        assert_eq!(max.base_units(), ETH - 30 * GWEI * 21_000 - 14);
        assert!(matches!(
            send_max_amount(native, 30 * GWEI * 21_000 + 13, None, pair, 21_000, 7),
            Err(EvmError::InsufficientFunds { .. })
        ));
    }
}
