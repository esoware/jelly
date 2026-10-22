use std::collections::HashMap;

use bitcoin::{Script, Transaction, Txid};
use jelly_core::{Amount, AssetId, TxDirection, TxRecord};

use crate::coin::value_of;
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::prevout::PrevoutMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Confirmed {
    pub(crate) height: u32,
    /// The confirming block's time, when the header was fetched.
    pub(crate) unix_seconds: Option<i64>,
}

/// What one transaction means for one watched address.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TxSummary {
    direction: TxDirection,
    /// What arrived if incoming, what left for others without the fee if
    /// outgoing, and what came back if a self-transfer.
    amount: Amount,
    /// Only derivable when every input's previous output is known.
    fee: Option<Amount>,
    /// The first other-side address found, when any of those scripts has one.
    counterparty: Option<String>,
}

/// Classifies `tx` from the point of view of the address paying to `script_pubkey`,
/// one of the wallet's `own_scripts`.
///
/// Outputs are matched by the locking script behind any token prefix, so the coins
/// an output carries along with its tokens are counted like any other.
///
/// When several of the wallet's addresses pay into the transaction, each one's
/// record is what it lost or gained, and the fee is charged to the address of its
/// first input of the wallet's, so the records of one transaction add up to what
/// left the wallet.
fn summarise(
    params: &UtxoParams,
    tx: &Transaction,
    script_pubkey: &Script,
    own_scripts: &[&Script],
    prevouts: &PrevoutMap,
) -> Result<TxSummary, UtxoError> {
    let mut ours_in = Amount::ZERO;
    let mut total_in = Amount::ZERO;
    let mut is_every_input_known = !tx.input.is_empty();
    let mut sender = None;
    let mut payer: Option<&Script> = None;
    let mut has_other_payer = false;

    for txin in &tx.input {
        match prevouts.get(txin.previous_output) {
            Some(previous) => {
                let value = value_of(previous);
                total_in = add(total_in, value, "an input total")?;
                let locking = params.locking(previous.script_pubkey.as_script()).script;
                if locking == script_pubkey {
                    ours_in = add(ours_in, value, "an input total")?;
                } else if sender.is_none() {
                    sender = params.address_of(previous.script_pubkey.as_script());
                }
                if own_scripts.contains(&locking) {
                    payer.get_or_insert(locking);
                    has_other_payer |= locking != script_pubkey;
                }
            }
            None => is_every_input_known = false,
        }
    }

    let mut ours_out = Amount::ZERO;
    let mut total_out = Amount::ZERO;
    let mut to_others = Amount::ZERO;
    let mut recipient = None;
    let mut stranger = None;
    for txout in &tx.output {
        let value = value_of(txout);
        total_out = add(total_out, value, "an output total")?;
        let locking = params.locking(txout.script_pubkey.as_script()).script;
        if locking == script_pubkey {
            ours_out = add(ours_out, value, "an output total")?;
        } else {
            to_others = add(to_others, value, "an output total")?;
            if recipient.is_none() {
                recipient = params.address_of(txout.script_pubkey.as_script());
            }
            if stranger.is_none() && !own_scripts.contains(&locking) {
                stranger = params.address_of(txout.script_pubkey.as_script());
            }
        }
    }

    // A coinbase has no real inputs, and a transaction paying out more than it
    // takes in has no fee to report either.
    let fee = (is_every_input_known && !tx.is_coinbase())
        .then(|| total_in.checked_sub(total_out))
        .flatten();
    // A payment names the stranger it paid ahead of another address of the wallet's.
    let paid = stranger.or(recipient);

    let (direction, amount, counterparty) = if ours_in.is_zero() {
        (TxDirection::Incoming, ours_out, sender)
    } else if !has_other_payer {
        if to_others.is_zero() {
            // Every output comes back, so this is a consolidation and only the fee
            // moved.
            (TxDirection::SelfTransfer, ours_out, None)
        } else {
            (TxDirection::Outgoing, to_others, paid)
        }
    } else {
        let charged =
            if payer == Some(script_pubkey) { fee.unwrap_or(Amount::ZERO) } else { Amount::ZERO };
        let cost = add(ours_out, charged, "a share of a transaction")?;
        match ours_in.checked_sub(cost) {
            Some(lost) if !lost.is_zero() => (TxDirection::Outgoing, lost, paid),
            Some(_) => (TxDirection::SelfTransfer, ours_out, None),
            None => (TxDirection::Incoming, cost.saturating_sub(ours_in), sender),
        }
    };

    Ok(TxSummary { direction, amount, fee, counterparty })
}

