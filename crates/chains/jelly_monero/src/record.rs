use std::collections::BTreeMap;
use std::mem;

use jelly_core::{TxDetection, TxDirection, TxId, TxOutcome, TxRecord};
use monero_interface::ScannableBlock;
use monero_wallet::WalletOutput;
use monero_wallet::transaction::{Pruned, Transaction};

use crate::params::{ASSET, CHAIN, piconero};
use crate::sign::key_images_of;
use crate::state::{BlockRef, OwnedOutput, Recorded, ScanState};
use crate::subaddress::Subaddress;
use crate::watched::Watched;

const TARGET: &str = "jelly::monero::record";

/// What the scan of one block found.
pub(crate) struct Scanned {
    pub block: BlockRef,
    pub timestamp_secs: u64,
    pub outputs: Vec<(WalletOutput, [u8; 32])>,
    pub spends: Vec<SpendingTx>,
}

/// A transaction in a block, as far as spend detection cares.
pub(crate) struct SpendingTx {
    txid: [u8; 32],
    key_images: Vec<[u8; 32]>,
    fee_piconero: u64,
}

/// Pulls the key images and the fee out of a block's transactions before the
/// scanner consumes the block. A miner transaction generates coins and has no key
/// image, so it never appears here.
pub(crate) fn spends_in(block: &ScannableBlock) -> Vec<SpendingTx> {
    let mut spends = Vec::with_capacity(block.transactions.len());
    for (txid, transaction) in block.block.transactions.iter().zip(&block.transactions) {
        let key_images = key_images_of::<Pruned>(transaction);
        if key_images.is_empty() {
            continue;
        }
        spends.push(SpendingTx {
            txid: *txid,
            key_images,
            fee_piconero: pruned_fee_of(transaction),
        });
    }
    spends
}

/// Applies one scanned block to `state` and returns the records it owes, its
/// spends before its payments.
pub(crate) fn record_block(
    watched: &Watched,
    state: &mut ScanState,
    mut block: Scanned,
    tip: u64,
) -> Vec<TxRecord> {
    let mut receipts = record_receipts(state, &mut block);
    let mut records = record_spends(watched, state, &block, tip, &mut receipts);
    records.extend(payment_records(watched, &block, tip, &receipts));
    state.note_block(block.block);
    records
}

/// What one block paid this wallet, in piconero.
#[derive(Debug, Default)]
struct Receipts {
    /// What each transaction paid each account of this wallet. A spend is net of
    /// what came back to the account it spent from, and what it paid another
    /// account left this one as surely as a payment to anybody else.
    received_by_txid_and_account: BTreeMap<([u8; 32], u32), u64>,
    /// One payment is every output of one transaction that landed on one
    /// subaddress, newly found by this scan.
    payments: BTreeMap<([u8; 32], Subaddress), u64>,
}

/// Records what one block paid this wallet.
///
/// The block's outputs move into the state, so it leaves them empty.
fn record_receipts(state: &mut ScanState, block: &mut Scanned) -> Receipts {
    let mut receipts = Receipts::default();
    for (output, key_image) in mem::take(&mut block.outputs) {
        let amount = output.commitment().amount;
        let txid = output.transaction();
        let owned =
            OwnedOutput::new(output, block.block.height, block.timestamp_secs, key_image, None);
        let subaddress = owned.subaddress();
        let received_key = (txid, subaddress.major);
        match state.record_output(owned) {
            Recorded::Fresh => {
                add_to_total(&mut receipts.received_by_txid_and_account, received_key, amount);
                let payment = receipts.payments.entry((txid, subaddress)).or_default();
                *payment = payment.saturating_add(amount);
            }
            // A re-scanned block. The output is already counted, but a spend
            // inside this block still has to be reconciled against it.
            Recorded::Again => {
                add_to_total(&mut receipts.received_by_txid_and_account, received_key, amount);
            }
            Recorded::Burnt => tracing::warn!(
                target: TARGET,
                height = block.block.height,
                "ignoring a monero output whose key was already seen (the burning bug)"
            ),
        }
    }
    receipts
}

