use jelly_core::{Amount, EventSink, TxDirection, TxId, TxOutcome, TxRecord, WalletEvent};

use crate::client::TonClient;
use crate::config::CHAIN;
use crate::error::TonError;
use crate::state::{Stage, States};
use crate::watched::Watched;

/// Resends the payment `entry` holds, if it was sent, until the node's time passes its
/// expiry. After that the hold is released, and a sent payment is reported as failed.
pub(crate) async fn follow(
    client: &TonClient,
    states: &States,
    entry: &Watched,
    sync_utime: u32,
    events: &dyn EventSink,
) -> Result<(), TonError> {
    let Some(pending) = states.held(entry.address_id) else {
        return Ok(());
    };
    if sync_utime > pending.payment.valid_until {
        if let Stage::Sent(signed) = &pending.stage {
            // A message that expired was never executed, so it charged no fee.
            let record = TxRecord::builder(
                pending.payment.asset(),
                entry.address_id,
                TxId::new(CHAIN, &signed.hash),
                TxDirection::Outgoing,
                Amount::ZERO,
            )
            .counterparty(&pending.payment.to)
            .outcome(TxOutcome::Failed)
            .fee(Amount::ZERO)
            .build();
            events.emit(WalletEvent::TxDetected(record)).await;
        }
        states.settle(entry.address_id).await?;
    } else if let Stage::Sent(signed) = &pending.stage {
        states.persist(entry.address_id, Some(&pending)).await?;
        // A rebroadcast is best effort, because the send stays tracked until it settles or
        // expires.
        let _ = client.broadcast(&signed.boc).await;
    }
    Ok(())
}