/// The transaction `tx` only pays the fee of, when it is a child of an unconfirmed
/// parent that moves nothing but its fee.
///
/// That is a transaction of the wallet's alone, every input and its one output the
/// wallet's, whose first input spends an output of a transaction still unconfirmed or
/// confirmed in the same block as it, which only a child of an unconfirmed parent can
/// be. A child built to speed up a send spends that send's change first.
fn paid_for(
    params: &UtxoParams,
    tx: &Transaction,
    own_scripts: &[&Script],
    prevouts: &PrevoutMap,
    confirmed: Option<Confirmed>,
    heights: &HashMap<Txid, Option<u32>>,
) -> Option<Txid> {
    let is_own = |script: &Script| own_scripts.contains(&params.locking(script).script);
    let [output] = tx.output.as_slice() else { return None };
    if tx.is_coinbase()
        || !is_own(output.script_pubkey.as_script())
        || !tx.input.iter().all(|txin| {
            prevouts
                .get(txin.previous_output)
                .is_some_and(|previous| is_own(previous.script_pubkey.as_script()))
        })
    {
        return None;
    }
    let parent = tx.input.first()?.previous_output.txid;
    let parent_height = *heights.get(&parent)?;
    (parent_height.is_none() || parent_height == confirmed.map(|at| at.height)).then_some(parent)
}

/// The watched address a record is written from the point of view of, and what the
/// pass knows of the rest of the wallet.
pub(crate) struct Viewpoint<'a> {
    pub(crate) address_id: i64,
    pub(crate) script_pubkey: &'a Script,
    /// Every watched address's script, `script_pubkey`'s among them.
    pub(crate) own_scripts: &'a [&'a Script],
    /// The confirming height of every transaction in a watched history, absent while
    /// it is in the mempool.
    pub(crate) heights: &'a HashMap<Txid, Option<u32>>,
}

pub(crate) fn tx_record(
    params: &UtxoParams,
    viewpoint: &Viewpoint<'_>,
    tx: &Transaction,
    prevouts: &PrevoutMap,
    confirmed: Option<Confirmed>,
    tip_height: u32,
) -> Result<TxRecord, UtxoError> {
    let Viewpoint { address_id, script_pubkey, own_scripts, heights } = *viewpoint;
    let summary = summarise(params, tx, script_pubkey, own_scripts, prevouts)?;
    let parent = paid_for(params, tx, own_scripts, prevouts, confirmed, heights);
    // A speed-up moves nothing between parties, so none of its shares counts what it
    // pays back as an amount, and its fee is all it costs.
    let (direction, amount) = match parent {
        Some(_) => (TxDirection::SelfTransfer, Amount::ZERO),
        None => (summary.direction, summary.amount),
    };
    let mut builder = TxRecord::builder(
        AssetId::Native(params.chain),
        address_id,
        params.txid(tx.compute_txid()),
        direction,
        amount,
    );
    if let Some(parent) = parent {
        builder = builder.pays_for(params.txid(parent));
    }
    if let Some(fee) = summary.fee {
        builder = builder.fee(fee);
    }
    if let Some(counterparty) = summary.counterparty {
        builder = builder.counterparty(counterparty);
    }
    if let Some(at) = confirmed {
        builder =
            builder.confirmed_at(u64::from(at.height), at.unix_seconds, u64::from(tip_height));
    }
    // Every record comes from a decoded transaction, never a balance delta, so
    // the detection stays the default `Full`.
    Ok(builder.build())
}

