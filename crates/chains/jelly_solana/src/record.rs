//! Amounts come from the metadata's balance deltas, which cover inner and
//! multi-instruction transfers.
//!
//! Parsed instructions name the counterparty, mark a record as decoded rather than
//! balance-only, and show which lamports opened token accounts.

use jelly_core::{
    Amount, AssetId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord, TxRecordBuilder,
};

use crate::config::NATIVE;
use crate::rpc::{
    ParsedAccountKey, ParsedInfo, ParsedInstruction, TokenBalance, TransactionMeta, TransactionRow,
};
use crate::token::{SOLANA_TOKENS, SolanaToken};

const TRANSFERS: [&str; 2] = ["transfer", "transferWithSeed"];

/// The system instructions that pay lamports into an account.
///
/// The associated token program opens an account with `createAccount`, or with a
/// `transfer` of what it lacks when someone had already sent lamports to its address.
const FUNDING: [&str; 4] =
    ["createAccount", "createAccountWithSeed", "transfer", "transferWithSeed"];

/// One transaction, read for one watched address.
struct Seen<'a> {
    tx: &'a TransactionRow,
    metadata: &'a TransactionMeta,
    signature: &'a str,
    address: &'a str,
    address_id: i64,
    /// Set only for the fee payer.
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
        // A failed transaction moved nothing but still cost the fee payer the fee.
        if !is_fee_payer {
            return Vec::new();
        }
        return vec![
            seen.record(Parts {
                asset: NATIVE,
                direction: TxDirection::Outgoing,
                amount: Amount::ZERO,
                counterparty: None,
                is_decoded: true,
                outcome: TxOutcome::Failed,
            })
            .build(),
        ];
    }

    let mut records = Vec::new();
    for token in SOLANA_TOKENS {
        if let Some(record) = seen.token_record(token) {
            records.push(record);
        }
    }
    if let Some(record) = seen.native_record(&records) {
        records.push(record);
    }
    records
}

