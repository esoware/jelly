//! Sends in flight. Their inputs are reserved before broadcast and stay reserved
//! through a transport failure, so a rebuild never selects them. Resending the
//! identical signed bytes is always safe.
//!
//! Only a key image on chain proves a spend. A send displaced by a conflicting spend,
//! or refused everywhere when it's resent, is reported failed at once. Otherwise it's
//! reported failed, with no fee, only once its ring node no longer holds it and its
//! block budget has run out.

use std::fmt;

use jelly_core::{
    Amount, RetryPolicy, TxDetection, TxDirection, TxId, TxOutcome, TxRecord, WalletEvent,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::broadcast::BroadcastReply;
use crate::error::MoneroError;
use crate::node::broadcast_through_ring;
use crate::params::ASSET;
use crate::sign::parse_signed;
use crate::state::ScanState;
use crate::subaddress::Subaddress;
use crate::sync::Pass;

const TARGET: &str = "jelly::monero::pending";

/// A transaction this wallet broadcast whose fate isn't known yet.
///
/// Its inputs are reserved, not spent, because only a key image on chain proves a
/// spend. Reserving them makes a rebuild safe, since a second transaction spending
/// different outputs could land alongside this one and pay twice.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub(crate) struct InFlight {
    pub txid: [u8; 32],
    pub key_images: Vec<[u8; 32]>,
    /// Exactly the bytes that were signed, so a resend is byte identical and the
    /// network deduplicates it.
    pub raw: Vec<u8>,
    pub sent_at_height: u64,
    pub amount_piconero: u64,
    pub recipient: String,
    pub subaddress: Subaddress,
    /// The node that served the ring, the only one the bytes go to while it can be
    /// reached. Its URL can carry credentials.
    pub endpoint: String,
}

impl fmt::Debug for InFlight {
    /// The transaction id and counts, but no key image, no signed byte, and no
    /// endpoint, which the sends blob keeps in the vault.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InFlight")
            .field("txid", &hex::encode(self.txid))
            .field("key_images", &self.key_images.len())
            .field("raw_bytes", &self.raw.len())
            .field("sent_at_height", &self.sent_at_height)
            .finish_non_exhaustive()
    }
}

/// What the chain has settled about a transaction in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// A key image it spends is spent on chain by this very transaction, so it
    /// landed. The scan has already reported it with its real height and fee.
    Landed { height: u64 },
    /// A key image it spends is spent on chain by a different transaction. Only
    /// one transaction spending a key image can ever land, and this was not it.
    Displaced,
}

impl InFlight {
    /// Reads the chain's verdict out of what the scan recorded.
    ///
    /// No node is asked, because only a key image on chain proves a spend, and the
    /// scan sees those.
    fn settled(&self, state: &ScanState) -> Option<Settled> {
        let mut landed = None;
        for image in &self.key_images {
            let Some(spend) = state.spend_of(image) else { continue };
            if spend.txid == self.txid {
                landed = Some(landed.map_or(spend.height, |height: u64| height.min(spend.height)));
            } else {
                return Some(Settled::Displaced);
            }
        }
        landed.map(|height| Settled::Landed { height })
    }

    /// True once the send's block budget has run out. The caller also checks that the
    /// node it was sent to no longer holds it.
    ///
    /// Monero transactions don't expire, since one stays valid as long as its key
    /// images are unspent. Gone is an observation, not a protocol fact, and the budget
    /// turns a run of observations into a decision.
    const fn is_out_of_budget(&self, tip_height: u64, budget_blocks: u64) -> bool {
        tip_height.saturating_sub(self.sent_at_height) >= budget_blocks
    }
}

/// A send whose fate is known, kept only to replay its record until a committed scan
/// covers it. It reserves no input and holds no signed byte.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub(crate) struct TerminalSend {
    pub txid: [u8; 32],
    pub subaddress: Subaddress,
    pub amount_piconero: u64,
    pub recipient: String,
    pub checkpoint: u64,
    pub is_failed: bool,
}

impl fmt::Debug for TerminalSend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalSend")
            .field("txid", &hex::encode(self.txid))
            .field("checkpoint", &self.checkpoint)
            .field("is_failed", &self.is_failed)
            .finish_non_exhaustive()
    }
}

