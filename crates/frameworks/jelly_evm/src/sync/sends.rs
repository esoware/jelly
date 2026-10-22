use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, B256};
use jelly_core::{AssetId, TxConfirmation, TxId, TxRecord, WalletEvent, confirmations};

use super::balances::Explained;
use super::{PassInputs, TARGET};
use crate::client::Recheck;
use crate::cursor::ScanCursor;
use crate::error::EvmError;
use crate::ledger::{InFlight, Ledger, Observation, Tracked};
use crate::pending::store_pending;
use crate::record;
use crate::rpc::{Execution, Landing, Receipt};
use crate::watched::{Watched, address_of};

/// What a landed send took out of its sender's native balance.
///
/// A reverted one still paid its fee, and a token transfer or a send to itself
/// moves no native value. A receipt that does not state its fee or its outcome
/// cannot be netted.
fn spent_by(tracked: &Tracked, receipt: &Receipt) -> Explained {
    let Some(fee) = receipt.fee_wei() else { return Explained::Wholly };
    let value = match receipt.execution() {
        Execution::Unknown => return Explained::Wholly,
        Execution::Reverted => 0,
        Execution::Succeeded => {
            let is_native = matches!(tracked.asset, AssetId::Native(_));
            if is_native && tracked.to != tracked.from { tracked.amount.base_units() } else { 0 }
        }
    };
    fee.checked_add(value).map_or(Explained::Wholly, Explained::Spent)
}

/// One watched send at its nonce, with what it took out of its sender's native
/// balance once a receipt says. `None` while no receipt has answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Spend {
    pub(super) address_id: i64,
    pub(super) nonce: u64,
    pub(super) spent: Option<Explained>,
}

/// Returns every watched send at its nonce, priced where a receipt answered for
/// it.
///
/// A send ends only by fact: its receipt lands at depth, or another transaction
/// takes its nonce at depth. Until then it is watched however long that takes, and
/// its signed bytes go out again whenever the node asked about it does not know it.
///
/// Settlements stay in the durable ledger until a loaded checkpoint acknowledges
/// their records and its native nonce includes their spend. Queue acceptance is
/// not a commit, and a balance read can trail the receipt's node.
pub(super) async fn poll_sends(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    tip: u64,
    cursors: &mut BTreeMap<i64, ScanCursor>,
) -> Result<Vec<Spend>, EvmError> {
    let mut changed = BTreeSet::new();
    let recovery = {
        let mut ledger = inputs.ledger.lock();
        let owners: BTreeSet<Address> =
            ledger.settlements().iter().map(|entry| entry.tracked.from).collect();
        for owner in owners {
            let committed = address_of(watched, owner).and_then(|id| cursors.get(&id));
            if let Some(cursor) = committed
                && ledger.acknowledge(owner, cursor.settled_through, cursor.native_nonce)
            {
                changed.insert(owner);
            }
        }
        ledger.settlements().to_vec()
    };
    let mut recovered_spends = Vec::new();
    for entry in recovery {
        changed.insert(entry.tracked.from);
        let Some(address_id) = address_of(watched, entry.tracked.from) else { continue };
        let cursor = cursors.entry(address_id).or_default();
        if cursor.settled_through < entry.checkpoint {
            let record = match &entry.receipt {
                Some(receipt) => record::of_receipt(&entry.tracked, address_id, receipt, tip),
                None => record::of_replaced(&entry.tracked, address_id, entry.txid),
            };
            inputs.events.emit(WalletEvent::TxDetected(record)).await;
        }
        if let Some(receipt) = &entry.receipt {
            recovered_spends.push(Spend {
                address_id,
                nonce: entry.tracked.nonce,
                spent: Some(spent_by(&entry.tracked, receipt)),
            });
        }
    }
    let in_flight = inputs.ledger.lock().watching();
    let txids: Vec<B256> = in_flight.iter().map(|entry| entry.txid).collect();
    let receipts = inputs.client.receipts(&txids).await?;
    let recheck = recheck(inputs, &in_flight, &receipts, tip).await;

    let Verdicts { spends, touched, records, depths, landings, replaced, retired, resends } =
        judge(inputs, &inputs.ledger.lock(), watched, &in_flight, &receipts, recheck.as_ref(), tip);
    for record in records {
        inputs.events.emit(WalletEvent::TxDetected(record)).await;
    }
    for depth in depths {
        inputs.events.emit(WalletEvent::TxConfirmed(depth)).await;
    }
    resend(inputs, &resends).await;
    {
        let mut ledger = inputs.ledger.lock();
        for (txid, landing) in landings {
            ledger.land(txid, landing);
        }
        for (txid, receipt) in retired
            .into_iter()
            .map(|(txid, receipt)| (txid, Some(receipt)))
            .chain(replaced.into_iter().map(|txid| (txid, None)))
        {
            let Some(tracked) = ledger.entry(txid) else { continue };
            let Some(address_id) = address_of(watched, tracked.from) else { continue };
            let committed = cursors.get(&address_id).copied().unwrap_or_default().settled_through;
            ledger.retire(txid, receipt, committed)?;
        }
        for entry in ledger.settlements() {
            if let Some(address_id) = address_of(watched, entry.tracked.from) {
                let cursor = cursors.entry(address_id).or_default();
                cursor.settled_through = cursor.settled_through.max(entry.checkpoint);
            }
        }
    }
    changed.extend(touched);
    for owner in changed {
        store_pending(inputs.cursors, inputs.ledger, inputs.writing, watched, owner).await?;
    }
    recovered_spends.extend(spends);
    Ok(recovered_spends)
}