fn add(total: Amount, value: Amount, context: &'static str) -> Result<Amount, UtxoError> {
    total.checked_add(value).ok_or_else(|| UtxoError::overflow(context))
}

#[cfg(test)]
mod tests {
    use bitcoin::{OutPoint, ScriptBuf};
    use jelly_core::ScriptKind;

    use super::*;
    use crate::coin::units;
    use crate::test_util::{
        BITCOIN, BITCOIN_CASH, TestKey, test_key, tx_paying, tx_spending, tx_spending_each,
    };

    /// Each key of `funded` paid by its own output of one transaction, and those
    /// outputs spent together into `outputs`.
    fn pooled(
        funded: &[(&TestKey, u64)],
        outputs: &[(ScriptBuf, u64)],
    ) -> (Transaction, PrevoutMap) {
        let paying: Vec<(ScriptBuf, u64)> =
            funded.iter().map(|(key, value)| (key.script_pubkey(), *value)).collect();
        let funding = tx_paying(&paying);
        let spent: Vec<OutPoint> = (0u32..)
            .take(funded.len())
            .map(|vout| OutPoint { txid: funding.compute_txid(), vout })
            .collect();
        (tx_spending_each(&spent, outputs), PrevoutMap::index([&funding]))
    }

    fn of(
        params: &UtxoParams,
        tx: &Transaction,
        prevouts: &PrevoutMap,
        key: &TestKey,
        own: &[&TestKey],
    ) -> TxSummary {
        let scripts: Vec<ScriptBuf> = own.iter().map(|key| key.script_pubkey()).collect();
        let scripts: Vec<&Script> = scripts.iter().map(ScriptBuf::as_script).collect();
        summarise(params, tx, key.script_pubkey().as_script(), &scripts, prevouts).unwrap()
    }

