//! What each transaction of a wallet, or of one of its jetton wallets, adds to the
//! history.
//!
//! A send is not over when its own transaction is. A bounceable Gram message can bounce
//! back, and a jetton transfer returns an excess or a bounce under its query id, each in a
//! later transaction. [`Returns`] carries the sends that can still see such a return and
//! the returns read before their send, because a walk reads history newest first, so a
//! send's record is rewritten with what came back rather than a return reading as a
//! receipt of its own.

use jelly_core::{Amount, AssetId, TokenId, TxDirection, TxId, TxOutcome, TxRecord};
use serde::{Deserialize, Serialize};
use tonlib_core::types::TonAddress;

use crate::address::non_bounceable;
use crate::config::CHAIN;
use crate::error::TonError;
use crate::jetton::{Body, INTERNAL_TRANSFER, TRANSFER, read_body};
use crate::record::{Inbound, Tx};

/// A bounce or an excess lands seconds after the send it returns, so a send waits this
/// long for one, and a return this long for its send, before history settles them.
const RETURN_WINDOW_SECONDS: u32 = 3600;

/// The jetton wallet the owner holds under one registry master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) token: TokenId,
    pub(crate) wallet: TonAddress,
}

/// Part of the history cursor, so its field names are a stored format.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Returns {
    sends: Vec<Sent>,
    orphans: Vec<Orphan>,
}

/// A send as its record stands, kept while a return can still rewrite it, and for the
/// window after, so a walk that reads a return a second time knows it is not new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sent {
    txid: String,
    lt: u64,
    utime: u32,
    /// The tip the send was first read at, which stands in for its height.
    height: u64,
    asset: AssetId,
    direction: TxDirection,
    outcome: TxOutcome,
    amount: Amount,
    fee_nanogram: u64,
    counterparty: Option<String>,
    awaits: Vec<Await>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Await {
    part: Part,
    /// The txid of the transaction that returned this part.
    returned_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Part {
    /// A bounceable Gram message, which the account it paid may bounce.
    Bounce { to: String, value_nanogram: u64 },
    /// A jetton transfer, which returns an excess or a bounce under its query id.
    Jetton { query_id: u64, amount: Amount },
}

/// A return the walk read before its send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Orphan {
    back: Back,
    lt: u64,
    utime: u32,
    height: u64,
    txid: String,
    from: String,
    value_nanogram: u64,
    fee_nanogram: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Back {
    /// The Gram a jetton transfer did not spend, which its last jetton wallet returns.
    Excess { query_id: u64 },
    /// A transfer the sender's own jetton wallet refused, bounced to the sender.
    JettonBounce { query_id: u64 },
    /// An `internal_transfer` the recipient's jetton wallet refused, bounced to the
    /// sender's jetton wallet, which keeps the Gram it carried.
    RecipientBounce { query_id: u64 },
    /// A Gram message the account it paid bounced.
    Bounce,
}

impl Back {
    /// Whether the return reached the owner's own wallet, whose history holds the send,
    /// rather than one of its jetton wallets.
    const fn is_wallet_return(self) -> bool {
        match self {
            Self::Excess { .. } | Self::JettonBounce { .. } | Self::Bounce => true,
            Self::RecipientBounce { .. } => false,
        }
    }
}

impl Orphan {
    /// Whether this is what came back of `part`, which nothing has returned yet.
    fn is_return_of(&self, part: &Await) -> bool {
        part.returned_by.is_none()
            && match (self.back, &part.part) {
                (
                    Back::Excess { query_id }
                    | Back::JettonBounce { query_id }
                    | Back::RecipientBounce { query_id },
                    Part::Jetton { query_id: sent, .. },
                ) => query_id == *sent,
                (Back::Bounce, Part::Bounce { to, value_nanogram: sent }) => {
                    *to == self.from && *sent >= self.value_nanogram
                }
                (
                    Back::Excess { .. } | Back::JettonBounce { .. } | Back::RecipientBounce { .. },
                    Part::Bounce { .. },
                )
                | (Back::Bounce, Part::Jetton { .. }) => false,
            }
    }
}

impl Sent {
    fn absorbs(&self, orphan: &Orphan) -> bool {
        self.awaits.iter().any(|part| orphan.is_return_of(part))
    }

    /// Rewrites the send with what `orphan` brought back, charging only the fees the send
    /// actually lost.
    fn absorb(&mut self, orphan: &Orphan) -> Result<(), TonError> {
        let Self { awaits, amount, fee_nanogram, outcome, .. } = self;
        let part = awaits
            .iter_mut()
            .find(|part| orphan.is_return_of(part))
            .ok_or(TonError::Internal("a return was absorbed by a send it does not claim"))?;
        part.returned_by = Some(orphan.txid.clone());
        // What came back, net of the fee the return itself cost the wallet.
        let refund_nanogram = orphan.value_nanogram.saturating_sub(orphan.fee_nanogram);
        let lost_nanogram = orphan.fee_nanogram.saturating_sub(orphan.value_nanogram);
        match (&part.part, orphan.back) {
            (Part::Jetton { .. }, Back::Excess { .. }) => {
                *fee_nanogram =
                    add_u64(fee_nanogram.saturating_sub(refund_nanogram), lost_nanogram)?;
            }
            (Part::Jetton { amount: returned, .. }, Back::JettonBounce { .. }) => {
                *amount = amount.saturating_sub(*returned);
                *fee_nanogram =
                    add_u64(fee_nanogram.saturating_sub(refund_nanogram), lost_nanogram)?;
            }
            (Part::Jetton { amount: returned, .. }, Back::RecipientBounce { .. }) => {
                *amount = amount.saturating_sub(*returned);
            }
            (Part::Bounce { value_nanogram, .. }, Back::Bounce) => {
                *amount = amount.saturating_sub(Amount::from(*value_nanogram));
                // The bounced value is no longer paid out, and the part of it that did not
                // come back is a cost.
                *fee_nanogram = add_u64(
                    add_u64(*fee_nanogram, *value_nanogram)?.saturating_sub(refund_nanogram),
                    lost_nanogram,
                )?;
            }
            (Part::Jetton { .. }, Back::Bounce)
            | (
                Part::Bounce { .. },
                Back::Excess { .. } | Back::JettonBounce { .. } | Back::RecipientBounce { .. },
            ) => {
                return Err(TonError::Internal(
                    "a return was absorbed by a send it does not claim",
                ));
            }
        }
        let is_bounce = !matches!(orphan.back, Back::Excess { .. });
        if is_bounce && amount.is_zero() {
            *outcome = TxOutcome::Failed;
        }
        Ok(())
    }
}

/// One send's movement in one asset, before it becomes a record.
#[derive(Debug, Default)]
struct Movement {
    amount: u128,
    fee_nanogram: u64,
    targets: Vec<TonAddress>,
    awaits: Vec<Await>,
}

fn add_u128(left: u128, right: u128) -> Result<u128, TonError> {
    left.checked_add(right).ok_or(TonError::Protocol)
}

fn add_u64(left: u64, right: u64) -> Result<u64, TonError> {
    left.checked_add(right).ok_or(TonError::Protocol)
}

/// Reads one owner's transactions into records.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    pub(crate) address_id: i64,
    pub(crate) owner: &'a TonAddress,
    pub(crate) held: &'a [Held],
    pub(crate) tip: u64,
    /// The node's time, which decides whether a send is recent enough to see a return
    /// the walk has not read yet.
    pub(crate) sync_utime: u32,
}

