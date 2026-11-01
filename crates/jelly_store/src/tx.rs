use jelly_core::{Amount, AssetId, TokenId, TxDetection, TxId, TxOutcome, TxRecord};
use rusqlite::params;

use crate::address::AddressOwner;
use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::Writer;

pub(crate) const TX: &str = "transaction";
const UNIQUE_COLUMNS: &str = "address_id, asset, txid";

statements! {
    /// `sort_time` is generated from `timestamp` and `created_at`.
    ///
    /// A balance-delta sighting fills gaps but never replaces a decoded transfer, and an
    /// unknown outcome never replaces a known one. A UTXO mempool sighting clears an
    /// earlier mined position and its block time, but never the first-seen time of a row
    /// that wasn't mined. An optimistic local send clears neither. A sighting that can't
    /// tell which transaction a speed-up pays for keeps the one an earlier sighting named.
    UPSERT =
        "INSERT INTO txs \
           (wallet_id, address_id, chain, asset, txid, direction, outcome, detection, amount, \
            fee, rent, counterparty, pays_for, height, timestamp, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
         ON CONFLICT (address_id, asset, txid) DO UPDATE SET \
           direction = CASE WHEN detection = 'full' AND excluded.detection = 'balance_delta' \
             THEN direction ELSE excluded.direction END, \
           outcome = CASE excluded.outcome WHEN 'unknown' THEN outcome \
             ELSE excluded.outcome END, \
           detection = CASE WHEN detection = 'full' THEN detection ELSE excluded.detection END, \
           amount = CASE WHEN detection = 'full' AND excluded.detection = 'balance_delta' \
             THEN amount ELSE excluded.amount END, \
           fee = COALESCE(excluded.fee, fee), \
           rent = COALESCE(excluded.rent, rent), \
           counterparty = CASE WHEN detection = 'full' AND excluded.detection = 'balance_delta' \
             THEN counterparty ELSE COALESCE(excluded.counterparty, counterparty) END, \
           pays_for = COALESCE(excluded.pays_for, pays_for), \
           height = CASE WHEN ?17 THEN excluded.height ELSE COALESCE(excluded.height, height) END, \
           timestamp = CASE WHEN ?17 AND height IS NOT NULL THEN excluded.timestamp \
             ELSE COALESCE(excluded.timestamp, timestamp) END";

    /// Deletes the zero-value native row that names `?6`, token `?5`'s contract, once the
    /// same address's token row of the transaction carries a fee.
    ///
    /// A history source can list the owner's call to a token contract as a native
    /// transaction of its own, even when the call reverted, as EVM indexers do. Whichever
    /// arrived first, the token row already says everything the native row does. The fees
    /// needn't match: on the OP Stack, an indexed row that got no fee from its receipt
    /// prices only the gas, without the L1 data fee.
    DROP_REDUNDANT_NATIVE =
        "DELETE FROM txs \
         WHERE txid = ?1 AND address_id = ?2 AND asset = ?3 AND amount = ?4 \
           AND counterparty = ?6 \
           AND EXISTS (SELECT 1 FROM txs k \
             WHERE k.txid = ?1 AND k.address_id = ?2 AND k.asset = ?5 AND k.fee IS NOT NULL)";

    /// Whether the wallet already holds a row of the transaction for the asset, at any of
    /// its addresses.
    EXISTS =
        "SELECT EXISTS (SELECT 1 FROM txs \
           WHERE chain = ?1 AND txid = ?2 AND wallet_id = ?3 AND asset = ?4)";

    CLEAR_INCLUSION =
        "UPDATE txs SET height = NULL, timestamp = NULL WHERE chain = ?1 AND txid = ?2";
}

/// What writing one record did to the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxWrite {
    /// The wallet had no row of the transaction for the asset before, so it's news.
    Inserted,
    /// The wallet already had a row of it, at this address or another, so the record is a
    /// later sighting or another address's share of a known transaction. A native call
    /// row that a token row makes redundant counts here too.
    Updated,
}

pub(crate) const HEIGHT: Column = Column::new(TX, "height");

impl Writer<'_> {
    /// The wallet and chain come from the address row, so a record can't be filed under a
    /// wallet that doesn't own its address.
    pub(crate) fn put_tx(
        &self,
        record: &TxRecord,
        owner: AddressOwner,
    ) -> Result<TxWrite, StoreError> {
        if record.asset.chain() != owner.chain {
            return Err(StoreError::Rejected {
                entity: TX,
                constraint: String::from("the asset and the address belong to different chains"),
            });
        }
        let height = codec::optional_u64_to_db(record.height, HEIGHT)?;
        // UTXO sync reports decoded mempool transactions as successful. A local
        // broadcast reports an unknown outcome and can't undo a mined position.
        let is_mempool_sighting = owner.chain.is_utxo()
            && record.height.is_none()
            && record.outcome == TxOutcome::Success
            && record.detection == TxDetection::Full;
        let asset = record.asset.key();
        let is_known: bool = sql::required(
            self.connection(),
            EXISTS,
            params![owner.chain.as_str(), record.txid.value(), owner.wallet_id, asset],
            |row| row.get(0).map_err(StoreError::from),
            "an existence check returned no row",
        )?;
        sql::run(
            self.connection(),
            UPSERT,
            params![
                owner.wallet_id,
                record.address_id,
                owner.chain.as_str(),
                asset,
                record.txid.value(),
                record.direction.as_str(),
                record.outcome.as_str(),
                record.detection.as_str(),
                codec::amount_to_blob(record.amount),
                record.fee.map(codec::amount_to_blob),
                record.rent.map(codec::amount_to_blob),
                record.counterparty,
                record.pays_for.as_ref().map(TxId::value),
                height,
                record.timestamp,
                self.now_unix_secs(),
                is_mempool_sighting,
            ],
        )
        .constrained(TX, UNIQUE_COLUMNS)?;
        let called = match record.asset {
            AssetId::Native(_) => TokenId::ALL.into_iter().find(|token| {
                token.chain() == owner.chain
                    && record.counterparty.as_deref() == Some(token.contract())
            }),
            AssetId::Token(token) => Some(token),
        };
        let dropped = match called {
            Some(token) => sql::run(
                self.connection(),
                DROP_REDUNDANT_NATIVE,
                params![
                    record.txid.value(),
                    record.address_id,
                    AssetId::Native(owner.chain).key(),
                    codec::amount_to_blob(Amount::ZERO),
                    AssetId::Token(token).key(),
                    token.contract(),
                ],
            )?,
            None => 0,
        };
        let is_redundant = matches!(record.asset, AssetId::Native(_)) && dropped > 0;
        Ok(if is_known || is_redundant { TxWrite::Updated } else { TxWrite::Inserted })
    }

    /// A reorg is chain-wide, so this clears the height and timestamp of every row, in
    /// every wallet, that records `txid` on its chain.
    pub fn clear_tx_inclusion(&self, txid: &TxId) -> Result<(), StoreError> {
        sql::run(self.connection(), CLEAR_INCLUSION, params![txid.chain().as_str(), txid.value()])?;
        Ok(())
    }
}
