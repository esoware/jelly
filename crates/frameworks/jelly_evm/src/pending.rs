use alloy::primitives::{Address, B256, Bytes, keccak256};
use jelly_core::{
    Amount, AssetId, ChainId, CursorBytes, CursorScope, CursorStore, ReservationSlot,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::error::EvmError;
use crate::fee::FeePair;
use crate::ledger::{Envelope, Ledger, Settled, State, Tracked};
use crate::rpc::{Landing, Receipt};
use crate::watched::{Watched, address_of};

const VERSION: u8 = 1;

/// Where an address's watched sends live. They are a reservation rather than scan
/// progress, because a second send must not reuse their nonces.
const SENDS_SLOT: ReservationSlot = ReservationSlot::EvmNonces;

/// The sent set of one address as it is stored.
///
/// JSON, because nothing here is secret and a readable set is worth more in a bug
/// report than the bytes it saves. Only sent transactions are written, because a
/// held slot means nothing after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSet {
    version: u8,
    settled: Vec<StoredSettlement>,
    entries: Vec<Stored>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredSettlement {
    tracked: Stored,
    receipt: Option<Receipt>,
    checkpoint: u64,
}

impl StoredSet {
    fn of(ledger: &Ledger, from: Address) -> Self {
        Self {
            version: VERSION,
            settled: ledger
                .settlements()
                .iter()
                .filter(|entry| entry.tracked.from == from)
                .filter_map(|entry| {
                    Some(StoredSettlement {
                        tracked: Stored::of(&entry.tracked, entry.tracked.sent()?),
                        receipt: entry.receipt.clone(),
                        checkpoint: entry.checkpoint,
                    })
                })
                .collect(),
            entries: ledger
                .sent_from(from)
                .map(|(entry, envelope)| Stored::of(entry, envelope))
                .collect(),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, EvmError> {
        serde_json::to_vec(self).map_err(|source| EvmError::Encode { what: "pending set", source })
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let set: Self = serde_json::from_slice(bytes).ok()?;
        (set.version == VERSION).then_some(set)
    }

    fn into_entries(self) -> Result<(Vec<Tracked>, Vec<Settled>), EvmError> {
        let entries = self.entries.into_iter().map(Tracked::try_from).collect::<Result<_, _>>()?;
        let settled = self
            .settled
            .into_iter()
            .map(|entry| {
                let txid = entry.tracked.txid;
                if entry.checkpoint == 0
                    || entry.receipt.as_ref().is_some_and(|receipt| {
                        receipt.transaction_hash != txid || receipt.landing().is_none()
                    })
                {
                    return Err(EvmError::UnreadableState);
                }
                Ok(Settled {
                    tracked: entry.tracked.try_into()?,
                    txid,
                    receipt: entry.receipt,
                    checkpoint: entry.checkpoint,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok((entries, settled))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    from: Address,
    nonce: u64,
    txid: B256,
    asset: AssetId,
    amount: Amount,
    to: Address,
    /// Wei per gas, as decimal strings, so the numbers a replacement is measured
    /// against survive a restart exactly.
    max_fee_per_gas: Amount,
    max_priority_fee_per_gas: Amount,
    /// Hex, and absent for a transfer.
    #[serde(skip_serializing_if = "Option::is_none")]
    calldata: Option<Bytes>,
    /// The signed transaction as it was broadcast, in hex.
    raw: Bytes,
    landing: Option<Landing>,
}

impl Stored {
    fn of(entry: &Tracked, envelope: &Envelope) -> Self {
        Self {
            from: entry.from,
            nonce: entry.nonce,
            txid: envelope.txid,
            asset: entry.asset,
            amount: entry.amount,
            to: entry.to,
            max_fee_per_gas: Amount::from_base_units(entry.fee.max_fee_per_gas()),
            max_priority_fee_per_gas: Amount::from_base_units(entry.fee.max_priority_fee_per_gas()),
            calldata: entry.calldata.clone(),
            raw: envelope.raw.clone(),
            landing: entry.landing,
        }
    }
}

impl TryFrom<Stored> for Tracked {
    type Error = EvmError;

    /// Refuses bytes that do not hash to the txid, which no broadcast of this
    /// adapter writes.
    fn try_from(stored: Stored) -> Result<Self, Self::Error> {
        if keccak256(&stored.raw) != stored.txid {
            return Err(EvmError::UnreadableState);
        }
        let fee = FeePair::new(
            stored.max_fee_per_gas.base_units(),
            stored.max_priority_fee_per_gas.base_units(),
        )?;
        Ok(Self {
            from: stored.from,
            nonce: stored.nonce,
            asset: stored.asset,
            amount: stored.amount,
            to: stored.to,
            fee,
            calldata: stored.calldata,
            state: State::Signed {
                envelope: Envelope { txid: stored.txid, raw: stored.raw },
                is_sent: true,
            },
            landing: stored.landing,
        })
    }
}

/// Stores `owner`'s sent set. Takes only the ports it uses, because a broadcast
/// writes it outside any pass.
pub(crate) async fn store_pending(
    cursors: &dyn CursorStore,
    ledger: &Mutex<Ledger>,
    writing: &AsyncMutex<()>,
    watched: &[Watched],
    owner: Address,
) -> Result<(), EvmError> {
    let _writing = writing.lock().await;
    let Some(address_id) = address_of(watched, owner) else {
        return Ok(());
    };
    let set = StoredSet::of(&ledger.lock(), owner);
    cursors
        .reserve(CursorScope::Address(address_id), SENDS_SLOT, CursorBytes::new(set.encode()?))
        .await
        .map_err(EvmError::Cursor)
}

/// The stored sent sets of `address_ids`, to restore into a ledger whose watch set
/// will be `watched`.
///
/// A set that does not read back, or holds a send of another network or of an
/// address its row does not watch, is [`EvmError::UnreadableState`].
pub(crate) async fn load_pending(
    cursors: &dyn CursorStore,
    chain: ChainId,
    watched: &[Watched],
    address_ids: &[i64],
) -> Result<(Vec<Tracked>, Vec<Settled>), EvmError> {
    let scopes: Vec<CursorScope> = address_ids.iter().copied().map(CursorScope::Address).collect();
    let rows = cursors.load_reservations(&scopes, &[SENDS_SLOT]).await.map_err(EvmError::Cursor)?;
    let mut entries = Vec::new();
    let mut settled = Vec::new();
    for row in rows {
        let address_id = row.scope.address_id().ok_or(EvmError::UnreadableState)?;
        let is_watched_here = |tracked: &Tracked| {
            tracked.asset.chain() == chain
                && watched.contains(&Watched { address_id, owner: tracked.from })
        };
        let (row_entries, row_settled) = row
            .payload
            .with_bytes(StoredSet::decode)
            .and_then(|set| set.into_entries().ok())
            .filter(|(row_entries, row_settled)| {
                row_entries
                    .iter()
                    .chain(row_settled.iter().map(|entry| &entry.tracked))
                    .all(is_watched_here)
            })
            .ok_or(EvmError::UnreadableState)?;
        entries.extend(row_entries);
        settled.extend(row_settled);
    }
    Ok((entries, settled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{address, envelope, held, sent_ledger, signed, txid_of};

    #[test]
    fn only_sent_transactions_are_stored_and_a_restore_does_not_duplicate_them() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        let txid = txid_of(2);
        ledger.hold(held(owner, 5, 1_000, 0));
        ledger.record_signed(signed(owner, 6, 1_000, 2));
        ledger.mark_sent(txid);
        ledger.land(txid, Some(Landing { height: 31, block_hash: None }));

        let stored = StoredSet::of(&ledger, owner);
        assert_eq!(stored.entries.len(), 1, "a held slot is not persisted");
        let bytes = stored.encode().expect("it encodes");
        let read = StoredSet::decode(&bytes).expect("it decodes");
        assert_eq!(read, stored);

        let mut fresh = Ledger::new();
        fresh.restore(read.clone().into_entries().unwrap());
        fresh.restore(read.into_entries().unwrap());
        assert_eq!(fresh.len(), 1, "restoring twice does not duplicate");
        let entry = fresh.entry(txid).expect("the entry");
        assert!(entry.is_sent());
        assert_eq!(entry.landing.map(|landing| landing.height), Some(31));
        assert_eq!(entry.fee.max_fee_per_gas(), 1_000);
        assert_eq!(entry.calldata, None);
        assert_eq!(fresh.next_nonce(owner, 0), 7);
    }

    #[test]
    fn a_sent_call_keeps_its_calldata_through_signing_and_a_restore() {
        let mut ledger = Ledger::new();
        let owner = address(1);
        let txid = txid_of(5);
        let deposit = Bytes::from_static(&[0xd0, 0xe3, 0x0d, 0xb0]);
        ledger.hold(held(owner, 4, 1_000, 0));
        let mut call = signed(owner, 4, 1_000, 5);
        call.calldata = Some(deposit.clone());
        ledger.record_signed(call);
        ledger.mark_sent(txid);
        assert_eq!(ledger.len(), 1, "the build's slot was filled in");

        let mut fresh = Ledger::new();
        fresh.restore(StoredSet::of(&ledger, owner).into_entries().unwrap());
        let entry = fresh.entry(txid).expect("the entry");
        assert_eq!(entry.calldata, Some(deposit));
    }

    #[test]
    fn a_sent_transaction_keeps_the_bytes_it_went_out_as_through_a_restore() {
        let txid = txid_of(6);
        let ledger = sent_ledger(6);
        assert_eq!(ledger.entry(txid).and_then(Tracked::sent_bytes), Some(&envelope(6).raw));

        let mut fresh = Ledger::new();
        fresh.restore(StoredSet::of(&ledger, address(1)).into_entries().unwrap());
        assert_eq!(fresh.entry(txid).and_then(Tracked::sent_bytes), Some(&envelope(6).raw));
    }

    #[test]
    fn stored_bytes_that_do_not_hash_to_their_txid_are_refused() {
        let ledger = sent_ledger(6);
        let mut stored = serde_json::to_value(StoredSet::of(&ledger, address(1))).unwrap();
        stored["entries"][0]["raw"] = serde_json::json!("0x0207");
        let set = StoredSet::decode(&serde_json::to_vec(&stored).unwrap()).expect("it decodes");
        assert!(matches!(set.into_entries(), Err(EvmError::UnreadableState)));
    }

    #[test]
    fn a_stored_set_from_another_version_or_shape_is_refused() {
        assert!(StoredSet::decode(b"not json").is_none());
        assert!(StoredSet::decode(br#"{"version":9,"entries":[],"settled":[]}"#).is_none());
        let empty =
            StoredSet::decode(br#"{"version":1,"entries":[],"settled":[]}"#).expect("it decodes");
        assert!(empty.entries.is_empty());
    }
}
