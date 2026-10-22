use std::collections::BTreeMap;

use jelly_core::{Amount, AssetId, TxDirection, TxId, TxOutcome, TxRecord};
use serde::{Deserialize, Serialize};
use zcash_protocol::value::BalanceError;
use zcash_transparent::bundle::{OutPoint, TxOut};

use crate::address::{CODEC, is_pay_to_pubkey, script};
use crate::client::TxInfo;
use crate::config::CHAIN;
use crate::error::ZcashError;
use crate::payload::Payment;
use crate::zat::add;

/// Stored in the `zcash.transactions` scan cursor, so its field names are a stored format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    txid: String,
    direction: TxDirection,
    amount_zat: u64,
    fee_zat: Option<u64>,
    counterparty: Option<String>,
    pub(crate) height: Option<u32>,
    timestamp: Option<i64>,
}

impl Record {
    /// What the first source shows for `payment` until its history has it.
    pub(crate) fn from_payment(payment: &Payment, txid: &str) -> Self {
        let is_self = payment.sources.iter().any(|source| source.address == payment.to);
        Self {
            txid: txid.to_owned(),
            direction: if is_self { TxDirection::SelfTransfer } else { TxDirection::Outgoing },
            amount_zat: if is_self {
                payment.amount_zat.saturating_add(payment.change_zat)
            } else {
                payment.amount_zat
            },
            fee_zat: Some(payment.fee_zat),
            counterparty: (!is_self).then(|| payment.to.clone()),
            height: None,
            timestamp: None,
        }
    }

    pub(crate) fn tx_record(
        &self,
        address_id: i64,
        tip_height: u32,
        outcome: TxOutcome,
    ) -> TxRecord {
        let mut builder = TxRecord::builder(
            AssetId::Native(CHAIN),
            address_id,
            TxId::new(CHAIN, &self.txid),
            self.direction,
            Amount::from(self.amount_zat),
        )
        .outcome(outcome);
        if let Some(fee_zat) = self.fee_zat {
            builder = builder.fee(Amount::from(fee_zat));
        }
        if let Some(counterparty) = &self.counterparty {
            builder = builder.counterparty(counterparty);
        }
        if let Some(height) = self.height {
            builder =
                builder.confirmed_at(u64::from(height), self.timestamp, u64::from(tip_height));
        } else if let Some(timestamp) = self.timestamp {
            builder = builder.seen_at(timestamp);
        }
        builder.build()
    }
}