impl TerminalSend {
    fn failed_record(&self, address_id: i64, seen_at: Option<i64>) -> TxRecord {
        let mut builder = TxRecord::builder(
            ASSET,
            address_id,
            TxId::new(ASSET.chain(), hex::encode(self.txid)),
            TxDirection::Outgoing,
            Amount::from(self.amount_piconero),
        )
        .fee(Amount::ZERO)
        .counterparty(self.recipient.clone())
        .outcome(TxOutcome::Failed)
        .detection(TxDetection::Full);
        if let Some(seen_at) = seen_at {
            builder = builder.seen_at(seen_at);
        }
        builder.build()
    }
}

/// Decides each in-flight transaction's fate, resending the ones its ring node lost.
///
/// The node that served a transaction's ring is the only one asked about it and the
/// only one its bytes go to, so its origin isn't spread over the pool. Another node is
/// asked only while that one can't be reached at all, and a node that can't answer is
/// asked again next pass. A transaction is resent only once it's in neither the
/// node's pool nor its chain.
///
/// The terminal sends are written before `settled_through` moves past them, whether or
/// not the sends blob changed for any other reason.
pub(crate) async fn follow_up(
    pass: &Pass<'_>,
    tip: u64,
    has_acknowledged: bool,
) -> Result<(), MoneroError> {
    let waiting_sends: Vec<InFlight> = pass.state.lock().in_flight_sends().cloned().collect();
    for send in waiting_sends {
        // Bound separately, because a guard in the match scrutinee would still be
        // held when an arm locks the state again.
        let verdict = send.settled(&pass.state.lock());
        match verdict {
            Some(Settled::Landed { height }) => {
                pass.state.lock().retire_landed(&send.txid)?;
                tracing::info!(target: TARGET, height, "a monero send landed");
                continue;
            }
            Some(Settled::Displaced) => {
                pass.state.lock().retire_failed(&send.txid)?;
                continue;
            }
            None => {}
        }

        let ring = Some(send.endpoint.as_str());
        let asked = match pass.node.is_known(ring, send.txid).await {
            Err(error) if error.is_unreachable() => {
                pass.node.is_known(None, send.txid).await.map(|is_known| (is_known, None))
            }
            answered => answered.map(|is_known| (is_known, ring)),
        };
        let (is_known, via) = match asked {
            Ok(asked) => asked,
            Err(error) => {
                tracing::debug!(
                    target: TARGET,
                    %error,
                    "no node could say whether a monero send is still in flight"
                );
                continue;
            }
        };
        if is_known {
            continue;
        }
        if send.is_out_of_budget(tip, pass.config.abandon_after_blocks) {
            pass.state.lock().retire_failed(&send.txid)?;
        } else {
            resend(pass, &send, via).await?;
        }
    }
    let terminal = {
        let mut state = pass.state.lock();
        state.mark_landed_terminal_sends();
        state.terminal_sends().to_vec()
    };
    let has_pruned = pass.state.lock().prune_rings();
    if has_acknowledged || has_pruned || !terminal.is_empty() {
        pass.blobs.write_sends(pass.state).await?;
    }
    for send in &terminal {
        if send.is_failed {
            report_failed(pass, send).await;
        }
    }
    let mut state = pass.state.lock();
    for send in &terminal {
        state.settle_through(send.checkpoint);
    }
    Ok(())
}

/// Resends the identical bytes, which is always safe because the txid is derived
/// from them and the network deduplicates.
///
/// They go through the ring node, or through the pool for `None` when the ring node
/// couldn't be reached.
async fn resend(pass: &Pass<'_>, send: &InFlight, via: Option<&str>) -> Result<(), MoneroError> {
    let Ok(transaction) = parse_signed(&send.raw) else {
        tracing::warn!(
            target: TARGET,
            "a stored monero transaction does not decode, so it cannot be resent"
        );
        return Ok(());
    };
    let reply = match via {
        Some(endpoint) => broadcast_through_ring(pass.node.as_ref(), endpoint, &transaction).await,
        None => pass.node.broadcast(None, &transaction).await,
    };
    match reply {
        Ok(BroadcastReply::Accepted) => Ok(()),
        Ok(BroadcastReply::Refused(rejection)) => {
            if rejection.retry == RetryPolicy::FinalEverywhere {
                return pass.state.lock().retire_failed(&send.txid);
            }
            tracing::debug!(
                target: TARGET,
                detail = %rejection.detail,
                "a monero resend was refused"
            );
            Ok(())
        }
        Err(error) => {
            tracing::debug!(
                target: TARGET,
                %error,
                "a monero resend did not reach a node"
            );
            Ok(())
        }
    }
}

