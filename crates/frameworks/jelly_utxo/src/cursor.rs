use std::collections::BTreeMap;

use bitcoin::hashes::Hash as _;
use bitcoin::{OutPoint, Txid};
use jelly_core::{Amount, TxDirection};
use jelly_electrum::{HistoryDigest, HistoryEntry};

use crate::coin::{Origin, Utxo, to_u64, units};
use crate::error::UtxoError;
use crate::script::SpendableScript;

/// The first field of every cursor. Bytes with any other version don't decode, so
/// they read as unreadable state, not as a missing cursor.
const CURSOR_VERSION: u32 = 6;

/// The flags, txid, vout, value, and height.
const COIN_BYTES: usize = 1 + 32 + 4 + 8 + 4;
/// The txid and height.
const SEEN_BYTES: usize = 32 + 4;
/// The txid, direction, and spend count, without the spends.
const PENDING_BYTES: usize = 32 + 1 + 4;
/// The txid and vout.
const OUTPOINT_BYTES: usize = 32 + 4;
/// The version, digest flag, and the three counts, without the optional digest.
const HEADER_BYTES: usize = 4 + 1 + 4 + 4 + 4;

const CONFIRMED: u8 = 0b001;
const COINBASE: u8 = 0b010;
const PEGOUT: u8 = 0b100;
const OWN: u8 = 0b1000;
const TOKENS: u8 = 0b1_0000;

const INCOMING: u8 = 0;
const OUTGOING: u8 = 1;
const SELF_TRANSFER: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorUtxo {
    pub(crate) outpoint: OutPoint,
    pub(crate) value: Amount,
    /// Absent while the funding transaction is in the mempool.
    pub(crate) height: Option<u32>,
    pub(crate) origin: Origin,
    pub(crate) has_tokens: bool,
}

/// A mempool transaction the writing pass decoded, with enough for a later pass to
/// settle it if a conflicting transaction replaces it, even in a session that never
/// decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorPending {
    pub(crate) txid: Txid,
    pub(crate) direction: TxDirection,
    pub(crate) spends: Vec<OutPoint>,
}

/// What one address's sync pass leaves for the next one. Only a successful pass
/// writes one, and the balance isn't stored because it's the sum of the coins.
///
/// On disk, little-endian throughout:
///
/// ```text
/// version   u32
/// flags     u8            bit 0: a digest follows
/// digest    [u8; 32]      present only when the flag is set
/// count     u32
/// coins     count * { flags u8, txid [u8; 32], vout u32, value u64, height u32 }
/// count     u32
/// history   count * { txid [u8; 32], height u32 }
/// count     u32
/// pending   count * { txid [u8; 32], direction u8, count u32,
///                     spends count * { txid [u8; 32], vout u32 } }
/// ```
///
/// A coin's flags are bit 0 for confirmed, bit 1 for a coinbase output, bit 2 for
/// a peg-out, bit 3 for an output of the wallet's own transaction, and bit 4 for
/// an output that carries tokens. A history entry's height is zero while it's in
/// the mempool, since no wallet's history holds the genesis block. A pending
/// transaction's direction is 0 for incoming, 1 for outgoing, and 2 for a
/// self-transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncCursor {
    /// The history fingerprint the writing pass saw. Not the protocol's `status`
    /// hash, since servers legitimately order entries within a block differently and
    /// a failover would invalidate every cursor.
    digest: Option<HistoryDigest>,
    coins: Vec<CursorUtxo>,
    /// Every transaction the writing pass reported, at the height it reported it,
    /// absent while in the mempool. A transaction never changes, so the next pass
    /// downloads only the ones that are new or have moved.
    seen: BTreeMap<Txid, Option<u32>>,
    pending: Vec<CursorPending>,
}

impl SyncCursor {
    pub(crate) fn new(
        digest: Option<HistoryDigest>,
        coins: &[Utxo],
        history: &[HistoryEntry],
        pending: Vec<CursorPending>,
    ) -> Self {
        Self {
            digest,
            coins: coins
                .iter()
                .map(|coin| CursorUtxo {
                    outpoint: coin.outpoint(),
                    value: coin.value(),
                    height: coin.height(),
                    origin: coin.origin(),
                    has_tokens: coin.has_tokens(),
                })
                .collect(),
            seen: history.iter().map(|item| (item.txid, item.confirmation.height())).collect(),
            pending,
        }
    }

    /// Whether the writing pass reported `txid` confirmed at `height`.
    pub(crate) fn is_seen_at(&self, txid: Txid, height: u32) -> bool {
        self.seen.get(&txid) == Some(&Some(height))
    }