impl Reader<'_> {
    fn held_token(&self, wallet: &TonAddress) -> Option<TokenId> {
        self.held.iter().find(|held| held.wallet == *wallet).map(|held| held.token)
    }

    /// The records one transaction of the owner's wallet adds or rewrites.
    pub(crate) fn wallet_tx(
        &self,
        tx: &Tx,
        returns: &mut Returns,
    ) -> Result<Vec<TxRecord>, TonError> {
        let mut records = self.expire(tx.utime, returns)?;
        match tx.inbound {
            Inbound::External => records.extend(self.send(tx, returns)?),
            Inbound::Internal { ref from, value_nanogram, is_bounced, ref body }
                if tx.outbound.is_empty() && !tx.phases.is_bounced =>
            {
                let back = match (is_bounced, read_body(body)) {
                    (true, Body::Bounced { op: TRANSFER, query_id })
                        if self.held_token(from).is_some() =>
                    {
                        Some(Back::JettonBounce { query_id })
                    }
                    (true, _) => Some(Back::Bounce),
                    // A notification from one of the owner's jetton wallets is the
                    // jetton's own movement, which that wallet's history records.
                    (false, Body::Notification) if self.held_token(from).is_some() => {
                        return Ok(records);
                    }
                    (false, Body::Excesses { query_id }) => Some(Back::Excess { query_id }),
                    (false, _) => None,
                };
                match back {
                    Some(back) => {
                        let orphan = Orphan {
                            back,
                            lt: tx.lt,
                            utime: tx.utime,
                            height: self.tip,
                            txid: tx.txid.clone(),
                            from: from.to_hex(),
                            value_nanogram,
                            fee_nanogram: tx.fee_nanogram,
                        };
                        records.extend(self.receive(orphan, returns)?);
                    }
                    None => records.push(self.native(tx)?),
                }
            }
            Inbound::Internal { .. } | Inbound::None => records.push(self.native(tx)?),
        }
        Ok(records)
    }

    /// The records one transaction of the owner's jetton wallet adds or rewrites.
    pub(crate) fn jetton_tx(
        &self,
        tx: &Tx,
        held: &Held,
        returns: &mut Returns,
    ) -> Result<Vec<TxRecord>, TonError> {
        let Inbound::Internal { ref from, is_bounced, ref body, .. } = tx.inbound else {
            return Ok(Vec::new());
        };
        match (is_bounced, read_body(body)) {
            (false, Body::InternalTransfer { amount, from: sender })
                if amount > 0 && !tx.phases.is_aborted =>
            {
                let direction = if sender == *self.owner {
                    TxDirection::SelfTransfer
                } else {
                    TxDirection::Incoming
                };
                let mut record = TxRecord::builder(
                    AssetId::Token(held.token),
                    self.address_id,
                    TxId::new(CHAIN, &tx.txid),
                    direction,
                    Amount::from_base_units(amount),
                )
                .confirmed_at(self.tip, Some(i64::from(tx.utime)), self.tip);
                // A mint names no sender.
                if sender != TonAddress::NULL {
                    record = record.counterparty(non_bounceable(&sender));
                }
                Ok(vec![record.build()])
            }
            (true, Body::Bounced { op: INTERNAL_TRANSFER, query_id }) => {
                let orphan = Orphan {
                    back: Back::RecipientBounce { query_id },
                    lt: tx.lt,
                    utime: tx.utime,
                    height: self.tip,
                    txid: tx.txid.clone(),
                    from: from.to_hex(),
                    value_nanogram: 0,
                    fee_nanogram: 0,
                };
                self.receive(orphan, returns)
            }
            (true | false, _) => Ok(Vec::new()),
        }
    }

    /// Settles what a completed walk of the owner's wallet leaves over. A wallet return
    /// still unclaimed has no send in the history, so it is the receipt it looks like,
    /// and anything a window old is let go.
    pub(crate) fn settle(&self, returns: &mut Returns) -> Result<Vec<TxRecord>, TonError> {
        let now = self.sync_utime;
        let records = returns
            .orphans
            .extract_if(.., |orphan| orphan.back.is_wallet_return())
            .map(|orphan| self.receipt(&orphan))
            .collect::<Result<Vec<_>, _>>()?;
        returns.orphans.retain(|orphan| orphan.utime.saturating_add(RETURN_WINDOW_SECONDS) >= now);
        returns.sends.retain(|sent| sent.utime.saturating_add(RETURN_WINDOW_SECONDS) >= now);
        Ok(records)
    }

    /// A wallet return more than a window newer than `utime` belongs to no send the walk
    /// has yet to read, so it settles as a receipt now and the cursor stays small.
    fn expire(&self, utime: u32, returns: &mut Returns) -> Result<Vec<TxRecord>, TonError> {
        let horizon = utime.saturating_add(RETURN_WINDOW_SECONDS);
        returns
            .orphans
            .extract_if(.., |orphan| orphan.back.is_wallet_return() && orphan.utime > horizon)
            .map(|orphan| self.receipt(&orphan))
            .collect()
    }

    fn receive(&self, orphan: Orphan, returns: &mut Returns) -> Result<Vec<TxRecord>, TonError> {
        let is_known = returns.sends.iter().any(|sent| {
            sent.awaits.iter().any(|part| part.returned_by.as_ref() == Some(&orphan.txid))
        }) || returns.orphans.iter().any(|known| known.txid == orphan.txid);
        if is_known {
            return Ok(Vec::new());
        }
        let claimant = returns
            .sends
            .iter_mut()
            .filter(|sent| sent.lt < orphan.lt && sent.absorbs(&orphan))
            .max_by_key(|sent| sent.lt);
        if let Some(sent) = claimant {
            sent.absorb(&orphan)?;
            Ok(vec![self.sent_record(sent)])
        } else {
            returns.orphans.push(orphan);
            Ok(Vec::new())
        }
    }

    fn send(&self, tx: &Tx, returns: &mut Returns) -> Result<Vec<TxRecord>, TonError> {
        let known: Vec<TxRecord> = returns
            .sends
            .iter()
            .filter(|sent| sent.txid == tx.txid)
            .map(|sent| self.sent_record(sent))
            .collect();
        if !known.is_empty() {
            return Ok(known);
        }
        let is_failed = tx.phases.is_aborted || tx.phases.is_skipped;
        let movements = self.movements(tx)?;
        let mut records = Vec::with_capacity(movements.len());
        for (asset, movement) in movements {
            let target = match movement.targets.as_slice() {
                [target] => Some(target.clone()),
                _ => None,
            };
            let mut sent = Sent {
                txid: tx.txid.clone(),
                lt: tx.lt,
                utime: tx.utime,
                height: self.tip,
                asset,
                direction: if target.as_ref() == Some(self.owner) {
                    TxDirection::SelfTransfer
                } else {
                    TxDirection::Outgoing
                },
                outcome: if is_failed { TxOutcome::Failed } else { TxOutcome::Success },
                amount: Amount::from_base_units(movement.amount),
                fee_nanogram: movement.fee_nanogram,
                counterparty: target.as_ref().map(non_bounceable),
                awaits: if is_failed { Vec::new() } else { movement.awaits },
            };
            let mut index = 0;
            while let Some(orphan) = returns.orphans.get(index) {
                if orphan.lt > sent.lt && sent.absorbs(orphan) {
                    let orphan = returns.orphans.remove(index);
                    sent.absorb(&orphan)?;
                } else {
                    index = index.saturating_add(1);
                }
            }
            records.push(self.sent_record(&sent));
            if !sent.awaits.is_empty()
                && sent.utime.saturating_add(RETURN_WINDOW_SECONDS) >= self.sync_utime
            {
                returns.sends.push(sent);
            }
        }
        Ok(records)
    }

    /// What one send moved in each asset. The transaction's own fee goes to the Gram
    /// movement when the send moved Gram, and to its first jetton otherwise.
    fn movements(&self, tx: &Tx) -> Result<Vec<(AssetId, Movement)>, TonError> {
        let mut native = Movement::default();
        let mut jettons: Vec<(TokenId, Movement)> = Vec::new();
        for message in &tx.outbound {
            let transfer = match read_body(&message.body) {
                // A transfer of no jettons moves only the Gram it carried.
                Body::Transfer { query_id, amount, destination } if amount > 0 => {
                    self.held_token(&message.to).map(|token| (token, query_id, amount, destination))
                }
                Body::Transfer { .. }
                | Body::InternalTransfer { .. }
                | Body::Notification
                | Body::Excesses { .. }
                | Body::Bounced { .. }
                | Body::Other => None,
            };
            if let Some((token, query_id, amount, destination)) = transfer {
                if !jettons.iter().any(|(held, _)| *held == token) {
                    jettons.push((token, Movement::default()));
                }
                let Some((_, movement)) = jettons.iter_mut().find(|(held, _)| *held == token)
                else {
                    return Err(TonError::Internal("a jetton movement went missing"));
                };
                movement.amount = add_u128(movement.amount, amount)?;
                movement.fee_nanogram = add_u64(
                    movement.fee_nanogram,
                    add_u64(message.value_nanogram, message.fee_nanogram)?,
                )?;
                movement.targets.push(destination);
                movement.awaits.push(Await {
                    part: Part::Jetton { query_id, amount: Amount::from_base_units(amount) },
                    returned_by: None,
                });
            } else {
                native.amount = add_u128(native.amount, u128::from(message.value_nanogram))?;
                native.fee_nanogram = add_u64(native.fee_nanogram, message.fee_nanogram)?;
                native.targets.push(message.to.clone());
                if message.is_bounceable {
                    native.awaits.push(Await {
                        part: Part::Bounce {
                            to: message.to.to_hex(),
                            value_nanogram: message.value_nanogram,
                        },
                        returned_by: None,
                    });
                }
            }
        }
        let mut movements: Vec<(AssetId, Movement)> = Vec::new();
        if !native.targets.is_empty() || jettons.is_empty() {
            native.fee_nanogram = add_u64(native.fee_nanogram, tx.fee_nanogram)?;
            movements.push((AssetId::Native(CHAIN), native));
        } else if let Some((_, first)) = jettons.first_mut() {
            first.fee_nanogram = add_u64(first.fee_nanogram, tx.fee_nanogram)?;
        }
        movements
            .extend(jettons.into_iter().map(|(token, movement)| (AssetId::Token(token), movement)));
        Ok(movements)
    }

    fn sent_record(&self, sent: &Sent) -> TxRecord {
        let mut record = TxRecord::builder(
            sent.asset,
            self.address_id,
            TxId::new(CHAIN, &sent.txid),
            sent.direction,
            sent.amount,
        )
        .fee(Amount::from(sent.fee_nanogram))
        .outcome(sent.outcome)
        .confirmed_at(sent.height, Some(i64::from(sent.utime)), self.tip);
        if let Some(counterparty) = &sent.counterparty {
            record = record.counterparty(counterparty);
        }
        record.build()
    }

    /// A wallet return no send claimed, as the receipt it is.
    fn receipt(&self, orphan: &Orphan) -> Result<TxRecord, TonError> {
        let from = TonAddress::from_hex_str(&orphan.from).map_err(|_| TonError::Protocol)?;
        let direction =
            if from == *self.owner { TxDirection::SelfTransfer } else { TxDirection::Incoming };
        Ok(TxRecord::builder(
            AssetId::Native(CHAIN),
            self.address_id,
            TxId::new(CHAIN, &orphan.txid),
            direction,
            Amount::from(orphan.value_nanogram),
        )
        .fee(Amount::from(orphan.fee_nanogram))
        .counterparty(non_bounceable(&from))
        .confirmed_at(orphan.height, Some(i64::from(orphan.utime)), self.tip)
        .build())
    }

    /// A transaction that is neither a send nor a return, netted into one Gram record.
    fn native(&self, tx: &Tx) -> Result<TxRecord, TonError> {
        let (incoming_nanogram, mut counterparty) = match tx.inbound {
            Inbound::Internal { ref from, value_nanogram, .. } => {
                (value_nanogram, Some(from.clone()))
            }
            Inbound::External | Inbound::None => (0, None),
        };
        let mut outgoing_nanogram = 0u64;
        let mut fee_nanogram = tx.fee_nanogram;
        for message in &tx.outbound {
            outgoing_nanogram = add_u64(outgoing_nanogram, message.value_nanogram)?;
            // `total_fees` counts only the share of forwarding fees collected here. The
            // rest travels in each message's `fwd_fee` and `ihr_fee`, and the account
            // pays it too.
            fee_nanogram = add_u64(fee_nanogram, message.fee_nanogram)?;
        }
        if let [only] = tx.outbound.as_slice() {
            counterparty = Some(only.to.clone());
        } else if !tx.outbound.is_empty() {
            counterparty = None;
        }
        let (mut direction, amount_nanogram) = if tx.phases.is_bounced {
            // The bounce returned the inbound value less the fees it paid, so the
            // account itself paid only the rest.
            fee_nanogram =
                add_u64(fee_nanogram, outgoing_nanogram)?.saturating_sub(incoming_nanogram);
            (TxDirection::Incoming, 0)
        } else if outgoing_nanogram > incoming_nanogram {
            (TxDirection::Outgoing, outgoing_nanogram.saturating_sub(incoming_nanogram))
        } else {
            (TxDirection::Incoming, incoming_nanogram.saturating_sub(outgoing_nanogram))
        };
        if counterparty.as_ref() == Some(self.owner) {
            direction = TxDirection::SelfTransfer;
        }
        let outcome = if tx.phases.is_bounced { TxOutcome::Failed } else { TxOutcome::Success };
        let mut record = TxRecord::builder(
            AssetId::Native(CHAIN),
            self.address_id,
            TxId::new(CHAIN, &tx.txid),
            direction,
            Amount::from(amount_nanogram),
        )
        .fee(Amount::from(fee_nanogram))
        .outcome(outcome)
        // The provider does not return the containing block, so the tip at detection
        // stands in for its height.
        .confirmed_at(self.tip, Some(i64::from(tx.utime)), self.tip);
        if let Some(counterparty) = counterparty {
            record = record.counterparty(non_bounceable(&counterparty));
        }
        Ok(record.build())
    }
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use tonlib_core::cell::Cell;
    use tonlib_core::tlb_types::block::msg_address::MsgAddress;
    use tonlib_core::tlb_types::primitives::either::{EitherRef, EitherRefLayout};
    use tonlib_core::tlb_types::tep::jetton::{
        JettonInternalTransferMessage, JettonTransferMessage,
    };
    use tonlib_core::tlb_types::tlb::TLB as _;

    use super::*;
    use crate::fixtures::{address, rows};
    use crate::record::AccountTx;

    const NATIVE: AssetId = AssetId::Native(CHAIN);
    const USDT: AssetId = AssetId::Token(TokenId::UsdtJetton);

    /// What a V5R1 wallet's send of four bounceable messages cost when all four bounced.
    ///
    /// It is charged its own fees, the forwarding of its four messages, and what the
    /// bounces did not bring back: 807689 + 4 * 44446 + 349443 - 104, where 104 is what
    /// the four returns brought in above their own fees.
    const BOUNCED_SEND_FEE_NANOTON: u128 = 1_334_812;

    fn usdt(wallet: &str) -> [Held; 1] {
        [Held { token: TokenId::UsdtJetton, wallet: address(wallet) }]
    }

    /// Reads `rows` the way a walk of the owner's wallet does.
    fn walk(reader: &Reader<'_>, returns: &mut Returns, rows: &[AccountTx]) -> Vec<TxRecord> {
        let mut records = Vec::new();
        for row in rows {
            let tx = row.read(reader.owner).unwrap();
            records.extend(reader.wallet_tx(&tx, returns).unwrap());
        }
        records
    }

    fn fee(record: &TxRecord) -> u128 {
        record.fee.map(Amount::base_units).unwrap()
    }

    fn one(records: Vec<TxRecord>) -> TxRecord {
        let [record] = records.try_into().unwrap();
        record
    }

    #[test]
    fn real_mainnet_history_decodes_from_cells_not_provider_summaries() {
        let owner = address("UQDv2YSmlrlLH3hLNOVxC8FcQf4F9eGNs4vb2zKma4txo6i3");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let rows = rows(include_str!("../tests/fixtures/history.json"));
        let first = rows[0].read(&owner).unwrap();
        let record = one(reader.wallet_tx(&first, &mut Returns::default()).unwrap());
        assert_eq!(
            record.txid.value(),
            "8c1e3433389d4957936307a62d43953f07372ea4d962f13579b036e359d61dec"
        );
        assert_eq!(record.amount.base_units(), 1_490_072_745);
        assert_eq!(fee(&record), 3_828_848);
        assert_eq!(record.direction, TxDirection::Outgoing);
        assert_eq!(record.outcome, TxOutcome::Success);
        assert_eq!(record.confirmations, 1);
        assert_eq!(first.previous, rows[1].transaction_id);
        let second = rows[1].read(&owner).unwrap();
        let self_transfer = one(reader.wallet_tx(&second, &mut Returns::default()).unwrap());
        assert_eq!(self_transfer.direction, TxDirection::SelfTransfer);
        assert_eq!(self_transfer.amount.base_units(), 300_000_000);
    }

    #[test]
    fn a_bounced_incoming_transfer_reads_as_a_failed_receipt_of_nothing() {
        // A real mainnet transaction: a bounceable transfer of 86492 nanogram reached an
        // undeployed account, its compute phase was skipped, and the bounce phase sent
        // 19825 back to the sender, so the account kept nothing.
        let row: AccountTx =
            serde_json::from_str(include_str!("../tests/fixtures/bounced.json")).unwrap();
        let owner = address("EQB61bsO2ekfVGrBkk8o8HWqoQkdbht3q2B9VywE4dnv-5Ko");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let tx = row.read(&owner).unwrap();
        let record = one(reader.wallet_tx(&tx, &mut Returns::default()).unwrap());
        assert_eq!(
            (record.outcome, record.amount.base_units()),
            (TxOutcome::Failed, 0),
            "expected the bounced deposit to be a failed transfer that credited nothing, but it \
             is a {:?} {:?} of {} nanogram, the bounce fees passed off as a receipt",
            record.outcome,
            record.direction,
            record.amount.base_units()
        );
    }

    #[test]
    fn an_external_send_of_a_zero_value_message_reads_as_a_success() {
        // A real mainnet V5R1 transaction whose action phase succeeded and created one
        // internal message carrying no value.
        let row: AccountTx =
            serde_json::from_str(include_str!("../tests/fixtures/zero_value.json")).unwrap();
        let owner = address("EQCapl10YPW2yGLt62420qxAZUBOxceNaj4PB-EUA8TzpXWj");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let tx = row.read(&owner).unwrap();
        let record = one(reader.wallet_tx(&tx, &mut Returns::default()).unwrap());
        assert_eq!(
            record.outcome,
            TxOutcome::Success,
            "expected the delivered zero-value transfer to be a success, but it is {:?}, so its \
             fee of {:?} nanogram is dropped from every total",
            record.outcome,
            record.fee.map(Amount::base_units)
        );
    }

    #[test]
    fn bounced_gram_messages_fail_their_send_and_charge_only_what_was_lost() {
        let owner = address("EQBOipe4nieujUcQ7G642_mvM_0NHoeK91DF6va-OpUKzUMn");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let rows = rows(include_str!("../tests/fixtures/native_bounces.json"));
        let record = one(walk(&reader, &mut returns, &rows));
        assert_eq!(
            record.txid.value(),
            "1f1239bd2879b346c70d112910750b11f4d635b2de41db6b23fe5168ecc8806d"
        );
        assert_eq!(
            (record.asset, record.direction, record.outcome),
            (NATIVE, TxDirection::Outgoing, TxOutcome::Failed)
        );
        assert_eq!(record.amount, Amount::ZERO);
        assert_eq!(fee(&record), BOUNCED_SEND_FEE_NANOTON);
        assert!(reader.settle(&mut returns).unwrap().is_empty());
    }

    #[test]
    fn a_bounce_read_after_its_send_rewrites_the_send_to_the_same_end() {
        let owner = address("EQBOipe4nieujUcQ7G642_mvM_0NHoeK91DF6va-OpUKzUMn");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let rows = rows(include_str!("../tests/fixtures/native_bounces.json"));
        let mut returns = Returns::default();
        let sent = one(walk(&reader, &mut returns, &rows[4..]));
        assert_eq!(
            (sent.outcome, sent.amount.base_units(), fee(&sent)),
            (TxOutcome::Success, 349_443, 985_473)
        );
        let rewrites = walk(&reader, &mut returns, &rows[..4]);
        assert_eq!(rewrites.len(), 4, "each bounce rewrites the send");
        assert!(rewrites.iter().all(|record| record.txid == sent.txid));
        let last = rewrites.last().unwrap();
        assert_eq!(
            (last.outcome, last.amount, fee(last)),
            (TxOutcome::Failed, Amount::ZERO, BOUNCED_SEND_FEE_NANOTON)
        );
    }

    #[test]
    fn only_a_send_within_the_window_waits_for_its_returns() {
        let owner = address("EQBOipe4nieujUcQ7G642_mvM_0NHoeK91DF6va-OpUKzUMn");
        let rows = rows(include_str!("../tests/fixtures/native_bounces.json"));
        let send = rows[4].read(&owner).unwrap();
        for (sync_utime, is_kept) in [
            (send.utime.saturating_add(RETURN_WINDOW_SECONDS), true),
            (send.utime.saturating_add(RETURN_WINDOW_SECONDS).saturating_add(1), false),
        ] {
            let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime };
            let mut returns = Returns::default();
            reader.wallet_tx(&send, &mut returns).unwrap();
            assert_eq!(returns.sends.len(), usize::from(is_kept), "{sync_utime}");
        }
    }

    #[test]
    fn a_jetton_send_is_charged_net_of_its_excess_and_its_bounce_fails_it() {
        // A V4R2 wallet's USDT history: a 15.99 USDT transfer its jetton wallet refused
        // and bounced, and a 0.75 USDT transfer that delivered and returned an excess.
        let owner = address("0:6B0D3FA7A07B0C116C947436FB46CB269C1E9FC4BA113001BF72C060404041A5");
        let held = usdt("0:26052FDC85FD7FA37BD4B16827F52FB1575002991943217EAA6FD78B549C3CAD");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let rows = rows(include_str!("../tests/fixtures/usdt_bounce_owner.json"));
        let [bounced, delivered] = walk(&reader, &mut returns, &rows).try_into().unwrap();
        assert_eq!(bounced.asset, USDT);
        assert_eq!((bounced.outcome, bounced.amount), (TxOutcome::Failed, Amount::ZERO));
        // 466266 + 50000000 attached + 105069 forwarding - (49866665 back - 44601).
        assert_eq!(fee(&bounced), 749_271);
        assert_eq!(delivered.asset, USDT);
        assert_eq!(
            (delivered.outcome, delivered.amount.base_units()),
            (TxOutcome::Success, 750_000)
        );
        // 448461 + 50000000 attached + 96357 forwarding - (49455594 excess - 66068).
        assert_eq!(fee(&delivered), 1_155_292);
        assert_eq!(
            delivered.counterparty.as_deref(),
            Some("UQCFJEP4WZ_mpdo0_kMEmsTgvrMHG7K_tWY16pQhKHwoOtFz")
        );
        assert!(reader.settle(&mut returns).unwrap().is_empty());
    }

    #[test]
    fn a_return_read_twice_is_not_counted_twice() {
        let owner = address("0:6B0D3FA7A07B0C116C947436FB46CB269C1E9FC4BA113001BF72C060404041A5");
        let held = usdt("0:26052FDC85FD7FA37BD4B16827F52FB1575002991943217EAA6FD78B549C3CAD");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let rows = rows(include_str!("../tests/fixtures/usdt_bounce_owner.json"));
        let mut returns = Returns::default();
        let first = walk(&reader, &mut returns, &rows);
        let again = walk(&reader, &mut returns, &rows);
        assert_eq!(again, first, "a walk that reads the same history again changes nothing");
        assert!(walk(&reader, &mut returns, from_ref(&rows[2])).is_empty());
        assert!(reader.settle(&mut returns).unwrap().is_empty());
    }

    #[test]
    fn notifications_and_excesses_of_registry_jettons_are_not_gram_receipts() {
        // The wallet-core test wallet's mainnet history: USDT arriving with a notification,
        // Gram funding, then two USDT transfers that each returned an excess.
        let owner = address("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_");
        let held = usdt("EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let rows = rows(include_str!("../tests/fixtures/usdt_owner.json"));
        let [commented, first, funding] = walk(&reader, &mut returns, &rows).try_into().unwrap();
        assert_eq!((commented.asset, commented.amount.base_units()), (USDT, 110_000));
        assert_eq!(fee(&commented), 7_458_233);
        assert_eq!((first.asset, first.amount.base_units()), (USDT, 120_000));
        assert_eq!(
            first.txid.value(),
            "d77e0d550b681ef30feb9fbb6e5ca0c67da5f55ef8606b57ddf4fefb07e9b342"
        );
        assert_eq!(fee(&first), 16_191_619, "the deployment and the excess both count");
        assert_eq!((funding.asset, funding.direction), (NATIVE, TxDirection::Incoming));
        assert_eq!(funding.amount.base_units(), 220_000_000);
        assert!(reader.settle(&mut returns).unwrap().is_empty());
    }

    #[test]
    fn only_the_jetton_wallets_of_registry_masters_count() {
        let owner = address("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_");
        let reader = Reader { address_id: 1, owner: &owner, held: &[], tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let rows = rows(include_str!("../tests/fixtures/usdt_owner.json"));
        let mut records = walk(&reader, &mut returns, &rows);
        records.extend(reader.settle(&mut returns).unwrap());
        assert!(records.iter().all(|record| record.asset == NATIVE));
        assert_eq!(records.len(), 6, "every transaction is a Gram record of its own");
        let notification = records.iter().find(|record| record.amount.base_units() == 1).unwrap();
        assert_eq!(notification.direction, TxDirection::Incoming);
    }

    #[test]
    fn a_jetton_wallet_records_what_arrived_and_nothing_its_owner_sent() {
        let owner = address("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_");
        let held = usdt("EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let mut records = Vec::new();
        for row in rows(include_str!("../tests/fixtures/usdt_wallet.json")) {
            let tx = row.read(&held[0].wallet).unwrap();
            records.extend(reader.jetton_tx(&tx, &held[0], &mut returns).unwrap());
        }
        let arrived = one(records);
        assert_eq!((arrived.asset, arrived.direction), (USDT, TxDirection::Incoming));
        assert_eq!(arrived.amount.base_units(), 230_000);
        assert_eq!(
            arrived.txid.value(),
            "a7232eb5272b7a9c9f14d325bdbd307ccb7f1d21562c267949d9cc14cb751321"
        );
        assert_eq!(
            arrived.counterparty.as_deref(),
            Some("UQBe6DtCpJZe8M4t-crMXe93JlEYgSl30S5OUuMSLOfeQa2r")
        );
        assert_eq!(arrived.fee, None);

        let refusing = usdt("0:26052FDC85FD7FA37BD4B16827F52FB1575002991943217EAA6FD78B549C3CAD");
        for row in rows(include_str!("../tests/fixtures/usdt_bounce_wallet.json")) {
            let tx = row.read(&refusing[0].wallet).unwrap();
            assert!(reader.jetton_tx(&tx, &refusing[0], &mut returns).unwrap().is_empty());
        }
    }

    #[test]
    fn a_transfer_the_recipients_wallet_refused_fails_the_send_without_a_refund() {
        // A wallet's 500000 USDT transfer whose recipient's jetton wallet bounced the
        // internal_transfer back to the sender's jetton wallet, which keeps the Gram.
        let owner = address("0:BABFC7B073E63D5C072463E7E3CF069767847B8937C92AC80086A323E9BF58BA");
        let held = usdt("0:5192CD62508B80067FF41E3377DCC99CE4CE831B8723D44B91122605538A051D");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let mut returns = Returns::default();
        let owner_rows = rows(include_str!("../tests/fixtures/recipient_bounce_owner.json"));
        let sent = one(walk(&reader, &mut returns, &owner_rows));
        assert_eq!((sent.outcome, sent.amount.base_units()), (TxOutcome::Success, 500_000_000_000));
        let mut rewrites = Vec::new();
        for row in rows(include_str!("../tests/fixtures/recipient_bounce_wallet.json")) {
            let tx = row.read(&held[0].wallet).unwrap();
            rewrites.extend(reader.jetton_tx(&tx, &held[0], &mut returns).unwrap());
        }
        let failed = one(rewrites);
        assert_eq!(failed.txid, sent.txid);
        assert_eq!((failed.outcome, failed.amount), (TxOutcome::Failed, Amount::ZERO));
        assert_eq!(fee(&failed), fee(&sent), "nothing came back, so the attachment is lost");
        assert_eq!(fee(&sent), 300_592_842);
    }

    #[test]
    fn a_zero_amount_jetton_movement_is_never_recorded() {
        let owner = address("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_");
        let held = usdt("EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let wallet_rows = rows(include_str!("../tests/fixtures/usdt_wallet.json"));
        let mut tx = wallet_rows[2].read(&held[0].wallet).unwrap();
        let empty = JettonInternalTransferMessage {
            query_id: 1,
            amount: 0u32.into(),
            from_address: address("UQBe6DtCpJZe8M4t-crMXe93JlEYgSl30S5OUuMSLOfeQa2r")
                .to_msg_address(),
            response_address: MsgAddress::NONE,
            fwd_amount: 0u32.into(),
            either_forward_payload: EitherRef {
                value: Cell::default().to_arc(),
                layout: EitherRefLayout::ToCell,
            },
        };
        if let Inbound::Internal { ref mut body, .. } = tx.inbound {
            *body = empty.to_cell().unwrap().to_arc();
        }
        assert!(reader.jetton_tx(&tx, &held[0], &mut Returns::default()).unwrap().is_empty());

        let owner_rows = rows(include_str!("../tests/fixtures/usdt_owner.json"));
        let mut send = owner_rows[3].read(&owner).unwrap();
        let nothing = JettonTransferMessage {
            query_id: 1,
            amount: 0u32.into(),
            destination: owner.to_msg_address(),
            response_destination: owner.to_msg_address(),
            custom_payload: None,
            forward_ton_amount: 1u32.into(),
            forward_payload: EitherRef {
                value: Cell::default().to_arc(),
                layout: EitherRefLayout::ToCell,
            },
        };
        send.outbound[0].body = nothing.to_cell().unwrap().to_arc();
        let record = one(reader.wallet_tx(&send, &mut Returns::default()).unwrap());
        assert_eq!((record.asset, record.amount.base_units()), (NATIVE, 100_000_000));
    }

    #[test]
    fn a_return_whose_send_would_be_a_window_older_settles_as_a_receipt() {
        let owner = address("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_");
        let held = usdt("EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC");
        let reader = Reader { address_id: 1, owner: &owner, held: &held, tip: 100, sync_utime: 0 };
        let rows = rows(include_str!("../tests/fixtures/usdt_owner.json"));
        let mut returns = Returns::default();
        let excess = rows[0].read(&owner).unwrap();
        assert!(reader.wallet_tx(&excess, &mut returns).unwrap().is_empty());
        let mut funding = rows[4].read(&owner).unwrap();
        funding.utime = excess.utime.saturating_sub(RETURN_WINDOW_SECONDS.saturating_add(1));
        let [receipt, _] = reader.wallet_tx(&funding, &mut returns).unwrap().try_into().unwrap();
        assert_eq!(
            (receipt.txid.value(), receipt.direction),
            (excess.txid.as_str(), TxDirection::Incoming)
        );
        assert_eq!(receipt.amount.base_units(), 96_947_968);
    }
}
