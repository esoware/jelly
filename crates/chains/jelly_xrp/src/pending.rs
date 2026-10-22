use jelly_core::{Amount, AssetId, EventSink, TxDirection, TxId, TxOutcome, TxRecord, WalletEvent};

use crate::client::{TxLookup, XrpClient};
use crate::config::CHAIN;
use crate::error::XrpError;
use crate::record::record;
use crate::state::{Stage, States};
use crate::watched::Watched;

/// Settles, resends, or lets go of the send `entry` holds, as the validated ledger at
/// `tip` shows it.
pub(crate) async fn follow(
    client: &XrpClient,
    states: &States,
    entry: &Watched,
    tip: u32,
    events: &dyn EventSink,
) -> Result<(), XrpError> {
    let Some(pending) = states.held(entry.address_id) else {
        return Ok(());
    };
    let last_ledger = pending.payment.last_ledger;
    if let Stage::Sent(signed) = &pending.stage {
        let hash = &signed.hash;
        match client.tx(hash, pending.payment.first_ledger, last_ledger).await? {
            TxLookup::Validated(result) => {
                let record = record(&result, entry.address_id, &entry.address, tip)?
                    .ok_or(XrpError::Protocol)?;
                events.emit(WalletEvent::TxDetected(record)).await;
                states.settle(entry.address_id).await?;
            }
            // A tip past LastLedgerSequence means no later ledger can include the payment, and
            // `searched_all` means the server held every ledger it could be in, since a gap
            // could hide it. Together they prove it was never applied and charged no fee.
            TxLookup::NotFound { has_searched_all: true } if tip > last_ledger => {
                let record = TxRecord::builder(
                    AssetId::Native(CHAIN),
                    entry.address_id,
                    TxId::new(CHAIN, hash),
                    TxDirection::Outgoing,
                    Amount::ZERO,
                )
                .counterparty(&pending.payment.to)
                .outcome(TxOutcome::Failed)
                .fee(Amount::ZERO)
                .build();
                events.emit(WalletEvent::TxDetected(record)).await;
                states.settle(entry.address_id).await?;
            }
            TxLookup::NotFound { .. } if tip <= last_ledger => {
                states.persist(entry.address_id, Some(&pending)).await?;
                // A resubmission is best effort, because the send stays tracked until a
                // validated ledger or the expiry proof settles it.
                let _ = client.submit(&signed.blob).await;
            }
            TxLookup::NotFound { .. } | TxLookup::Unvalidated => {}
        }
    } else if tip > last_ledger {
        states.release(entry.address_id);
    }
    Ok(())
}