/// Asks one node, for every send no receipt answered for, whether its nonce was
/// taken at depth, whether it has a receipt after all, and whether it knows the
/// transaction.
///
/// The depth is the confirmation target, so a nonce counts as taken only where a
/// receipt would have retired the send that took it. A node that cannot answer
/// leaves those sends as they are for the pass, because nothing about them is
/// settled by its silence.
async fn recheck(
    inputs: &PassInputs<'_>,
    in_flight: &[InFlight],
    receipts: &[Option<Receipt>],
    tip: u64,
) -> Option<Recheck> {
    let unresolved: Vec<InFlight> = in_flight
        .iter()
        .zip(receipts)
        .filter(|(_, receipt)| receipt.as_ref().and_then(Receipt::landing).is_none())
        .map(|(entry, _)| *entry)
        .collect();
    if unresolved.is_empty() {
        return None;
    }
    let mut owners: Vec<Address> = unresolved.iter().map(|entry| entry.from).collect();
    owners.sort_unstable();
    owners.dedup();
    let txids: Vec<B256> = unresolved.iter().map(|entry| entry.txid).collect();
    let target = u64::from(inputs.params.confirmations_target.max(1));
    let at_block = tip.saturating_add(1).saturating_sub(target);
    match inputs.client.recheck(&owners, at_block, &txids).await {
        Ok(found) => Some(found),
        Err(error) => {
            tracing::warn!(
                target: TARGET,
                %error,
                "the unresolved sends could not be rechecked; they stay pending this pass"
            );
            None
        }
    }
}

/// Sends again the exact signed bytes of each send the rechecking node does not know.
///
/// A refusal is expected when another transaction already took the nonce, and
/// either way the send is judged on what the next pass finds.
async fn resend(inputs: &PassInputs<'_>, txids: &[B256]) {
    for txid in txids {
        let raw = inputs.ledger.lock().entry(*txid).and_then(Tracked::sent_bytes).cloned();
        let Some(raw) = raw else { continue };
        match inputs.client.send_raw(raw.as_ref()).await {
            Ok(_) => tracing::info!(
                target: TARGET,
                %txid,
                "a node did not know a pending send, so its signed bytes went out again"
            ),
            Err(error) => tracing::debug!(
                target: TARGET,
                %txid,
                %error,
                "a pending send a node did not know was refused when it went out again"
            ),
        }
    }
}

/// What one receipt poll decided, held back from the ledger until its records
/// are delivered.
#[derive(Debug, Default)]
struct Verdicts {
    spends: Vec<Spend>,
    touched: BTreeSet<Address>,
    records: Vec<TxRecord>,
    depths: Vec<TxConfirmation>,
    landings: Vec<(B256, Option<Landing>)>,
    /// Sends whose nonce another transaction took at depth, each with a replaced
    /// record.
    replaced: BTreeSet<B256>,
    /// Sends deep enough to stop watching, with receipts retained for replay.
    retired: Vec<(B256, Receipt)>,
    /// Sends the rechecking node does not know, whose signed bytes go out again.
    resends: Vec<B256>,
}

