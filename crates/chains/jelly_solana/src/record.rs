//! Amounts come from the metadata's balance deltas, which cover inner and
//! multi-instruction transfers. Instructions only name counterparties and mark a
//! record as decoded rather than balance-only.

use jelly_core::{Amount, AssetId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord};

use crate::config::NATIVE;
use crate::rpc::{
    ParsedAccountKey, ParsedInstruction, TokenBalance, TransactionMeta, TransactionRow,
};
use crate::token::{SOLANA_TOKENS, SolanaToken};

/// One transaction, read for one watched address.
struct Seen<'a> {
    tx: &'a TransactionRow,
    metadata: &'a TransactionMeta,
    signature: &'a str,
    address: &'a str,
    address_id: i64,
    /// The fee, which only the fee payer paid.
    fee: Option<Amount>,
    tip_slot: u64,
}

pub(crate) fn of_transaction(
    tx: &TransactionRow,
    signature: &str,
    address: &str,
    address_id: i64,
    tip_slot: u64,
) -> Vec<TxRecord> {
    let Some(metadata) = tx.meta.as_ref() else { return Vec::new() };
    let keys = &tx.transaction.message.account_keys;
    let is_fee_payer = keys.first().is_some_and(|key| key.pubkey == address);
    let seen = Seen {
        tx,
        metadata,
        signature,
        address,
        address_id,
        fee: is_fee_payer.then(|| Amount::from(metadata.fee)),
        tip_slot,
    };

    if metadata.err.is_some() {
        // A failed transaction moved nothing and still cost the fee payer the
        // fee. It never contributes to a total.
        if !is_fee_payer {
            return Vec::new();
        }
        return vec![seen.record(Parts {
            asset: NATIVE,
            direction: TxDirection::Outgoing,
            amount: Amount::ZERO,
            counterparty: None,
            is_decoded: true,
            outcome: TxOutcome::Failed,
        })];
    }

    let mut records = Vec::new();
    for token in SOLANA_TOKENS {
        if let Some(record) = seen.token_record(token) {
            records.push(record);
        }
    }
    if let Some(record) = seen.native_record(records.is_empty()) {
        records.push(record);
    }
    records
}

impl Seen<'_> {
    fn token_record(&self, token: SolanaToken) -> Option<TxRecord> {
        let mint = token.mint().to_string();
        let delta = token_delta(self.metadata, self.address, &mint);
        if delta == 0 {
            return None;
        }
        let counterparty = token_counterparty(self.metadata, self.address, &mint, delta);
        Some(self.record(Parts {
            asset: token.asset(),
            direction: direction(delta, counterparty.as_deref(), self.address),
            amount: Amount::from_base_units(delta.unsigned_abs()),
            counterparty,
            is_decoded: has_token_transfer(self.tx),
            outcome: TxOutcome::Success,
        }))
    }

    /// The fee payer of a transaction that moved nothing else this build tracks
    /// still gets a record, because the fee left the account and a history
    /// without it does not add up. That is what `is_fee_only` decides.
    fn native_record(&self, is_fee_only: bool) -> Option<TxRecord> {
        let keys = &self.tx.transaction.message.account_keys;
        let index = keys.iter().position(|key| key.pubkey == self.address)?;
        let is_fee_payer = index == 0;
        let moved = native_delta(self.metadata, index, is_fee_payer);
        let self_transfer = native_self_transfer(self.tx, self.address);
        if moved == 0 && self_transfer.is_none() && !(is_fee_payer && is_fee_only) {
            return None;
        }
        let (direction, amount, counterparty, is_decoded) = if let Some(lamports) = self_transfer {
            (TxDirection::SelfTransfer, u128::from(lamports), Some(self.address.to_owned()), true)
        } else {
            let named = decoded_native_counterparty(self.tx, self.address);
            let is_decoded = named.is_some();
            let party =
                named.or_else(|| native_counterparty(self.metadata, keys, self.address, moved));
            (
                direction(moved, party.as_deref(), self.address),
                moved.unsigned_abs(),
                party,
                is_decoded,
            )
        };
        Some(self.record(Parts {
            asset: NATIVE,
            direction,
            amount: Amount::from_base_units(amount),
            counterparty,
            is_decoded,
            outcome: TxOutcome::Success,
        }))
    }

    fn record(&self, parts: Parts) -> TxRecord {
        let mut builder = TxRecord::builder(
            parts.asset,
            self.address_id,
            TxId::new(parts.asset.chain(), self.signature),
            parts.direction,
            parts.amount,
        )
        .outcome(parts.outcome)
        .detection(if parts.is_decoded { TxDetection::Full } else { TxDetection::BalanceDelta })
        .confirmed_at(self.tx.slot, self.tx.block_time, self.tip_slot);
        if let Some(fee) = self.fee {
            builder = builder.fee(fee);
        }
        if let Some(party) = parts.counterparty {
            builder = builder.counterparty(party);
        }
        builder.build()
    }
}