impl Seen<'_> {
    /// Carries the rent the address paid to open accounts of the token, zero included,
    /// so it replaces the figure a send's preview estimated.
    fn token_record(&self, token: SolanaToken) -> Option<TxRecord> {
        let mint = token.mint().to_string();
        let delta = token_delta(self.metadata, self.address, &mint);
        if delta == 0 {
            return None;
        }
        let counterparty = token_counterparty(self.metadata, self.address, &mint, delta);
        let rent_lamports =
            self.opened_accounts(&mint).map(|(_, lamports)| lamports).fold(0, u64::saturating_add);
        let record = self
            .record(Parts {
                asset: token.asset(),
                direction: direction(delta, counterparty.as_deref(), self.address),
                amount: Amount::from_base_units(delta.unsigned_abs()),
                counterparty,
                is_decoded: has_token_transfer(self.tx),
                outcome: TxOutcome::Success,
            })
            .rent(Amount::from(rent_lamports))
            .build();
        Some(record)
    }

    /// The fee payer of a transaction that moved no token this build tracks still gets
    /// a record, because the fee left the account and the history wouldn't add up
    /// without it.
    ///
    /// Rent that `token_records` carry, such as for a recipient's new token account,
    /// belongs to moving the tokens. It's added back to the SOL delta, and the accounts
    /// it opened are never named as the SOL counterparty.
    fn native_record(&self, token_records: &[TxRecord]) -> Option<TxRecord> {
        let keys = &self.tx.transaction.message.account_keys;
        let index = keys.iter().position(|key| key.pubkey == self.address)?;
        let is_fee_payer = index == 0;
        let mut opened = Vec::new();
        let mut rent_lamports = 0u64;
        for record in token_records {
            let AssetId::Token(token) = record.asset else { continue };
            for (account, lamports) in self.opened_accounts(token.contract()) {
                opened.push(account);
                rent_lamports = rent_lamports.saturating_add(lamports);
            }
        }
        let moved = native_delta(self.metadata, index, is_fee_payer)
            .saturating_add(i128::from(rent_lamports));
        let self_transfer = native_self_transfer(self.tx, self.address);
        let is_fee_only = token_records.is_empty();
        if moved == 0 && self_transfer.is_none() && !(is_fee_payer && is_fee_only) {
            return None;
        }
        let (direction, amount, counterparty, is_decoded) = if let Some(lamports) = self_transfer {
            (TxDirection::SelfTransfer, u128::from(lamports), Some(self.address.to_owned()), true)
        } else {
            let named = decoded_native_counterparty(self.tx, self.address, &opened);
            let is_decoded = named.is_some();
            let party = named
                .or_else(|| native_counterparty(self.metadata, keys, self.address, moved, &opened));
            (
                direction(moved, party.as_deref(), self.address),
                moved.unsigned_abs(),
                party,
                is_decoded,
            )
        };
        let record = self
            .record(Parts {
                asset: NATIVE,
                direction,
                amount: Amount::from_base_units(amount),
                counterparty,
                is_decoded,
                outcome: TxOutcome::Success,
            })
            .build();
        Some(record)
    }

    /// Each account of `mint` the transaction opened, meaning it holds the token after
    /// and didn't before, with the lamports the address paid into it, up to what the
    /// account kept.
    fn opened_accounts(&self, mint: &str) -> impl Iterator<Item = (&str, u64)> {
        let keys = &self.tx.transaction.message.account_keys;
        let metadata = self.metadata;
        metadata
            .post_token_balances
            .iter()
            .filter(move |after| after.mint == mint)
            .filter(move |after| {
                metadata
                    .pre_token_balances
                    .iter()
                    .all(|before| before.account_index != after.account_index)
            })
            .filter_map(move |opened| {
                let account = keys.get(opened.account_index)?.pubkey.as_str();
                let paid_lamports = all_instructions(self.tx)
                    .filter_map(|instruction| system_info(instruction, &FUNDING))
                    .filter(|info| info.source.as_deref() == Some(self.address))
                    .filter(|info| {
                        info.new_account.as_deref().or(info.destination.as_deref()) == Some(account)
                    })
                    .filter_map(|info| info.lamports)
                    .fold(0, u64::saturating_add);
                let kept_lamports =
                    u64::try_from(lamport_delta(metadata, opened.account_index)).unwrap_or(0);
                Some((account, paid_lamports.min(kept_lamports)))
            })
    }

    fn record(&self, parts: Parts) -> TxRecordBuilder {
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
        builder
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

/// The account whose lamport delta has the opposite sign and the largest size, other
/// than those in `skipped`.
fn native_counterparty(
    metadata: &TransactionMeta,
    keys: &[ParsedAccountKey],
    address: &str,
    our_delta: i128,
    skipped: &[&str],
) -> Option<String> {
    let mut best: Option<(&str, i128)> = None;
    for (index, key) in keys.iter().enumerate() {
        if key.pubkey == address || skipped.contains(&key.pubkey.as_str()) {
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

/// What `instruction` says when it is a system instruction of one of `kinds`.
fn system_info<'a>(instruction: &'a ParsedInstruction, kinds: &[&str]) -> Option<&'a ParsedInfo> {
    if instruction.program.as_deref() != Some("system") {
        return None;
    }
    let body = instruction.parsed.as_ref().filter(|body| kinds.contains(&body.kind.as_str()))?;
    body.info.as_ref()
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

fn decoded_native_counterparty(
    tx: &TransactionRow,
    address: &str,
    skipped: &[&str],
) -> Option<String> {
    for info in all_instructions(tx).filter_map(|instruction| system_info(instruction, &TRANSFERS))
    {
        let source = info.source.as_deref();
        let destination = info.destination.as_deref();
        if destination.is_some_and(|account| skipped.contains(&account)) {
            continue;
        }
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
    all_instructions(tx)
        .filter_map(|instruction| system_info(instruction, &TRANSFERS))
        .find(|info| {
            info.source.as_deref() == Some(address) && info.destination.as_deref() == Some(address)
        })
        .map(|info| info.lamports.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;
    use crate::fixtures::tracked;
    use crate::pending::Tracked;
    use crate::rpc::{
        ParsedBody, ParsedMessage, ParsedTransaction, TransactionFailure, UiTokenAmount,
    };

    fn keys(list: &[&str]) -> Vec<ParsedAccountKey> {
        list.iter().map(|key| ParsedAccountKey { pubkey: (*key).to_owned() }).collect()
    }

    fn balance(account_index: usize, owner: &str, mint: &str, amount: u64) -> TokenBalance {
        TokenBalance {
            account_index,
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
            pre_token_balances: vec![balance(1, "me", mint, 100), balance(2, "me", mint, 50)],
            post_token_balances: vec![balance(1, "me", mint, 20), balance(2, "me", mint, 50)],
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
                balance(1, "me", mint, 100),
                balance(2, "small", mint, 0),
                balance(3, "big", mint, 0),
            ],
            post_token_balances: vec![
                balance(1, "me", mint, 0),
                balance(2, "small", mint, 10),
                balance(3, "big", mint, 90),
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
            native_counterparty(&lamports, &accounts, "me", -1_000, &[]).as_deref(),
            Some("peer")
        );
        assert_eq!(native_counterparty(&lamports, &accounts, "me", 1_000, &[]), None);
        assert_eq!(native_counterparty(&lamports, &accounts, "me", -1_000, &["peer"]), None);
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
                pre_token_balances: vec![
                    balance(2, "lookalike", mint, 0),
                    balance(3, "me", mint, 9),
                ],
                post_token_balances: vec![
                    balance(2, "lookalike", mint, 0),
                    balance(3, "me", mint, 9),
                ],
                ..sample_metadata()
            }),
        };
        assert!(of_transaction(&poisoned, "sig", "me", 4, 100).is_empty());
    }

    fn system(kind: &str, info: ParsedInfo) -> ParsedInstruction {
        ParsedInstruction {
            program: Some(String::from("system")),
            parsed: Some(ParsedBody { kind: kind.to_owned(), info: Some(info) }),
        }
    }

    fn create_account(created: &str, lamports: u64) -> ParsedInstruction {
        system(
            "createAccount",
            ParsedInfo {
                source: Some(String::from("me")),
                destination: None,
                lamports: Some(lamports),
                new_account: Some(created.to_owned()),
            },
        )
    }

    fn transfer(destination: &str, lamports: u64) -> ParsedInstruction {
        system(
            "transfer",
            ParsedInfo {
                source: Some(String::from("me")),
                destination: Some(destination.to_owned()),
                lamports: Some(lamports),
                new_account: None,
            },
        )
    }

    /// A send of 10 USDC from "me", which pays the fee, to "peer", whose new token
    /// account "opened" is. "mine" is the sender's token account, and "other" holds
    /// no tokens.
    fn usdc_send(
        instructions: Vec<ParsedInstruction>,
        pre_balances: Vec<u64>,
        post_balances: Vec<u64>,
    ) -> TransactionRow {
        let mint = TokenId::UsdcSpl.contract();
        TransactionRow {
            slot: 100,
            block_time: Some(1_700_000_000),
            transaction: ParsedTransaction {
                message: ParsedMessage {
                    account_keys: keys(&["me", "mine", "opened", "other"]),
                    instructions,
                },
            },
            meta: Some(TransactionMeta {
                pre_balances,
                post_balances,
                pre_token_balances: vec![balance(1, "me", mint, 50)],
                post_token_balances: vec![balance(1, "me", mint, 40), balance(2, "peer", mint, 10)],
                ..sample_metadata()
            }),
        }
    }

    #[test]
    fn the_rent_of_the_account_a_token_send_opened_is_its_own_and_no_sol_movement() {
        for (funding, lamports_before, rent_lamports) in
            [(create_account("opened", 2_000), 0, 2_000), (transfer("opened", 1_999), 1, 1_999)]
        {
            let tx = usdc_send(
                vec![funding],
                vec![100_000, 2_000, lamports_before, 0],
                vec![93_000 + lamports_before, 2_000, 2_000, 0],
            );
            let records = of_transaction(&tx, "sig", "me", 7, 100);
            assert_eq!(records.len(), 1, "{records:?}");
            let record = records.first().expect("the token record");
            assert_eq!(record.asset, AssetId::Token(TokenId::UsdcSpl));
            assert_eq!(record.fee, Some(Amount::from_base_units(5_000)), "the network fee alone");
            assert_eq!(record.rent, Some(Amount::from_base_units(rent_lamports)));
        }
    }

    /// The rent went into an account someone had already sent a lamport to, so it is a
    /// plain transfer, and "other" got 1,000 lamports from the address in a way only the
    /// balances show.
    #[test]
    fn an_account_the_rent_opened_is_no_party_to_the_sol_the_address_moved_besides() {
        let tx = usdc_send(
            vec![transfer("opened", 1_999)],
            vec![100_000, 2_000, 1, 0],
            vec![92_001, 2_000, 2_000, 1_000],
        );
        let records = of_transaction(&tx, "sig", "me", 7, 100);
        assert_eq!(records.len(), 2, "{records:?}");
        let sol = records.iter().find(|record| record.asset == NATIVE).expect("a SOL record");
        assert_eq!(sol.direction, TxDirection::Outgoing);
        assert_eq!(sol.amount, Amount::from_base_units(1_000));
        assert_eq!(sol.counterparty.as_deref(), Some("other"));
        assert_eq!(sol.detection, TxDetection::BalanceDelta, "no instruction named the payment");
    }

    /// A swap that pays USDT for USDC and opens the address's own USDC account.
    #[test]
    fn the_rent_is_on_the_record_of_the_token_whose_account_it_opened() {
        let usdt = TokenId::UsdtSpl.contract();
        let usdc = TokenId::UsdcSpl.contract();
        let tx = TransactionRow {
            slot: 100,
            block_time: Some(1_700_000_000),
            transaction: ParsedTransaction {
                message: ParsedMessage {
                    account_keys: keys(&["me", "my_usdt", "my_usdc", "pool_usdt", "pool_usdc"]),
                    instructions: vec![create_account("my_usdc", 2_000)],
                },
            },
            meta: Some(TransactionMeta {
                pre_balances: vec![100_000, 2_000, 0, 2_000, 2_000],
                post_balances: vec![93_000, 2_000, 2_000, 2_000, 2_000],
                pre_token_balances: vec![
                    balance(1, "me", usdt, 50),
                    balance(3, "pool", usdt, 0),
                    balance(4, "pool", usdc, 50),
                ],
                post_token_balances: vec![
                    balance(1, "me", usdt, 40),
                    balance(2, "me", usdc, 10),
                    balance(3, "pool", usdt, 10),
                    balance(4, "pool", usdc, 40),
                ],
                ..sample_metadata()
            }),
        };
        let records = of_transaction(&tx, "sig", "me", 7, 100);
        assert_eq!(records.len(), 2, "a record of each token and none of SOL: {records:?}");
        let rent_of = |token: TokenId| {
            records.iter().find(|record| record.asset == AssetId::Token(token))?.rent
        };
        assert_eq!(rent_of(TokenId::UsdcSpl), Some(Amount::from_base_units(2_000)));
        assert_eq!(rent_of(TokenId::UsdtSpl), Some(Amount::ZERO));
    }

    /// A stake account or an escrow holds no tokens, and an account of a token this
    /// build does not track is no part of moving one it does.
    #[test]
    fn sol_paid_into_anything_but_a_new_account_of_a_moved_token_is_a_sol_movement() {
        for untracked in [None, Some("an untracked mint")] {
            let mut tx = usdc_send(
                vec![create_account("opened", 2_000), create_account("other", 50_000)],
                vec![100_000, 2_000, 0, 0],
                vec![43_000, 2_000, 2_000, 50_000],
            );
            if let Some(mint) = untracked
                && let Some(metadata) = tx.meta.as_mut()
            {
                metadata.post_token_balances.push(balance(3, "me", mint, 1));
            }
            let records = of_transaction(&tx, "sig", "me", 7, 100);
            assert_eq!(records.len(), 2, "{untracked:?}: {records:?}");
            let sol = records.iter().find(|record| record.asset == NATIVE).expect("a SOL record");
            assert_eq!(sol.direction, TxDirection::Outgoing);
            assert_eq!(sol.amount, Amount::from_base_units(50_000), "{untracked:?}");
            assert_eq!(sol.counterparty.as_deref(), Some("other"));
        }
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
