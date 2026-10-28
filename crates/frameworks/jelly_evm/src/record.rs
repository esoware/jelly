use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use alloy::primitives::{Address, B256};
use jelly_core::{Amount, AssetId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord};

use crate::abi;
use crate::history::{InternalRow, NativeRow, TokenRow};
use crate::ledger::Tracked;
use crate::rpc::{Execution, LogEntry, Receipt};
use crate::sync::TARGET;
use crate::token::TokenTable;
use crate::units;
use crate::watched::Watched;

/// A reverted transaction is recorded as a failed spend with a real fee and no
/// movement, because it still consumed its nonce and paid for gas.
pub(crate) fn of_receipt(
    tracked: &Tracked,
    address_id: i64,
    receipt: &Receipt,
    tip: u64,
) -> TxRecord {
    let outcome = match receipt.execution() {
        Execution::Succeeded => TxOutcome::Success,
        Execution::Reverted => TxOutcome::Failed,
        Execution::Unknown => TxOutcome::Unknown,
    };
    let moved = if outcome == TxOutcome::Failed { Amount::ZERO } else { tracked.amount };
    let mut record = TxRecord::builder(
        tracked.asset,
        address_id,
        TxId::new(tracked.asset.chain(), receipt.transaction_hash.to_string()),
        sent_direction(tracked),
        moved,
    )
    .counterparty(tracked.to.to_string())
    .outcome(outcome);
    if let Some(fee) = receipt.fee_wei() {
        record = record.fee(Amount::from_base_units(fee));
    }
    if let Some(landing) = receipt.landing() {
        record = record.confirmed_at(landing.height, None, tip);
    }
    record.build()
}

/// A send whose nonce another transaction took never ran.
///
/// It moved nothing and cost nothing. The zero fee is written out, because a
/// stored row keeps its old fee when a record names none.
pub(crate) fn of_replaced(tracked: &Tracked, address_id: i64, txid: B256) -> TxRecord {
    TxRecord::builder(
        tracked.asset,
        address_id,
        TxId::new(tracked.asset.chain(), txid.to_string()),
        sent_direction(tracked),
        Amount::ZERO,
    )
    .counterparty(tracked.to.to_string())
    .outcome(TxOutcome::Replaced)
    .fee(Amount::ZERO)
    .build()
}

fn sent_direction(tracked: &Tracked) -> TxDirection {
    if tracked.to == tracked.from { TxDirection::SelfTransfer } else { TxDirection::Outgoing }
}

/// Keyed by address and the height the change was first seen at, because there
/// is no txid.
pub(crate) fn of_balance_delta(
    entry: &Watched,
    asset: AssetId,
    previous: u128,
    current: u128,
    tip: u64,
) -> TxRecord {
    let direction = if current > previous { TxDirection::Incoming } else { TxDirection::Outgoing };
    let delta = current.abs_diff(previous);
    TxRecord::builder(
        asset,
        entry.address_id,
        TxId::new(asset.chain(), format!("delta:{}:{tip}", entry.owner)),
        direction,
        Amount::from_base_units(delta),
    )
    .detection(TxDetection::BalanceDelta)
    .outcome(TxOutcome::Unknown)
    .confirmed_at(tip, None, tip)
    .build()
}

/// What one transaction moved for one watched address in one asset.
///
/// The store keeps one row per address, asset and txid, so every transfer a
/// transaction makes is summed into one record instead of each overwriting the
/// last.
#[derive(Debug)]
struct Moved {
    owner: Address,
    received: u128,
    sent: u128,
    to_self: u128,
    /// The first other party that paid the owner.
    sender: Option<Address>,
    /// The first other party the owner paid.
    recipient: Option<Address>,
    confirmed: Option<(u64, Option<i64>)>,
    fee: Option<u128>,
    is_too_wide: bool,
    /// The owner created a contract, which pays no named recipient.
    is_creation: bool,
    /// The transaction failed, so it moved nothing and still paid its fee.
    is_failed: bool,
}