/// What one record says, over and above what [`Seen`] already holds.
struct Parts {
    asset: AssetId,
    direction: TxDirection,
    amount: Amount,
    counterparty: Option<String>,
    /// True when an instruction named the movement, false when only the balance
    /// sheet did.
    is_decoded: bool,
    outcome: TxOutcome,
}

/// The fee payer's lamport delta includes the fee, so stripping it leaves what
/// was transferred.
fn native_delta(metadata: &TransactionMeta, index: usize, is_fee_payer: bool) -> i128 {
    let fee = if is_fee_payer { i128::from(metadata.fee) } else { 0 };
    lamport_delta(metadata, index).saturating_add(fee)
}

/// How the lamports of the account at `index` moved, fee included.
fn lamport_delta(metadata: &TransactionMeta, index: usize) -> i128 {
    let pre = i128::from(metadata.pre_balances.get(index).copied().unwrap_or(0));
    let post = i128::from(metadata.post_balances.get(index).copied().unwrap_or(0));
    post.saturating_sub(pre)
}

fn direction(delta: i128, counterparty: Option<&str>, address: &str) -> TxDirection {
    if counterparty == Some(address) {
        return TxDirection::SelfTransfer;
    }
    if delta > 0 { TxDirection::Incoming } else { TxDirection::Outgoing }
}

fn sum_owned(balances: &[TokenBalance], owner: &str, mint: &str) -> i128 {
    balances
        .iter()
        .filter(|entry| entry.mint == mint && entry.owner.as_deref() == Some(owner))
        .map(|entry| i128::from(entry.ui_token_amount.amount))
        .fold(0i128, i128::saturating_add)
}

fn token_delta(metadata: &TransactionMeta, owner: &str, mint: &str) -> i128 {
    sum_owned(&metadata.post_token_balances, owner, mint).saturating_sub(sum_owned(
        &metadata.pre_token_balances,
        owner,
        mint,
    ))
}

/// The owner whose delta for the same mint has the opposite sign and the largest
/// size.
fn token_counterparty(
    metadata: &TransactionMeta,
    address: &str,
    mint: &str,
    our_delta: i128,
) -> Option<String> {
    let mut owners: Vec<&str> = Vec::new();
    for entry in metadata.pre_token_balances.iter().chain(metadata.post_token_balances.iter()) {
        if entry.mint != mint {
            continue;
        }
        if let Some(owner) = entry.owner.as_deref()
            && owner != address
            && !owners.contains(&owner)
        {
            owners.push(owner);
        }
    }
    owners
        .into_iter()
        .map(|owner| (owner, token_delta(metadata, owner, mint)))
        .filter(|(_, delta)| *delta != 0 && (*delta > 0) != (our_delta > 0))
        .max_by_key(|(_, delta)| delta.abs())
        .map(|(owner, _)| owner.to_owned())
}

/// The account whose lamport delta has the opposite sign and the largest size.
fn native_counterparty(
    metadata: &TransactionMeta,
    keys: &[ParsedAccountKey],
    address: &str,
    our_delta: i128,
) -> Option<String> {
    let mut best: Option<(&str, i128)> = None;
    for (index, key) in keys.iter().enumerate() {
        if key.pubkey == address {
            continue;
        }
        let delta = lamport_delta(metadata, index);
        if delta == 0 || (delta > 0) == (our_delta > 0) {
            continue;
        }
        if best.is_none_or(|(_, seen)| delta.abs() > seen.abs()) {
            best = Some((key.pubkey.as_str(), delta));
        }
    }
    best.map(|(key, _)| key.to_owned())
}