/// One incoming record for each payment to a watched subaddress.
fn payment_records(
    watched: &Watched,
    block: &Scanned,
    tip: u64,
    receipts: &Receipts,
) -> Vec<TxRecord> {
    let mut records = Vec::new();
    for (&(txid, subaddress), &amount) in &receipts.payments {
        let Some(address_id) = watched.address_id_of(subaddress) else { continue };
        records.push(
            TxRecord::builder(
                ASSET,
                address_id,
                TxId::new(CHAIN, hex::encode(txid)),
                TxDirection::Incoming,
                piconero(amount),
            )
            .confirmed_at(block.block.height, timestamp_of(block.timestamp_secs), tip)
            .outcome(TxOutcome::Success)
            .detection(TxDetection::Full)
            .build(),
        );
    }
    records
}

/// Records what one block spent of this wallet's coins.
///
/// A send in flight or awaiting scan acknowledgement is filed under the
/// subaddress it was made from, which is where the engine filed it when it went
/// out. Any other spend is filed under the subaddress that funded most of it.
///
/// A send returns its change to its account's primary address, so whatever a
/// spend paid there is taken out of `receipts.payments`. It is part of the spend,
/// which is netted against it, and not a payment in.
fn record_spends(
    watched: &Watched,
    state: &mut ScanState,
    block: &Scanned,
    tip: u64,
    receipts: &mut Receipts,
) -> Vec<TxRecord> {
    let mut records = Vec::new();
    for spending in &block.spends {
        let mut spent_piconero = 0u64;
        let mut funded_by: Option<(Subaddress, u64)> = None;
        for image in &spending.key_images {
            let Some(position) = state.mark_spent(image, spending.txid, block.block.height) else {
                continue;
            };
            let Some(output) = state.outputs().get(position) else { continue };
            let amount = output.amount();
            spent_piconero = spent_piconero.saturating_add(amount);
            if funded_by.is_none_or(|(_, largest)| amount > largest) {
                funded_by = Some((output.subaddress(), amount));
            }
        }
        if spent_piconero == 0 {
            continue;
        }
        let subaddress = state
            .sent_from(&spending.txid)
            .or_else(|| funded_by.map(|(subaddress, _)| subaddress))
            .unwrap_or(Subaddress::PRIMARY);
        receipts.payments.remove(&(spending.txid, subaddress.account_primary()));
        let Some(address_id) = watched.attribute(subaddress) else {
            tracing::warn!(
                target: TARGET,
                height = block.block.height,
                "a monero spend was detected on an account with no watched address"
            );
            continue;
        };
        let returned_piconero = receipts
            .received_by_txid_and_account
            .get(&(spending.txid, subaddress.major))
            .copied()
            .unwrap_or_default();
        let net_piconero =
            spent_piconero.saturating_sub(returned_piconero).saturating_sub(spending.fee_piconero);
        let direction =
            if net_piconero == 0 { TxDirection::SelfTransfer } else { TxDirection::Outgoing };
        records.push(
            TxRecord::builder(
                ASSET,
                address_id,
                TxId::new(CHAIN, hex::encode(spending.txid)),
                direction,
                piconero(net_piconero),
            )
            .fee(piconero(spending.fee_piconero))
            .confirmed_at(block.block.height, timestamp_of(block.timestamp_secs), tip)
            .outcome(TxOutcome::Success)
            .detection(TxDetection::Full)
            .build(),
        );
    }
    records
}

fn add_to_total(totals: &mut BTreeMap<([u8; 32], u32), u64>, key: ([u8; 32], u32), amount: u64) {
    let total = totals.entry(key).or_default();
    *total = total.saturating_add(amount);
}

/// The fee a pruned transaction paid, which is the form a scanned block carries.
/// Only a `RingCT` transaction states one.
const fn pruned_fee_of(transaction: &Transaction<Pruned>) -> u64 {
    match transaction {
        Transaction::V2 { proofs: Some(proofs), .. } => proofs.base.fee,
        Transaction::V2 { proofs: None, .. } | Transaction::V1 { .. } => 0,
    }
}

fn timestamp_of(block_time_secs: u64) -> Option<i64> {
    i64::try_from(block_time_secs).ok()
}
