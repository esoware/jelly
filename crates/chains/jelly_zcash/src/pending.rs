use jelly_core::{Amount, EventSink, TxConfirmation, TxId, TxOutcome, WalletEvent};

use crate::config::CHAIN;
use crate::cursor::Cursor;
use crate::error::ZcashError;
use crate::record::Record;
use crate::state::States;
use crate::watched::Watched;

/// Settles the sends `entry` holds against the history `cursor` read.
///
/// A send from several sources is reported only by its first, which is the address the
/// engine recorded it under, and each source releases its own copy.
pub(crate) async fn follow(
    states: &States,
    entry: &Watched,
    cursor: &Cursor,
    events: &dyn EventSink,
) -> Result<(), ZcashError> {
    let records = &cursor.records;
    let tip_height = cursor.tip_height;
    let mut finished = Vec::new();
    for send in states.held(entry.address_id) {
        let is_reporter = send
            .payment
            .sources
            .first()
            .is_some_and(|source| source.address_id == entry.address_id);
        if let Some(signed) = send.signed() {
            let is_confirmed =
                records.get(&signed.txid).is_some_and(|record| record.height.is_some());
            let is_expired = cursor.is_complete
                && tip_height > send.payment.expiry_height
                && !records.contains_key(&signed.txid);
            if is_expired && send.is_sent() && is_reporter {
                let mut failed = Record::from_payment(&send.payment, &signed.txid).tx_record(
                    entry.address_id,
                    tip_height,
                    TxOutcome::Failed,
                );
                failed.amount = Amount::ZERO;
                failed.fee = Some(Amount::ZERO);
                events
                    .emit(WalletEvent::TxConfirmed(TxConfirmation {
                        txid: TxId::new(CHAIN, &signed.txid),
                        confirmations: 0,
                    }))
                    .await;
                events.emit(WalletEvent::TxDetected(failed)).await;
            }
            if is_confirmed || is_expired {
                finished.push(send.payment);
            } else if send.is_sent() && is_reporter && !records.contains_key(&signed.txid) {
                let unresolved = Record::from_payment(&send.payment, &signed.txid);
                events
                    .emit(WalletEvent::TxDetected(unresolved.tx_record(
                        entry.address_id,
                        tip_height,
                        TxOutcome::Unknown,
                    )))
                    .await;
            }
        } else if tip_height > send.payment.expiry_height {
            finished.push(send.payment);
        }
    }
    if !finished.is_empty() {
        states.settle(entry.address_id, &finished).await?;
    }
    Ok(())
}