/// Every instruction, top level and inner, because a transfer through a program
/// appears only in the inner list.
fn all_instructions(tx: &TransactionRow) -> impl Iterator<Item = &ParsedInstruction> {
    let inner = tx.meta.iter().flat_map(|metadata| {
        metadata.inner_instructions.iter().flat_map(|group| group.instructions.iter())
    });
    tx.transaction.message.instructions.iter().chain(inner)
}

fn is_system_transfer(instruction: &ParsedInstruction) -> bool {
    instruction.program.as_deref() == Some("system")
        && instruction
            .parsed
            .as_ref()
            .is_some_and(|body| body.kind == "transfer" || body.kind == "transferWithSeed")
}

fn has_token_transfer(tx: &TransactionRow) -> bool {
    all_instructions(tx).any(|instruction| {
        matches!(instruction.program.as_deref(), Some("spl-token" | "spl-token-2022"))
            && instruction.parsed.as_ref().is_some_and(|body| {
                matches!(
                    body.kind.as_str(),
                    "transfer" | "transferChecked" | "transferCheckedWithFee"
                )
            })
    })
}

fn decoded_native_counterparty(tx: &TransactionRow, address: &str) -> Option<String> {
    for instruction in all_instructions(tx) {
        if !is_system_transfer(instruction) {
            continue;
        }
        let Some(info) = instruction.parsed.as_ref().and_then(|body| body.info.as_ref()) else {
            continue;
        };
        let source = info.source.as_deref();
        let destination = info.destination.as_deref();
        if source == Some(address) && destination != Some(address) {
            return destination.map(str::to_owned);
        }
        if destination == Some(address) && source != Some(address) {
            return source.map(str::to_owned);
        }
    }
    None
}