    /// Two of our addresses pay one stranger, with change to the first. Each
    /// record is what its address lost, the fee charged to the first input's, so
    /// together they are what the stranger received.
    #[test]
    fn the_records_of_a_pooled_send_add_up_to_the_payment() {
        let first = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let second = test_key(&BITCOIN, ScriptKind::P2tr, 3).unwrap();
        let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2).unwrap();
        let (tx, prevouts) = pooled(
            &[(&first, 60_000), (&second, 70_000)],
            &[(theirs.script_pubkey(), 120_000), (first.script_pubkey(), 9_000)],
        );
        let own = [&first, &second];
        let paid = of(&BITCOIN, &tx, &prevouts, &first, &own);
        let joined = of(&BITCOIN, &tx, &prevouts, &second, &own);
        assert_eq!(
            (paid.direction, paid.amount, paid.fee),
            (TxDirection::Outgoing, units(50_000), Some(units(1_000)))
        );
        assert_eq!((joined.direction, joined.amount), (TxDirection::Outgoing, units(70_000)));
        assert_eq!(paid.amount.checked_add(joined.amount), Some(units(120_000)));
        for summary in [&paid, &joined] {
            assert_eq!(summary.counterparty.as_deref(), Some(theirs.address()));
        }
    }

    /// The first address's own coins can come back to it with more than it put in,
    /// which its record shows as what it gained.
    #[test]
    fn a_pooled_address_that_gains_on_balance_records_what_it_gained() {
        let first = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let second = test_key(&BITCOIN, ScriptKind::P2pkh, 4).unwrap();
        let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2).unwrap();
        let (tx, prevouts) = pooled(
            &[(&first, 10_000), (&second, 200_000)],
            &[(theirs.script_pubkey(), 100_000), (first.script_pubkey(), 109_000)],
        );
        let own = [&first, &second];
        let gained = of(&BITCOIN, &tx, &prevouts, &first, &own);
        assert_eq!((gained.direction, gained.amount), (TxDirection::Incoming, units(100_000)));
        let lost = of(&BITCOIN, &tx, &prevouts, &second, &own);
        assert_eq!((lost.direction, lost.amount), (TxDirection::Outgoing, units(200_000)));
        assert_eq!(lost.amount.checked_sub(gained.amount), Some(units(100_000)));
    }

    /// Change to an address that put nothing in is that address's income.
    ///
    /// The one address that spent records everything it sent away, so the difference
    /// is the payment, and the payment names the stranger ahead of our own change
    /// address.
    #[test]
    fn change_to_another_of_our_addresses_is_its_income() {
        let change = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let spender = test_key(&BITCOIN, ScriptKind::P2pkh, 4).unwrap();
        let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2).unwrap();
        let (tx, prevouts) = pooled(
            &[(&spender, 130_000)],
            &[(change.script_pubkey(), 9_000), (theirs.script_pubkey(), 120_000)],
        );
        let own = [&change, &spender];
        let sent = of(&BITCOIN, &tx, &prevouts, &spender, &own);
        assert_eq!((sent.direction, sent.amount), (TxDirection::Outgoing, units(129_000)));
        assert_eq!(sent.counterparty.as_deref(), Some(theirs.address()));
        let received = of(&BITCOIN, &tx, &prevouts, &change, &own);
        assert_eq!((received.direction, received.amount), (TxDirection::Incoming, units(9_000)));
        assert_eq!(sent.amount.checked_sub(received.amount), Some(units(120_000)));
    }

    fn with_tokens(script: &ScriptBuf) -> ScriptBuf {
        let mut bytes = vec![0xef];
        bytes.extend_from_slice(&[0xbb; 32]);
        bytes.extend_from_slice(&[0x10, 0x01]);
        bytes.extend_from_slice(script.as_bytes());
        ScriptBuf::from(bytes)
    }

    /// A payment that sends tokens with some coins to the address counts those
    /// coins, and the sender is named through the plain script behind its prefix.
    #[test]
    fn the_coins_riding_with_tokens_are_counted_and_the_sender_is_named() {
        let ours = test_key(&BITCOIN_CASH, ScriptKind::P2pkh, 1).unwrap();
        let theirs = test_key(&BITCOIN_CASH, ScriptKind::P2pkh, 2).unwrap();
        let funding = tx_paying(&[(with_tokens(&theirs.script_pubkey()), 5_000)]);
        let payment = tx_spending(
            &funding,
            0,
            &[(with_tokens(&ours.script_pubkey()), 800), (theirs.script_pubkey(), 3_000)],
        );
        let prevouts = PrevoutMap::index([&funding]);
        let own = [ours.script_pubkey()];
        let own: Vec<&Script> = own.iter().map(ScriptBuf::as_script).collect();
        let summary =
            summarise(&BITCOIN_CASH, &payment, ours.script_pubkey().as_script(), &own, &prevouts)
                .unwrap();
        assert_eq!(summary.direction, TxDirection::Incoming);
        assert_eq!(summary.amount, units(800));
        assert_eq!(summary.fee, Some(units(1_200)));
        assert_eq!(summary.counterparty.as_deref(), Some(theirs.address()));

        // Spending it is an outgoing payment of the coins that left with the
        // tokens, which a token wallet on the same keys can make.
        let spend = tx_spending(&payment, 0, &[(with_tokens(&theirs.script_pubkey()), 700)]);
        let prevouts = PrevoutMap::index([&payment]);
        let summary =
            summarise(&BITCOIN_CASH, &spend, ours.script_pubkey().as_script(), &own, &prevouts)
                .unwrap();
        assert_eq!(summary.direction, TxDirection::Outgoing);
        assert_eq!(summary.amount, units(700));
        assert_eq!(summary.counterparty.as_deref(), Some(theirs.address()));

        // On a chain without tokens the same bytes pay no address of ours.
        let on_bitcoin =
            summarise(&BITCOIN, &payment, ours.script_pubkey().as_script(), &own, &prevouts)
                .unwrap();
        assert_eq!(on_bitcoin.amount, Amount::ZERO);
    }
}