/// The record of `info` for `address`, one of the wallet's `own` addresses.
///
/// When several of the wallet's addresses pay into the transaction, each one's record is
/// what it lost or gained, and the fee is charged to the address of its first input of the
/// wallet's, so the records of one transaction add up to what left the wallet.
pub(crate) fn summarize(
    info: &TxInfo,
    address: &str,
    own: &[&str],
    previous_outputs: &BTreeMap<OutPoint, TxOut>,
) -> Result<Record, ZcashError> {
    let owner_of = |output: &TxOut| -> Result<Option<&str>, ZcashError> {
        for &candidate in own {
            if output.script_pubkey() == &script(candidate)? || is_pay_to_pubkey(output, candidate)
            {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    };
    let bundle = info.tx.transparent_bundle().ok_or(ZcashError::Protocol)?;
    let mut ours_in_zat = 0;
    let mut ours_out_zat = 0;
    let mut others_out_zat = 0;
    let mut sender = None;
    let mut recipient = None;
    let mut stranger = None;
    let mut payer = None;
    let mut has_other_payer = false;
    if !bundle.is_coinbase() {
        for input in &bundle.vin {
            let output = previous_outputs.get(input.prevout()).ok_or(ZcashError::Protocol)?;
            let owner = owner_of(output)?;
            if owner == Some(address) {
                ours_in_zat = add(ours_in_zat, u64::from(output.value()))?;
            } else if sender.is_none() {
                sender = CODEC.address_of(&output.script_pubkey().0.0);
            }
            if let Some(owner) = owner {
                payer.get_or_insert(owner);
                has_other_payer |= owner != address;
            }
        }
    }
    for output in &bundle.vout {
        let owner = owner_of(output)?;
        if owner == Some(address) {
            ours_out_zat = add(ours_out_zat, u64::from(output.value()))?;
        } else {
            others_out_zat = add(others_out_zat, u64::from(output.value()))?;
            let named = CODEC.address_of(&output.script_pubkey().0.0);
            if recipient.is_none() {
                recipient.clone_from(&named);
            }
            if stranger.is_none() && owner.is_none() {
                stranger = named;
            }
        }
    }
    if ours_in_zat == 0 && ours_out_zat == 0 {
        return Err(ZcashError::Protocol);
    }
    let fee_zat = if bundle.is_coinbase() {
        None
    } else {
        info.tx
            .fee_paid::<BalanceError, _>(|point| Ok(previous_outputs.get(point).map(TxOut::value)))
            .map_err(|_| ZcashError::Protocol)?
            .map(u64::from)
    };
    let is_shielded = info.tx.sprout_bundle().is_some()
        || info.tx.sapling_bundle().is_some()
        || info.tx.orchard_bundle().is_some()
        || info.tx.ironwood_bundle().is_some();
    let paid = stranger.or(recipient);
    let (direction, amount_zat, counterparty) = if has_other_payer {
        let charged_zat = if payer == Some(address) { fee_zat.unwrap_or_default() } else { 0 };
        let cost_zat = add(ours_out_zat, charged_zat)?;
        match ours_in_zat.checked_sub(cost_zat) {
            Some(0) => (TxDirection::SelfTransfer, ours_out_zat, None),
            Some(lost_zat) => (TxDirection::Outgoing, lost_zat, paid),
            None => (TxDirection::Incoming, cost_zat.saturating_sub(ours_in_zat), sender),
        }
    } else {
        let spent_zat =
            ours_in_zat.saturating_sub(ours_out_zat).saturating_sub(fee_zat.unwrap_or_default());
        // A transaction that pays no one else transparently is a self transfer, unless it
        // has shielded parts and cost more than its fee, because the rest then went to a
        // shielded recipient this adapter cannot see.
        if ours_out_zat > ours_in_zat {
            (TxDirection::Incoming, ours_out_zat.saturating_sub(ours_in_zat), sender)
        } else if others_out_zat == 0 && (!is_shielded || spent_zat == 0) {
            (TxDirection::SelfTransfer, ours_out_zat, None)
        } else {
            (TxDirection::Outgoing, spent_zat, paid)
        }
    };
    Ok(Record {
        txid: info.tx.txid().to_string(),
        direction,
        amount_zat,
        fee_zat,
        counterparty,
        height: info.height,
        timestamp: info.timestamp,
    })
}

#[cfg(test)]
mod tests {
    use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
    use zcash_protocol::{
        consensus::{BlockHeight, BranchId, MAIN_NETWORK},
        value::Zatoshis,
    };
    use zcash_script::script::Code;
    use zcash_transparent::{
        address::Script,
        bundle::{self, Bundle, TxIn},
    };

    use super::*;
    use crate::fixtures::key_address;

    const HEIGHT: u32 = 3_514_158;

    fn output(address: &str, value_zat: u64) -> TxOut {
        TxOut::new(Zatoshis::from_u64(value_zat).unwrap(), script(address).unwrap())
    }

    /// A mined transaction spending one coin of each address in `funded` into `outputs`.
    fn pooled(funded: &[(&str, u64)], outputs: Vec<TxOut>) -> (TxInfo, BTreeMap<OutPoint, TxOut>) {
        let mut previous = BTreeMap::new();
        let mut vin = Vec::new();
        for (seed, (owner, value_zat)) in (1u8..).zip(funded) {
            let point = OutPoint::new([seed; 32], 0);
            previous.insert(point.clone(), output(owner, *value_zat));
            vin.push(TxIn::from_parts(point, Script(Code(vec![0, 0])), u32::MAX));
        }
        let tx = TransactionData::<Authorized>::from_parts(
            TxVersion::V5,
            BranchId::for_height(&MAIN_NETWORK, BlockHeight::from_u32(HEIGHT)),
            0,
            BlockHeight::from_u32(HEIGHT.saturating_add(40)),
            Some(Bundle { vin, vout: outputs, authorization: bundle::Authorized }),
            None,
            None,
            None,
        )
        .freeze()
        .unwrap();
        (TxInfo { tx, height: Some(HEIGHT), timestamp: None }, previous)
    }

    /// Two addresses of the wallet pay a stranger, with change to the first. Each record
    /// is what its address lost, the fee charged to the first input's, so together they
    /// are the payment.
    #[test]
    fn the_records_of_a_pooled_payment_add_up_to_the_payment() {
        let (first, second, theirs) = (key_address(7), key_address(8), key_address(9));
        let own = [first.as_str(), second.as_str()];
        let (info, previous) = pooled(
            &[(&first, 70_000), (&second, 70_000)],
            vec![output(&theirs, 120_000), output(&first, 10_000)],
        );
        let paid = summarize(&info, &first, &own, &previous).unwrap();
        let joined = summarize(&info, &second, &own, &previous).unwrap();
        assert_eq!((paid.direction, paid.amount_zat), (TxDirection::Outgoing, 50_000));
        assert_eq!(paid.fee_zat, Some(10_000));
        assert_eq!((joined.direction, joined.amount_zat), (TxDirection::Outgoing, 70_000));
        assert_eq!(paid.amount_zat.saturating_add(joined.amount_zat), 120_000);
        for record in [&paid, &joined] {
            assert_eq!(record.counterparty.as_deref(), Some(theirs.as_str()));
        }
    }

    /// Change to an address that put nothing in is that address's income.
    ///
    /// The one address that spent less the change is the payment, and the payment names
    /// the stranger ahead of the wallet's own change address.
    #[test]
    fn change_to_another_of_the_wallets_addresses_is_its_income() {
        let (change, spender, theirs) = (key_address(7), key_address(8), key_address(9));
        let own = [change.as_str(), spender.as_str()];
        let (info, previous) =
            pooled(&[(&spender, 140_000)], vec![output(&change, 10_000), output(&theirs, 120_000)]);
        let sent = summarize(&info, &spender, &own, &previous).unwrap();
        assert_eq!((sent.direction, sent.amount_zat), (TxDirection::Outgoing, 130_000));
        assert_eq!(sent.counterparty.as_deref(), Some(theirs.as_str()));
        let received = summarize(&info, &change, &own, &previous).unwrap();
        assert_eq!((received.direction, received.amount_zat), (TxDirection::Incoming, 10_000));
        assert_eq!(sent.amount_zat.saturating_sub(received.amount_zat), 120_000);
    }
}