/// Keyed by address id, asset and txid, the identity of a stored record.
type Moves = BTreeMap<(i64, AssetId, B256), Moved>;

impl Moved {
    const fn new(owner: Address) -> Self {
        Self {
            owner,
            received: 0,
            sent: 0,
            to_self: 0,
            sender: None,
            recipient: None,
            confirmed: None,
            fee: None,
            is_too_wide: false,
            is_creation: false,
            is_failed: false,
        }
    }

    /// Takes a transfer that names the owner on at least one side. No recipient
    /// means the owner created a contract.
    fn add(&mut self, from: Address, to: Option<Address>, value: u128) {
        let total = if from != self.owner {
            self.sender.get_or_insert(from);
            &mut self.received
        } else if to == Some(self.owner) {
            &mut self.to_self
        } else {
            match to {
                Some(recipient) => {
                    self.recipient.get_or_insert(recipient);
                }
                None => self.is_creation = true,
            }
            &mut self.sent
        };
        match total.checked_add(value) {
            Some(sum) => *total = sum,
            None => self.is_too_wide = true,
        }
    }

    /// The net movement: incoming when more came in than went out, outgoing when
    /// more went out, each by the difference. A zero net with only a sender is
    /// incoming, and one with only a recipient or a contract creation is outgoing.
    /// Anything else that nets to zero is a self-transfer. `None` when a sum doesn't
    /// fit in 128 bits.
    fn record(
        &self,
        (address_id, asset, txid): (i64, AssetId, B256),
        tip: u64,
    ) -> Option<TxRecord> {
        if self.is_too_wide {
            return None;
        }
        let (direction, amount, counterparty) =
            match (self.received.cmp(&self.sent), self.sender, self.recipient) {
                (Ordering::Greater, sender, _) | (Ordering::Equal, sender @ Some(_), None) => {
                    (TxDirection::Incoming, self.received.abs_diff(self.sent), sender)
                }
                (Ordering::Less, _, recipient) | (Ordering::Equal, None, recipient @ Some(_)) => {
                    (TxDirection::Outgoing, self.received.abs_diff(self.sent), recipient)
                }
                (Ordering::Equal, None, None) if self.is_creation => {
                    (TxDirection::Outgoing, 0, None)
                }
                (Ordering::Equal, _, _) => (
                    TxDirection::SelfTransfer,
                    self.sent.checked_add(self.to_self)?,
                    Some(self.owner),
                ),
            };
        let mut record = TxRecord::builder(
            asset,
            address_id,
            TxId::new(asset.chain(), txid.to_string()),
            direction,
            Amount::from_base_units(amount),
        );
        if self.is_failed {
            record = record.outcome(TxOutcome::Failed);
        }
        if let Some(party) = counterparty {
            record = record.counterparty(party.to_string());
        }
        if let Some((height, timestamp)) = self.confirmed {
            record = record.confirmed_at(height, timestamp, tip);
        }
        if let Some(fee) = self.fee {
            record = record.fee(Amount::from_base_units(fee));
        }
        Some(record.build())
    }
}

fn of_moves(moves: Moves, tip: u64) -> Vec<TxRecord> {
    let mut records = Vec::with_capacity(moves.len());
    for (identity, moved) in moves {
        if let Some(record) = moved.record(identity, tip) {
            records.push(record);
        } else {
            tracing::warn!(
                target: TARGET,
                txid = %identity.2,
                "the transfers of one transaction sum past 128 bits and were dropped"
            );
        }
    }
    records
}