/// One rule ends a send without a receipt, whoever took its nonce.
///
/// A send of this wallet at the same nonce that landed at depth took it, and so
/// did any transaction the rechecking node counts in the account's nonce at depth
/// while it holds no receipt for this one.
fn judge(
    inputs: &PassInputs<'_>,
    ledger: &Ledger,
    watched: &[Watched],
    in_flight: &[InFlight],
    receipts: &[Option<Receipt>],
    recheck: Option<&Recheck>,
    tip: u64,
) -> Verdicts {
    let chain = inputs.params.chain;
    let target = inputs.params.confirmations_target;
    let landed_deep: BTreeSet<(Address, u64)> = in_flight
        .iter()
        .zip(receipts)
        .filter_map(|(entry, receipt)| {
            let height = receipt.as_ref()?.landing()?.height;
            let tracked = ledger.entry(entry.txid)?;
            (confirmations(Some(height), tip) >= target).then_some((tracked.from, tracked.nonce))
        })
        .collect();
    let is_taken = |tracked: &Tracked, txid: B256| {
        landed_deep.contains(&(tracked.from, tracked.nonce))
            || recheck.is_some_and(|found| {
                found.nonce_of(tracked.from).is_some_and(|nonce| nonce > tracked.nonce)
                    && found.sighting(txid).is_some_and(|sighting| !sighting.has_receipt)
            })
    };
    let mut verdicts = Verdicts::default();
    for (entry, receipt) in in_flight.iter().zip(receipts) {
        let Some(address_id) = address_of(watched, entry.from) else { continue };
        let Some(tracked) = ledger.entry(entry.txid) else { continue };
        let observed = tracked.observe(receipt.as_ref());
        if let Observation::Landed { receipt, landing, should_report } = observed {
            let depth = confirmations(Some(landing.height), tip);
            if should_report || depth >= target {
                verdicts.records.push(record::of_receipt(tracked, address_id, receipt, tip));
            }
            verdicts.depths.push(TxConfirmation {
                txid: TxId::new(chain, entry.txid.to_string()),
                confirmations: depth,
            });
            verdicts.landings.push((entry.txid, Some(landing)));
            if depth >= target {
                verdicts.retired.push((entry.txid, receipt.clone()));
            }
            verdicts.touched.insert(entry.from);
            verdicts.spends.push(Spend {
                address_id,
                nonce: tracked.nonce,
                spent: Some(spent_by(tracked, receipt)),
            });
            continue;
        }
        if observed == Observation::Unlanded {
            tracing::warn!(
                target: TARGET,
                txid = %entry.txid,
                "an inclusion was reorganised away and the transaction is pending again"
            );
            verdicts.depths.push(TxConfirmation {
                txid: TxId::new(chain, entry.txid.to_string()),
                confirmations: 0,
            });
            verdicts.landings.push((entry.txid, None));
            verdicts.touched.insert(entry.from);
        }
        if is_taken(tracked, entry.txid) {
            if verdicts.replaced.insert(entry.txid) {
                verdicts.records.push(record::of_replaced(tracked, address_id, entry.txid));
            }
            verdicts.touched.insert(entry.from);
            continue;
        }
        if recheck
            .and_then(|found| found.sighting(entry.txid))
            .is_some_and(|sighting| !sighting.is_known && !sighting.has_receipt)
        {
            verdicts.resends.push(entry.txid);
        }
        verdicts.spends.push(Spend { address_id, nonce: tracked.nonce, spent: None });
    }
    verdicts
}

#[cfg(test)]
mod tests {
    use jelly_core::{Amount, ChainId, TokenId};

    use super::*;
    use crate::fee::FeePair;
    use crate::fixtures::address;
    use crate::payload::Intent;

    #[test]
    fn a_landing_accounts_for_its_fee_and_only_the_native_value_it_paid_away() {
        let send = |asset: AssetId, to: Address| {
            let intent =
                Intent { from: address(1), asset, to, amount: Amount::from_base_units(1_000) };
            Tracked::held(intent, 0, FeePair::new(0, 0).expect("a pair"), 0)
        };
        let receipt = |status: Option<u8>| Receipt {
            transaction_hash: B256::ZERO,
            status,
            gas_used: Some(21_000),
            effective_gas_price: Some(10),
            l1_fee: None,
            block_number: Some(100),
            block_hash: None,
        };
        let native = AssetId::Native(ChainId::Ethereum);
        let paid = send(native, address(2));
        assert_eq!(spent_by(&paid, &receipt(Some(1))), Explained::Spent(211_000));
        assert_eq!(spent_by(&paid, &receipt(Some(0))), Explained::Spent(210_000), "a revert");
        assert_eq!(
            spent_by(&send(native, address(1)), &receipt(Some(1))),
            Explained::Spent(210_000),
            "a send to itself gets its value back"
        );
        assert_eq!(
            spent_by(&send(AssetId::Token(TokenId::UsdtErc20), address(2)), &receipt(Some(1))),
            Explained::Spent(210_000),
            "a token transfer moves no native value"
        );
        assert_eq!(spent_by(&paid, &receipt(None)), Explained::Wholly, "an unstated outcome");
    }
}