    #[cfg(test)]
    pub(crate) const fn digest(&self) -> Option<HistoryDigest> {
        self.digest
    }

    pub(crate) fn coins(&self) -> &[CursorUtxo] {
        &self.coins
    }

    pub(crate) fn pending(&self) -> &[CursorPending] {
        &self.pending
    }

    /// A match means the address's history hasn't moved since the writing pass.
    pub(crate) fn matches(&self, digest: Option<HistoryDigest>) -> bool {
        self.digest == digest
    }

    /// A sum of `u64` values over any count a `Vec` can hold fits in a `u128`, so
    /// the fallback is unreachable.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) fn balance(&self) -> Amount {
        Amount::checked_sum(self.coins.iter().map(|coin| coin.value)).unwrap_or(Amount::MAX)
    }

    pub(crate) fn restore(&self, script: &SpendableScript) -> Vec<Utxo> {
        self.coins
            .iter()
            .map(|coin| {
                Utxo::new(coin.outpoint, coin.value, script.clone(), coin.height, coin.origin)
                    .holding_tokens(coin.has_tokens)
            })
            .collect()
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, UtxoError> {
        let count = u32::try_from(self.coins.len())
            .map_err(|_| UtxoError::overflow("the cursor's coin count"))?;
        let seen_count = u32::try_from(self.seen.len())
            .map_err(|_| UtxoError::overflow("the cursor's history count"))?;
        let pending_count = u32::try_from(self.pending.len())
            .map_err(|_| UtxoError::overflow("the cursor's pending count"))?;
        let digest_bytes = if self.digest.is_some() { HistoryDigest::LEN } else { 0 };
        let pending_bytes = self.pending.iter().fold(0usize, |total, tx| {
            total
                .saturating_add(PENDING_BYTES)
                .saturating_add(tx.spends.len().saturating_mul(OUTPOINT_BYTES))
        });
        let mut encoded = Vec::with_capacity(
            HEADER_BYTES
                .saturating_add(digest_bytes)
                .saturating_add(self.coins.len().saturating_mul(COIN_BYTES))
                .saturating_add(self.seen.len().saturating_mul(SEEN_BYTES))
                .saturating_add(pending_bytes),
        );
        encoded.extend_from_slice(&CURSOR_VERSION.to_le_bytes());
        encoded.push(u8::from(self.digest.is_some()));
        if let Some(digest) = self.digest {
            encoded.extend_from_slice(digest.as_bytes());
        }
        encoded.extend_from_slice(&count.to_le_bytes());
        for coin in &self.coins {
            let origin = match coin.origin {
                Origin::Ordinary => 0,
                Origin::Coinbase => COINBASE,
                Origin::Pegout => PEGOUT,
                Origin::Own => OWN,
            };
            let confirmed_bit = if coin.height.is_some() { CONFIRMED } else { 0 };
            let tokens_bit = if coin.has_tokens { TOKENS } else { 0 };
            encoded.push(origin | confirmed_bit | tokens_bit);
            encoded.extend_from_slice(&coin.outpoint.txid.to_byte_array());
            encoded.extend_from_slice(&coin.outpoint.vout.to_le_bytes());
            encoded.extend_from_slice(&to_u64(coin.value)?.to_le_bytes());
            encoded.extend_from_slice(&coin.height.unwrap_or(0).to_le_bytes());
        }
        encoded.extend_from_slice(&seen_count.to_le_bytes());
        for (txid, height) in &self.seen {
            encoded.extend_from_slice(&txid.to_byte_array());
            encoded.extend_from_slice(&height.unwrap_or(0).to_le_bytes());
        }
        encoded.extend_from_slice(&pending_count.to_le_bytes());
        for tx in &self.pending {
            let spend_count = u32::try_from(tx.spends.len())
                .map_err(|_| UtxoError::overflow("a pending transaction's input count"))?;
            encoded.extend_from_slice(&tx.txid.to_byte_array());
            encoded.push(match tx.direction {
                TxDirection::Incoming => INCOMING,
                TxDirection::Outgoing => OUTGOING,
                TxDirection::SelfTransfer => SELF_TRANSFER,
            });
            encoded.extend_from_slice(&spend_count.to_le_bytes());
            for outpoint in &tx.spends {
                encoded.extend_from_slice(&outpoint.txid.to_byte_array());
                encoded.extend_from_slice(&outpoint.vout.to_le_bytes());
            }
        }
        Ok(encoded)
    }

    /// `None` means unreadable state, which callers must not treat as an absent
    /// cursor.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = Reader { bytes };
        if u32::from_le_bytes(reader.take::<4>()?) != CURSOR_VERSION {
            return None;
        }
        let digest = match reader.take::<1>()? {
            [0] => None,
            [1] => Some(HistoryDigest::from_bytes(reader.take::<{ HistoryDigest::LEN }>()?)),
            _ => return None,
        };
        let count = usize::try_from(u32::from_le_bytes(reader.take::<4>()?)).ok()?;
        // Checked before anything is allocated, so a corrupt count can't ask for
        // more memory than the bytes could describe.
        if reader.remaining() < count.checked_mul(COIN_BYTES)? {
            return None;
        }
        let mut coins = Vec::with_capacity(count);
        for _ in 0..count {
            let [flags] = reader.take::<1>()?;
            let origin = match flags & !(CONFIRMED | TOKENS) {
                0 => Origin::Ordinary,
                COINBASE => Origin::Coinbase,
                PEGOUT => Origin::Pegout,
                OWN => Origin::Own,
                _ => return None,
            };
            let txid = Txid::from_byte_array(reader.take::<32>()?);
            let vout = u32::from_le_bytes(reader.take::<4>()?);
            let value = units(u64::from_le_bytes(reader.take::<8>()?));
            let height = u32::from_le_bytes(reader.take::<4>()?);
            coins.push(CursorUtxo {
                outpoint: OutPoint { txid, vout },
                value,
                height: (flags & CONFIRMED != 0).then_some(height),
                origin,
                has_tokens: flags & TOKENS != 0,
            });
        }
        let seen_count = usize::try_from(u32::from_le_bytes(reader.take::<4>()?)).ok()?;
        if reader.remaining() < seen_count.checked_mul(SEEN_BYTES)? {
            return None;
        }
        let mut seen = BTreeMap::new();
        for _ in 0..seen_count {
            let txid = Txid::from_byte_array(reader.take::<32>()?);
            let height = u32::from_le_bytes(reader.take::<4>()?);
            if seen.insert(txid, (height != 0).then_some(height)).is_some() {
                return None;
            }
        }
        let pending_count = usize::try_from(u32::from_le_bytes(reader.take::<4>()?)).ok()?;
        if reader.remaining() < pending_count.checked_mul(PENDING_BYTES)? {
            return None;
        }
        let mut pending = Vec::with_capacity(pending_count);
        for _ in 0..pending_count {
            let txid = Txid::from_byte_array(reader.take::<32>()?);
            let direction = match reader.take::<1>()? {
                [INCOMING] => TxDirection::Incoming,
                [OUTGOING] => TxDirection::Outgoing,
                [SELF_TRANSFER] => TxDirection::SelfTransfer,
                _ => return None,
            };
            let spend_count = usize::try_from(u32::from_le_bytes(reader.take::<4>()?)).ok()?;
            if reader.remaining() < spend_count.checked_mul(OUTPOINT_BYTES)? {
                return None;
            }
            let mut spends = Vec::with_capacity(spend_count);
            for _ in 0..spend_count {
                let txid = Txid::from_byte_array(reader.take::<32>()?);
                let vout = u32::from_le_bytes(reader.take::<4>()?);
                spends.push(OutPoint { txid, vout });
            }
            pending.push(CursorPending { txid, direction, spends });
        }
        if reader.remaining() != 0 {
            return None;
        }
        Some(Self { digest, coins, seen, pending })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.bytes.split_first_chunk::<N>()?;
        self.bytes = rest;
        Some(*head)
    }

    const fn remaining(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::ScriptKind;
    use jelly_electrum::Confirmation;

    use super::*;
    use crate::fixtures::{TIP, confirmed, incoming, unconfirmed};
    use crate::test_util::{BITCOIN, LITECOIN, fixture_txid, test_key};

    fn entry(seed: usize, confirmation: Confirmation) -> HistoryEntry {
        HistoryEntry { txid: fixture_txid(seed), confirmation, fee: None }
    }

    #[test]
    fn a_cursor_round_trips_and_an_unreadable_one_is_refused() {
        let ours = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let coins = vec![
            confirmed(&ours, 3, 123_456),
            unconfirmed(&ours, 4, 7_890),
            incoming(&ours, 5, 1_000),
        ];
        let history = [
            entry(3, Confirmation::Confirmed { height: TIP - 5 }),
            entry(4, Confirmation::Mempool { has_unconfirmed_parents: true }),
        ];
        let digest = HistoryDigest::from_bytes([9u8; 32]);
        let pending = vec![CursorPending {
            txid: fixture_txid(4),
            direction: TxDirection::SelfTransfer,
            spends: vec![
                OutPoint { txid: fixture_txid(3), vout: 0 },
                OutPoint { txid: fixture_txid(6), vout: 2 },
            ],
        }];
        let cursor = SyncCursor::new(Some(digest), &coins, &history, pending.clone());

        let bytes = cursor.encode().unwrap();
        let back = SyncCursor::decode(&bytes).expect("it decodes");
        assert_eq!(back, cursor);
        assert_eq!(back.digest(), Some(digest));
        assert_eq!(back.balance(), units(123_456 + 7_890 + 1_000));
        assert_eq!(back.restore(ours.script()), coins);
        assert_eq!(back.pending(), pending.as_slice());
        assert!(back.matches(Some(digest)));
        assert!(!back.matches(None));
        assert!(back.is_seen_at(fixture_txid(3), TIP - 5));
        assert!(!back.is_seen_at(fixture_txid(3), TIP - 4), "seen, but at another height");
        assert!(!back.is_seen_at(fixture_txid(4), TIP - 5), "seen only in the mempool");
        assert!(!back.is_seen_at(fixture_txid(5), TIP - 5), "never seen");

        // The header, the digest, one 49-byte record per coin, one 36-byte record
        // per history entry, and a 37-byte record per pending transaction with 36
        // bytes for each outpoint it spends.
        assert_eq!(bytes.len(), 4 + 1 + 32 + 4 + 3 * 49 + 4 + 2 * 36 + 4 + 37 + 2 * 36);

        assert!(SyncCursor::decode(b"").is_none());
        assert!(SyncCursor::decode(b"not a cursor").is_none());
        let truncated = bytes.get(..bytes.len() - 1).expect("a prefix").to_vec();
        assert!(SyncCursor::decode(&truncated).is_none());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(SyncCursor::decode(&trailing).is_none());
        let mut wrong_version = bytes;
        if let Some(slot) = wrong_version.first_mut() {
            *slot = CURSOR_VERSION.to_le_bytes().first().copied().unwrap_or(0).wrapping_add(1);
        }
        assert!(SyncCursor::decode(&wrong_version).is_none());
    }

    /// A coinbase or peg-out coin is held back until it matures, and a later
    /// session has only the cursor to say which coins those are.
    #[test]
    fn a_maturing_coin_keeps_its_origin_through_the_cursor() {
        let ours = test_key(&LITECOIN, ScriptKind::P2wpkh, 1).unwrap();
        let coins: Vec<Utxo> = [Origin::Coinbase, Origin::Pegout]
            .into_iter()
            .zip(0u32..)
            .map(|(origin, vout)| {
                Utxo::new(
                    OutPoint { txid: fixture_txid(5), vout },
                    units(190_000),
                    ours.script().clone(),
                    Some(TIP),
                    origin,
                )
            })
            .collect();
        let cursor = SyncCursor::new(None, &coins, &[], Vec::new());
        let back = SyncCursor::decode(&cursor.encode().unwrap()).expect("it decodes");
        assert_eq!(back.restore(ours.script()), coins);
    }

    #[test]
    fn a_coin_count_larger_than_the_bytes_is_refused_before_anything_is_allocated() {
        let mut bytes = SyncCursor::new(None, &[], &[], Vec::new()).encode().unwrap();
        if let Some(count) = bytes.get_mut(4 + 1..4 + 1 + 4) {
            count.copy_from_slice(&u32::MAX.to_le_bytes());
        }
        assert!(SyncCursor::decode(&bytes).is_none());
    }

    #[test]
    fn a_spend_count_larger_than_the_bytes_is_refused_before_anything_is_allocated() {
        let pending = vec![CursorPending {
            txid: fixture_txid(1),
            direction: TxDirection::Outgoing,
            spends: Vec::new(),
        }];
        let mut bytes = SyncCursor::new(None, &[], &[], pending).encode().unwrap();
        let spend_count = bytes.len().saturating_sub(4);
        if let Some(count) = bytes.get_mut(spend_count..) {
            count.copy_from_slice(&u32::MAX.to_le_bytes());
        }
        assert!(SyncCursor::decode(&bytes).is_none());
    }

    #[test]
    fn an_empty_history_has_a_cursor_with_no_digest() {
        let cursor = SyncCursor::new(None, &[], &[], Vec::new());
        let bytes = cursor.encode().unwrap();
        assert_eq!(bytes.len(), 4 + 1 + 4 + 4 + 4);
        assert_eq!(SyncCursor::decode(&bytes), Some(cursor.clone()));
        assert!(cursor.matches(None));
        assert_eq!(cursor.balance(), Amount::ZERO);
        assert!(cursor.coins().is_empty());
    }
}