/// A log counts toward every watched address it names, so a transfer between
/// two of this wallet's addresses produces a record for each.
///
/// A log from Bor's native token contract counts as the coin only when `native`
/// is set. Logs from an unknown contract, removed by a reorg, with no position, or
/// moving nothing are dropped. Anyone can make a contract log a transfer of nothing
/// from any address, which is how a lookalike address gets into a history.
pub(crate) fn of_logs(
    tokens: &TokenTable,
    native: Option<AssetId>,
    owners: &[Watched],
    logs: &[LogEntry],
    tip: u64,
) -> Vec<TxRecord> {
    let mut seen: HashSet<(B256, u64)> = HashSet::new();
    let mut moves = Moves::new();
    for log in logs {
        if log.removed {
            continue;
        }
        let asset = match native {
            Some(coin) if log.address == abi::BOR_NATIVE_TOKEN => coin,
            Some(_) | None => match tokens.at(log.address) {
                Some(token) => AssetId::Token(token.id()),
                None => continue,
            },
        };
        let Some(position) = log.position() else {
            tracing::warn!(
                target: TARGET,
                block = ?log.block_number,
                "a transfer log has no transaction hash or log index and was dropped"
            );
            continue;
        };
        if !seen.insert(position) {
            continue;
        }
        let decoded = match asset {
            AssetId::Native(_) => abi::decode_native_transfer(&log.topics, log.data.as_ref()),
            AssetId::Token(_) => abi::decode_transfer(&log.topics, log.data.as_ref()),
        };
        let Some(transfer) = decoded else { continue };
        let Ok(value) = units::to_u128("transfer value", transfer.value) else {
            tracing::warn!(
                target: TARGET,
                block = ?log.block_number,
                "a transfer value does not fit in 128 bits and was dropped"
            );
            continue;
        };
        if value == 0 {
            continue;
        }
        let timestamp = log.block_timestamp.and_then(|seconds| i64::try_from(seconds).ok());
        for entry in owners {
            if transfer.from != entry.owner && transfer.to != entry.owner {
                continue;
            }
            let moved = moves
                .entry((entry.address_id, asset, position.0))
                .or_insert_with(|| Moved::new(entry.owner));
            moved.add(transfer.from, Some(transfer.to), value);
            if let Some(height) = log.block_number {
                moved.confirmed = Some((height, timestamp));
            }
        }
    }
    of_moves(moves, tip)
}

/// Every native movement of one address, netted per transaction.
///
/// A transaction's own row and the internal calls that ran in it share its hash,
/// so they're summed into one record. The fee is on the transaction's own row, and
/// only its sender paid it. A failed transaction is recorded like a reverted
/// receipt, with no movement, a real fee and a failed outcome, and its internal
/// calls moved nothing either.
///
/// A fee in `receipt_fee_wei` replaces the one from the row's gas, which leaves out
/// the OP Stack's L1 data fee.
pub(crate) fn of_native(
    asset: AssetId,
    entry: &Watched,
    rows: &[NativeRow],
    internal: &[InternalRow],
    receipt_fee_wei: &HashMap<B256, u128>,
    tip: u64,
) -> Vec<TxRecord> {
    let mut moves = Moves::new();
    let mut reverted: HashSet<B256> = HashSet::new();
    for row in rows {
        let is_from_me = row.from == entry.owner;
        if !is_from_me && row.to != Some(entry.owner) {
            continue;
        }
        let moved = moves
            .entry((entry.address_id, asset, row.hash))
            .or_insert_with(|| Moved::new(entry.owner));
        moved.add(row.from, row.to, if row.is_failed { 0 } else { row.value });
        moved.confirmed = Some((row.block_number, Some(row.timestamp)));
        if row.is_failed {
            moved.is_failed = true;
            reverted.insert(row.hash);
        }
        if is_from_me && moved.fee.is_none() {
            moved.fee = receipt_fee_wei.get(&row.hash).copied().or_else(|| {
                let (gas_price, gas_used) = row.gas_price.zip(row.gas_used)?;
                gas_price.checked_mul(u128::from(gas_used))
            });
        }
    }
    for row in internal {
        let is_mine = row.from == entry.owner || row.to == Some(entry.owner);
        if !is_mine || row.is_failed || row.is_context_call || reverted.contains(&row.hash) {
            continue;
        }
        let moved = moves
            .entry((entry.address_id, asset, row.hash))
            .or_insert_with(|| Moved::new(entry.owner));
        moved.add(row.from, row.to, row.value);
        moved.confirmed.get_or_insert((row.block_number, Some(row.timestamp)));
    }
    of_moves(moves, tip)
}