fn native_self_transfer(tx: &TransactionRow, address: &str) -> Option<u64> {
    all_instructions(tx).find_map(|instruction| {
        if !is_system_transfer(instruction) {
            return None;
        }
        let info = instruction.parsed.as_ref()?.info.as_ref()?;
        let is_self_transfer =
            info.source.as_deref() == Some(address) && info.destination.as_deref() == Some(address);
        is_self_transfer.then(|| info.lamports.unwrap_or(0))
    })
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;
    use crate::fixtures::tracked;
    use crate::pending::Tracked;
    use crate::rpc::{ParsedMessage, ParsedTransaction, TransactionFailure, UiTokenAmount};

    fn keys(list: &[&str]) -> Vec<ParsedAccountKey> {
        list.iter().map(|key| ParsedAccountKey { pubkey: (*key).to_owned() }).collect()
    }

    fn balance(owner: &str, mint: &str, amount: u64) -> TokenBalance {
        TokenBalance {
            mint: mint.to_owned(),
            owner: Some(owner.to_owned()),
            ui_token_amount: UiTokenAmount { amount },
        }
    }

    fn sample_metadata() -> TransactionMeta {
        TransactionMeta {
            err: None,
            fee: 5_000,
            pre_balances: Vec::new(),
            post_balances: Vec::new(),
            pre_token_balances: Vec::new(),
            post_token_balances: Vec::new(),
            inner_instructions: Vec::new(),
        }
    }

    #[test]
    fn the_fee_payers_delta_has_its_fee_stripped_out() {
        let metadata = TransactionMeta {
            pre_balances: vec![10_000, 0],
            post_balances: vec![4_000, 1_000],
            ..sample_metadata()
        };
        assert_eq!(
            native_delta(&metadata, 0, true),
            -1_000,
            "six thousand out, five of it the fee"
        );
        assert_eq!(native_delta(&metadata, 1, false), 1_000);
        assert_eq!(native_delta(&metadata, 9, false), 0, "an index the metadata does not cover");
    }

    #[test]
    fn a_token_delta_sums_every_account_the_owner_holds_for_the_mint() {
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let metadata = TransactionMeta {
            pre_token_balances: vec![balance("me", mint, 100), balance("me", mint, 50)],
            post_token_balances: vec![balance("me", mint, 20), balance("me", mint, 50)],
            ..sample_metadata()
        };
        assert_eq!(token_delta(&metadata, "me", mint), -80);
        assert_eq!(token_delta(&metadata, "you", mint), 0);
        assert_eq!(token_delta(&metadata, "me", "another mint"), 0);
    }

    #[test]
    fn a_counterparty_is_the_largest_opposite_movement() {
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let metadata = TransactionMeta {
            pre_token_balances: vec![
                balance("me", mint, 100),
                balance("small", mint, 0),
                balance("big", mint, 0),
            ],
            post_token_balances: vec![
                balance("me", mint, 0),
                balance("small", mint, 10),
                balance("big", mint, 90),
            ],
            ..sample_metadata()
        };
        assert_eq!(token_counterparty(&metadata, "me", mint, -100).as_deref(), Some("big"));

        let accounts = keys(&["me", "peer", "unchanged"]);
        let lamports = TransactionMeta {
            pre_balances: vec![10_000, 0, 7],
            post_balances: vec![4_000, 1_000, 7],
            ..metadata
        };
        assert_eq!(
            native_counterparty(&lamports, &accounts, "me", -1_000).as_deref(),
            Some("peer")
        );
        assert_eq!(native_counterparty(&lamports, &accounts, "me", 1_000), None);
    }

    #[test]
    fn a_direction_reads_the_sign_unless_the_counterparty_is_the_address_itself() {
        assert_eq!(direction(5, Some("peer"), "me"), TxDirection::Incoming);
        assert_eq!(direction(-5, Some("peer"), "me"), TxDirection::Outgoing);
        assert_eq!(direction(0, None, "me"), TxDirection::Outgoing);
        assert_eq!(direction(-5, Some("me"), "me"), TxDirection::SelfTransfer);
    }

    /// A stranger's transfer of nothing into the watched address's token account
    /// leaves its token balance where it was, so no record names the stranger.
    #[test]
    fn a_token_movement_of_nothing_is_never_recorded() {
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let poisoned = TransactionRow {
            slot: 100,
            block_time: Some(1_700_000_000),
            transaction: ParsedTransaction {
                message: ParsedMessage {
                    account_keys: keys(&["lookalike", "me"]),
                    instructions: Vec::new(),
                },
            },
            meta: Some(TransactionMeta {
                pre_balances: vec![10_000, 7],
                post_balances: vec![5_000, 7],
                pre_token_balances: vec![balance("lookalike", mint, 0), balance("me", mint, 9)],
                post_token_balances: vec![balance("lookalike", mint, 0), balance("me", mint, 9)],
                ..sample_metadata()
            }),
        };
        assert!(of_transaction(&poisoned, "sig", "me", 4, 100).is_empty());
    }

    /// A transaction from "me" to "peer" that ran and failed.
    fn failed_transfer() -> TransactionRow {
        TransactionRow {
            slot: 100,
            block_time: Some(1_700_000_000),
            transaction: ParsedTransaction {
                message: ParsedMessage {
                    account_keys: keys(&["me", "peer"]),
                    instructions: Vec::new(),
                },
            },
            meta: Some(TransactionMeta {
                err: Some(TransactionFailure::new("InstructionError")),
                ..sample_metadata()
            }),
        }
    }

    #[test]
    fn a_failed_transaction_gives_the_fee_payer_a_fee_and_everyone_else_nothing() {
        let tx = failed_transfer();
        let mine = of_transaction(&tx, "sig", "me", 7, 100);
        assert_eq!(mine.len(), 1);
        let record = mine.first().expect("one record");
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.amount, Amount::ZERO);
        assert_eq!(record.fee, Some(Amount::from_base_units(5_000)));
        assert!(of_transaction(&tx, "sig", "peer", 8, 100).is_empty());
    }

    #[test]
    fn a_failed_send_reads_the_same_from_its_follow_up_and_from_the_walk() {
        let walked = of_transaction(&failed_transfer(), "sig", "me", 7, 100);
        let walked = walked.first().expect("the fee payer has a record");
        for asset in [NATIVE, AssetId::Token(TokenId::UsdcSpl)] {
            let sent = Tracked { payer: String::from("me"), asset, ..tracked("sig", 7) };
            let followed = sent.landed_record(100, 100, true);
            assert_eq!(
                (followed.asset, followed.amount),
                (walked.asset, walked.amount),
                "{asset:?}: two records for one transaction leave two rows or a flipping amount"
            );
        }
    }

    #[test]
    fn a_transaction_with_no_metadata_gives_no_records() {
        let tx = TransactionRow {
            slot: 1,
            block_time: None,
            transaction: ParsedTransaction {
                message: ParsedMessage { account_keys: keys(&["me"]), instructions: Vec::new() },
            },
            meta: None,
        };
        assert!(of_transaction(&tx, "sig", "me", 7, 1).is_empty());
    }
}