async fn report_failed(pass: &Pass<'_>, send: &TerminalSend) {
    let Some(address_id) = pass.watched.attribute(send.subaddress) else { return };
    let seen_at = match pass.clock.now_secs() {
        Ok(secs) => Some(secs),
        Err(error) => {
            tracing::warn!(
                target: TARGET,
                %error,
                "the system clock is unreadable, so this record is left for the store to time"
            );
            None
        }
    };
    pass.events.emit(WalletEvent::TxDetected(send.failed_record(address_id, seen_at))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::PUBLIC_SPEND;

    fn send(images: Vec<[u8; 32]>) -> InFlight {
        InFlight {
            txid: [0xaa; 32],
            key_images: images,
            raw: Vec::new(),
            sent_at_height: 1_000,
            amount_piconero: 500,
            recipient: "4AAA".to_owned(),
            subaddress: Subaddress::PRIMARY,
            endpoint: "https://user:secret@node.invalid:18081".to_owned(),
        }
    }

    fn terminal() -> TerminalSend {
        TerminalSend {
            txid: [0xaa; 32],
            subaddress: Subaddress::PRIMARY,
            amount_piconero: 500,
            recipient: "4AAA".to_owned(),
            checkpoint: 1,
            is_failed: true,
        }
    }

    #[test]
    fn a_terminal_send_debug_line_carries_no_recipient_and_zeroizing_clears_it() {
        let mut terminal = terminal();
        assert!(!format!("{terminal:?}").contains("4AAA"));
        terminal.zeroize();
        assert!(terminal.recipient.is_empty());
        assert_eq!(terminal.txid, [0; 32]);
        assert_eq!(terminal.amount_piconero, 0);
        assert_eq!(terminal.checkpoint, 0);
    }

    #[test]
    fn a_debug_line_names_the_txid_and_carries_no_key_image_or_signed_byte() {
        let key_image = [0x5eu8; 32];
        let mut send = send(vec![key_image]);
        send.raw = vec![0xab; 64];
        let rendered = format!("{send:?}");
        assert!(rendered.contains(&hex::encode([0xaa; 32])), "{rendered}");
        assert!(!rendered.contains(&hex::encode(key_image)), "{rendered}");
        assert!(!rendered.contains("94, 94"), "{rendered}");
        assert!(!rendered.contains("171, 171"), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
    }

    #[test]
    fn a_budget_is_measured_from_the_height_it_was_sent_at() {
        let send = send(vec![[1; 32]]);
        assert!(!send.is_out_of_budget(1_000, 720));
        assert!(!send.is_out_of_budget(1_719, 720));
        assert!(send.is_out_of_budget(1_720, 720));
    }

    #[test]
    fn a_send_with_nothing_observed_is_not_settled() {
        let state = ScanState::new(PUBLIC_SPEND, 0, (1, 1));
        assert_eq!(send(vec![[1; 32]]).settled(&state), None);
    }

    #[test]
    fn a_failed_record_charges_no_fee_and_names_the_recipient() {
        let record = terminal().failed_record(42, Some(1_700_000_500));
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.fee, Some(Amount::ZERO));
        assert_eq!(record.address_id, 42);
        assert_eq!(record.counterparty.as_deref(), Some("4AAA"));
        assert_eq!(record.txid.value(), hex::encode([0xaa; 32]));
        assert_eq!(record.timestamp, Some(1_700_000_500));
    }

    /// A clock that can't be read gives no date, so the record carries none and
    /// the store times the row it writes.
    #[test]
    fn a_failed_record_with_no_clock_reading_carries_no_timestamp() {
        let record = terminal().failed_record(42, None);
        assert_eq!(record.timestamp, None);
        assert_eq!(record.fee, Some(Amount::ZERO));
        assert_eq!(record.outcome, TxOutcome::Failed);
    }
}