/// A token row carries a fee only when the owner sent its transaction.
///
/// The row names who the tokens left, not who sent the transaction, and an
/// approved spender or a solver can move them in a transaction of its own. Only
/// `native`, the owner's transactions over the same blocks, says who sent it.
///
/// A row moving nothing is dropped, as [`of_logs`] drops its log. The fee of an
/// owner's transaction that moved no tokens is on its native row. A fee in
/// `receipt_fee_wei` replaces the one from the row's gas, as in [`of_native`].
pub(crate) fn of_tokens(
    tokens: &TokenTable,
    entry: &Watched,
    rows: &[TokenRow],
    native: &[NativeRow],
    receipt_fee_wei: &HashMap<B256, u128>,
    tip: u64,
) -> Vec<TxRecord> {
    let sent_by_owner: HashSet<B256> =
        native.iter().filter(|row| row.from == entry.owner).map(|row| row.hash).collect();
    let mut moves = Moves::new();
    for row in rows {
        let Some(token) = tokens.at(row.contract_address) else { continue };
        if row.value == 0 || (row.from != entry.owner && row.to != entry.owner) {
            continue;
        }
        let moved = moves
            .entry((entry.address_id, AssetId::Token(token.id()), row.hash))
            .or_insert_with(|| Moved::new(entry.owner));
        moved.add(row.from, Some(row.to), row.value);
        moved.confirmed = Some((row.block_number, Some(row.timestamp)));
        // Only the sender paid for the gas, and only once per transaction.
        if sent_by_owner.contains(&row.hash) && moved.fee.is_none() {
            moved.fee = receipt_fee_wei.get(&row.hash).copied().or_else(|| {
                let (gas_price, gas_used) = row.gas_price.zip(row.gas_used)?;
                gas_price.checked_mul(u128::from(gas_used))
            });
        }
    }
    of_moves(moves, tip)
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use alloy::primitives::{Bytes, U256};
    use jelly_core::{ChainId, TokenId};

    use super::*;
    use crate::fixtures::{address, token_table, watched};
    use crate::test_util::POLYGON;

    fn transfer_log(from: Address, to: Address, value: u128, index: u64) -> LogEntry {
        LogEntry {
            address: token_table().contracts()[0],
            topics: vec![abi::TRANSFER_TOPIC, from.into_word(), to.into_word()],
            data: Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec()),
            block_number: Some(100),
            block_timestamp: Some(1_700_000_000),
            transaction_hash: Some(B256::repeat_byte(index.to_le_bytes()[0])),
            log_index: Some(index),
            removed: false,
        }
    }

    /// Bor's log of a native movement, with zeroed balance words, in the same
    /// transaction as [`transfer_log`] at the same `index`.
    fn native_log(from: Address, to: Address, value: u128, index: u64) -> LogEntry {
        let mut data = U256::from(value).to_be_bytes::<32>().to_vec();
        data.extend([0u8; 4 * 32]);
        LogEntry {
            address: abi::BOR_NATIVE_TOKEN,
            topics: vec![
                abi::NATIVE_TRANSFER_TOPIC,
                abi::BOR_NATIVE_TOKEN.into_word(),
                from.into_word(),
                to.into_word(),
            ],
            data: Bytes::from(data),
            ..transfer_log(from, to, value, index)
        }
    }

    const POL: AssetId = AssetId::Native(ChainId::Polygon);

    fn polygon_tokens() -> TokenTable {
        TokenTable::for_params(&POLYGON).expect("the registry parses")
    }

    fn native_row(hash: u8, from: Address, to: Option<Address>, value: u128) -> NativeRow {
        NativeRow {
            block_number: 100,
            timestamp: 1_700_000_000,
            hash: B256::repeat_byte(hash),
            from,
            to,
            value,
            gas_used: Some(21_000),
            gas_price: Some(10),
            is_failed: false,
        }
    }

    fn internal_row(hash: u8, from: Address, to: Option<Address>, value: u128) -> InternalRow {
        InternalRow {
            block_number: 100,
            timestamp: 1_700_000_000,
            hash: B256::repeat_byte(hash),
            from,
            to,
            value,
            is_failed: false,
            is_context_call: false,
        }
    }

    fn native_records(rows: &[NativeRow], internal: &[InternalRow]) -> Vec<TxRecord> {
        let asset = AssetId::Native(ChainId::Ethereum);
        of_native(asset, &watched(1, 1), rows, internal, &HashMap::new(), 100)
    }

    fn token_row(hash: u8, from: Address, to: Address) -> TokenRow {
        TokenRow {
            block_number: 100,
            timestamp: 1_700_000_000,
            hash: B256::repeat_byte(hash),
            from,
            to,
            contract_address: token_table().contracts()[0],
            value: 500,
            gas_used: Some(60_000),
            gas_price: Some(10),
        }
    }

    #[test]
    fn a_transfer_between_two_watched_addresses_yields_a_row_for_each() {
        let members = vec![watched(1, 1), watched(2, 2)];
        let logs = vec![transfer_log(address(1), address(2), 500, 7)];
        let records = of_logs(&token_table(), None, &members, &logs, 100);
        assert_eq!(records.len(), 2);
        let sender = records.first().expect("the sending row");
        assert_eq!(sender.address_id, 1);
        assert_eq!(sender.direction, TxDirection::Outgoing);
        assert_eq!(sender.counterparty.as_deref(), Some(address(2).to_string().as_str()));
        let receiver = records.get(1).expect("the receiving row");
        assert_eq!(receiver.address_id, 2);
        assert_eq!(receiver.direction, TxDirection::Incoming);
        assert_eq!(receiver.amount.base_units(), 500);
        assert_eq!(receiver.asset, AssetId::Token(TokenId::LinkErc20));
        assert_eq!(receiver.detection, TxDetection::Full);
        assert_eq!(receiver.timestamp, Some(1_700_000_000));
        assert_eq!(receiver.fee, None, "a log carries no fee");
    }

    #[test]
    fn a_self_transfer_is_one_row_and_a_duplicate_log_is_dropped() {
        let members = vec![watched(1, 1)];
        let log = transfer_log(address(1), address(1), 5, 3);
        let records = of_logs(&token_table(), None, &members, &[log.clone(), log], 100);
        assert_eq!(records.len(), 1);
        assert_eq!(records.first().map(|record| record.direction), Some(TxDirection::SelfTransfer));
    }

    #[test]
    fn two_transfers_in_one_transaction_are_summed_into_one_record() {
        let members = vec![watched(1, 1)];
        let first = transfer_log(address(2), address(1), 2_000_000, 4);
        let mut second = transfer_log(address(2), address(1), 3_000_000, 5);
        second.transaction_hash = first.transaction_hash;
        let records = of_logs(&token_table(), None, &members, &[first, second], 100);

        let mut identities = HashSet::new();
        for record in &records {
            assert!(
                identities.insert((record.address_id, record.asset, record.txid.clone())),
                "the store keeps one row per address, asset and txid, so this one overwrites the \
                 other: {records:?}"
            );
        }
        let total: u128 = records.iter().map(|record| record.amount.base_units()).sum();
        assert_eq!(total, 5_000_000);
    }

    #[test]
    fn transfers_both_ways_in_one_transaction_net_to_the_difference() {
        let members = vec![watched(1, 1)];
        let paid = transfer_log(address(1), address(2), 3_000_000, 4);
        let mut refunded = transfer_log(address(3), address(1), 5_000_000, 5);
        refunded.transaction_hash = paid.transaction_hash;
        let records = of_logs(&token_table(), None, &members, &[paid, refunded], 100);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("the one record");
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount.base_units(), 2_000_000);
        assert_eq!(record.counterparty.as_deref(), Some(address(3).to_string().as_str()));
    }

    /// A third party's `transferFrom(owner, lookalike, 0)` logs a transfer of
    /// nothing out of the owner, as a stranger's transfer of nothing logs one in.
    #[test]
    fn a_transfer_of_nothing_either_way_is_never_recorded() {
        let members = vec![watched(1, 1)];
        let poisoned = [
            transfer_log(address(1), address(2), 0, 4),
            transfer_log(address(2), address(1), 0, 5),
        ];
        assert!(of_logs(&token_table(), None, &members, &poisoned, 100).is_empty());

        let mut paid = transfer_log(address(1), address(2), 0, 6);
        paid.transaction_hash = Some(B256::repeat_byte(9));
        let mut real = transfer_log(address(1), address(3), 700, 7);
        real.transaction_hash = paid.transaction_hash;
        let records = of_logs(&token_table(), None, &members, &[paid, real], 100);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("the real transfer");
        assert_eq!(record.amount.base_units(), 700);
        assert_eq!(
            record.counterparty.as_deref(),
            Some(address(3).to_string().as_str()),
            "the zero transfer beside it names no counterparty"
        );
    }

    #[test]
    fn an_indexed_token_row_moving_nothing_is_never_recorded() {
        let mut poisoned = token_row(3, address(1), address(2));
        poisoned.value = 0;
        let mut inbound = token_row(4, address(2), address(1));
        inbound.value = 0;
        let own = native_row(3, address(1), Some(token_table().contracts()[0]), 0);
        let records = of_tokens(
            &token_table(),
            &watched(1, 1),
            &[poisoned, inbound],
            from_ref(&own),
            &HashMap::new(),
            100,
        );
        assert!(records.is_empty(), "{records:?}");

        let natives = native_records(&[own], &[]);
        assert_eq!(
            natives.first().and_then(|record| record.fee).map(Amount::base_units),
            Some(210_000),
            "the owner's own call still records its fee on the native row"
        );
    }

    #[test]
    fn removed_foreign_positionless_and_oversized_logs_are_all_dropped() {
        let members = vec![watched(1, 1)];
        let mut removed = transfer_log(address(1), address(2), 5, 1);
        removed.removed = true;
        let mut foreign = transfer_log(address(1), address(2), 5, 2);
        foreign.address = address(0xee);
        let mut nowhere = transfer_log(address(1), address(2), 5, 3);
        nowhere.transaction_hash = None;
        nowhere.log_index = None;
        let mut oversized = transfer_log(address(1), address(2), 5, 4);
        oversized.data = Bytes::from(U256::MAX.to_be_bytes::<32>().to_vec());
        let records =
            of_logs(&token_table(), None, &members, &[removed, foreign, nowhere, oversized], 100);
        assert!(records.is_empty(), "{records:?}");
    }

    #[test]
    fn a_native_transfer_log_is_a_full_native_record_on_a_network_that_writes_them() {
        let members = vec![watched(1, 1)];
        let logs = [native_log(address(2), address(1), 7_000, 3)];
        let records = of_logs(&polygon_tokens(), Some(POL), &members, &logs, 100);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("the record");
        assert_eq!(record.asset, POL);
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount.base_units(), 7_000);
        assert_eq!(record.counterparty.as_deref(), Some(address(2).to_string().as_str()));
        assert_eq!(record.detection, TxDetection::Full);
        assert_eq!(record.height, Some(100));
        assert_eq!(record.timestamp, Some(1_700_000_000));
        assert_eq!(record.fee, None, "the log says nothing of who paid for the gas");

        assert!(
            of_logs(&polygon_tokens(), None, &members, &logs, 100).is_empty(),
            "a network whose node writes no such log never reads one as its coin"
        );
    }

    /// A swap that pays tokens in and coin out leaves one record per asset under
    /// the same txid, which the store keeps apart.
    #[test]
    fn a_transaction_moving_coin_and_tokens_yields_a_record_for_each_asset() {
        let members = vec![watched(1, 1)];
        let tokens = polygon_tokens();
        let mut paid = transfer_log(address(1), address(2), 500, 4);
        paid.address = tokens.contracts()[0];
        let mut received = native_log(address(2), address(1), 9_000, 5);
        received.transaction_hash = paid.transaction_hash;
        let records = of_logs(&tokens, Some(POL), &members, &[paid, received], 100);
        assert_eq!(records.len(), 2, "{records:?}");
        let of_asset = |asset: AssetId| {
            records.iter().find(|record| record.asset == asset).expect("a record for the asset")
        };
        assert_eq!(of_asset(POL).direction, TxDirection::Incoming);
        assert_eq!(of_asset(POL).amount.base_units(), 9_000);
        let token = of_asset(AssetId::Token(TokenId::LinkPolygon));
        assert_eq!(token.direction, TxDirection::Outgoing);
        assert_eq!(token.txid, of_asset(POL).txid);
    }

    #[test]
    fn a_native_log_that_does_not_decode_or_names_no_watched_address_is_dropped() {
        let members = vec![watched(1, 1)];
        let mut truncated = native_log(address(2), address(1), 5, 1);
        truncated.data = Bytes::from(U256::from(5u64).to_be_bytes::<32>().to_vec());
        let stranger = native_log(address(2), address(3), 5, 2);
        let records = of_logs(&polygon_tokens(), Some(POL), &members, &[truncated, stranger], 100);
        assert!(records.is_empty(), "{records:?}");
    }

    #[test]
    fn a_refund_inside_a_send_nets_into_one_outgoing_record_with_one_fee() {
        let sent = native_row(9, address(1), Some(address(2)), 1_000);
        let refund = internal_row(9, address(2), Some(address(1)), 300);
        let records = native_records(&[sent], &[refund]);
        assert_eq!(records.len(), 1, "one transaction is one stored row: {records:?}");
        let record = records.first().expect("the record");
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.amount.base_units(), 700);
        assert_eq!(record.fee.map(Amount::base_units), Some(210_000));
        assert_eq!(record.counterparty.as_deref(), Some(address(2).to_string().as_str()));
        assert_eq!(record.outcome, TxOutcome::Success);
        assert_eq!(record.detection, TxDetection::Full);
    }

    #[test]
    fn coin_a_contract_sends_is_incoming_summed_and_carries_no_fee() {
        let payout = internal_row(4, address(3), Some(address(1)), 500);
        let second = internal_row(4, address(3), Some(address(1)), 250);
        let records = native_records(&[], &[payout, second]);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("the record");
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount.base_units(), 750);
        assert_eq!(record.fee, None, "whoever called the contract paid for the gas");
        assert_eq!(record.counterparty.as_deref(), Some(address(3).to_string().as_str()));
        assert_eq!(record.height, Some(100));
        assert_eq!(record.timestamp, Some(1_700_000_000));
    }

    #[test]
    fn calls_that_failed_moved_nothing_or_ran_in_their_callers_context_are_left_out() {
        let mut failed = internal_row(1, address(3), Some(address(1)), 500);
        failed.is_failed = true;
        let mut borrowed = internal_row(2, address(3), Some(address(1)), 500);
        borrowed.is_context_call = true;
        assert!(native_records(&[], &[failed, borrowed]).is_empty());

        let mut reverted = native_row(5, address(1), Some(address(2)), 1_000);
        reverted.is_failed = true;
        let inside = internal_row(5, address(2), Some(address(1)), 300);
        let records = native_records(&[reverted], &[inside]);
        assert_eq!(records.len(), 1, "{records:?}");
        let record = records.first().expect("the failed send");
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.amount.base_units(), 0, "a failed transaction moved nothing");
        assert_eq!(record.fee.map(Amount::base_units), Some(210_000), "and still paid its fee");
        assert_eq!(record.direction, TxDirection::Outgoing);
    }

    #[test]
    fn a_contract_creation_is_outgoing_to_nobody_and_a_send_to_itself_is_a_self_transfer() {
        let created = native_records(&[native_row(6, address(1), None, 0)], &[]);
        let record = created.first().expect("the creation");
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.counterparty, None);
        assert_eq!(record.fee.map(Amount::base_units), Some(210_000));

        let itself = native_records(&[native_row(7, address(1), Some(address(1)), 40)], &[]);
        let record = itself.first().expect("the self-transfer");
        assert_eq!(record.direction, TxDirection::SelfTransfer);
        assert_eq!(record.amount.base_units(), 40);
    }

    #[test]
    fn a_token_record_carries_a_fee_only_for_a_transaction_the_owner_sent() {
        let contract = token_table().contracts()[0];
        let sent = token_row(3, address(1), address(2));
        let pulled = token_row(4, address(1), address(2));
        let own = native_row(3, address(1), Some(contract), 0);
        let records = of_tokens(
            &token_table(),
            &watched(1, 1),
            &[sent, pulled],
            &[own],
            &HashMap::new(),
            100,
        );
        let fee_of = |hash: u8| {
            let txid = B256::repeat_byte(hash).to_string();
            records.iter().find(|record| record.txid.value() == txid).and_then(|record| record.fee)
        };
        assert_eq!(fee_of(3).map(Amount::base_units), Some(600_000));
        assert_eq!(fee_of(4), None, "a spender or a solver sent that transaction and paid for it");
    }

    #[test]
    fn missing_or_invalid_indexed_gas_fields_leave_sender_fees_unknown() {
        for field in ["gasUsed", "gasPrice"] {
            for (value, expected_fee) in [
                (None, None),
                (Some("invalid"), None),
                (Some(""), None),
                (Some("340282366920938463463374607431768211456"), None),
                (Some("0"), Some(Amount::ZERO)),
            ] {
                let mut row = serde_json::json!({
                    "blockNumber": "100",
                    "timeStamp": "1700000000",
                    "hash": B256::repeat_byte(3),
                    "from": address(1),
                    "to": address(2),
                    "contractAddress": token_table().contracts()[0],
                    "value": "500",
                    "gasUsed": "21000",
                    "gasPrice": "10",
                });
                let fields = row.as_object_mut().unwrap();
                match value {
                    Some(value) => {
                        fields.insert(field.to_owned(), serde_json::json!(value));
                    }
                    None => {
                        fields.remove(field);
                    }
                }
                let body = row.to_string();
                let native: NativeRow = serde_json::from_str(&body).unwrap();
                let token: TokenRow = serde_json::from_str(&body).unwrap();
                let native_records = native_records(from_ref(&native), &[]);
                let token_records = of_tokens(
                    &token_table(),
                    &watched(1, 1),
                    &[token],
                    &[native],
                    &HashMap::new(),
                    100,
                );
                assert_eq!(native_records.len(), 1);
                assert_eq!(token_records.len(), 1);
                assert_eq!(native_records[0].fee, expected_fee, "{field}: {value:?}");
                assert_eq!(token_records[0].fee, expected_fee, "{field}: {value:?}");
            }
        }
    }

    #[test]
    fn a_delta_record_says_it_only_saw_a_balance_change() {
        let entry = watched(7, 1);
        let asset = AssetId::Native(ChainId::Ethereum);
        let up = of_balance_delta(&entry, asset, 100, 300, 42);
        assert_eq!(up.direction, TxDirection::Incoming);
        assert_eq!(up.amount.base_units(), 200);
        assert_eq!(up.detection, TxDetection::BalanceDelta);
        assert_eq!(up.outcome, TxOutcome::Unknown);
        assert_eq!(up.height, Some(42));
        assert_eq!(up.confirmations, 1);

        let down = of_balance_delta(&entry, asset, 300, 100, 42);
        assert_eq!(down.direction, TxDirection::Outgoing);
        assert_eq!(down.amount.base_units(), 200);
        assert_eq!(up.txid.value(), format!("delta:{}:42", address(1)));
    }
}
