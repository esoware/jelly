use hex::FromHex as _;
use jelly_core::{
    Amount, AssetId, ChainId, TokenId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord,
};
use rusqlite::{Row, ToSql, params, params_from_iter};

use crate::address::AddressOwner;
use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

const TX: &str = "transaction";
const UNIQUE_COLUMNS: &str = "address_id, asset, txid";

/// The direction the wallet's rows of `t`'s transaction give it together.
///
/// It is incoming when none of them spent, outgoing when one paid an address that is not
/// the wallet's or paid no address at all, and a self-transfer otherwise.
macro_rules! group_direction {
    () => {
        "CASE \
           WHEN NOT EXISTS (SELECT 1 FROM txs s \
             WHERE s.chain = t.chain AND s.txid = t.txid AND s.wallet_id = t.wallet_id \
               AND s.asset = t.asset AND s.direction IN ('outgoing','self')) THEN 'incoming' \
           WHEN EXISTS (SELECT 1 FROM txs s \
             LEFT JOIN addresses own ON own.wallet_id = s.wallet_id AND own.chain = s.chain \
               AND own.address = s.counterparty \
             WHERE s.chain = t.chain AND s.txid = t.txid AND s.wallet_id = t.wallet_id \
               AND s.asset = t.asset AND s.direction = 'outgoing' AND own.id IS NULL) \
             THEN 'outgoing' \
           ELSE 'self' \
         END"
    };
}

statements! {
    /// `sort_time` is generated from `timestamp` and `created_at`.
    ///
    /// Weak sightings fill gaps without replacing decoded transfers. A UTXO mempool
    /// sighting clears an earlier mined position and its block time, but never the
    /// first sighting of a row that was not mined; optimistic local sends clear neither.
    /// A sighting that cannot tell which transaction a speed-up pays for keeps the
    /// one an earlier sighting named.
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

    /// A native row that moved nothing and names `?7`, the contract of token `?6`, goes
    /// when that token's row of the transaction carries the same fee.
    ///
    /// Both are one address's rows of one transaction, so the native row says nothing
    /// the other does not, whichever arrived first. An EVM indexer lists the owner's call
    /// to a token contract as a native transaction of its own, even when the call
    /// reverted.
    DROP_REDUNDANT_NATIVE =
        "DELETE FROM txs \
         WHERE chain = ?1 AND txid = ?2 AND address_id = ?3 AND asset = ?4 AND amount = ?5 \
           AND counterparty = ?7 \
           AND EXISTS (SELECT 1 FROM txs k \
             WHERE k.chain = ?1 AND k.txid = ?2 AND k.address_id = ?3 AND k.asset = ?6 \
               AND k.fee = txs.fee)";

    /// Whether the wallet already holds a row of the transaction for the asset, at any of
    /// its addresses.
    EXISTS =
        "SELECT EXISTS (SELECT 1 FROM txs \
           WHERE chain = ?1 AND txid = ?2 AND wallet_id = ?3 AND asset = ?4)";

    CLEAR_INCLUSION =
        "UPDATE txs SET height = NULL, timestamp = NULL WHERE chain = ?1 AND txid = ?2";

    /// Keyset paging. The position is always bound, so the comparison is an index range
    /// seek rather than a scan.
    ///
    /// An unmined row reads as the time it sorts by, so it has a first sighting even
    /// when no sighting carried a time or a reorg cleared its block time.
    ///
    /// With no chain named, the chains the wallet has switched off are left out here
    /// rather than after paging, so their rows never take a page's places.
    LIST =
        "SELECT t.id AS id, t.address_id AS address_id, t.chain AS chain, \
           t.asset AS asset, t.txid AS txid, t.direction AS direction, \
           t.outcome AS outcome, t.detection AS detection, t.amount AS amount, \
           t.fee AS fee, t.rent AS rent, t.counterparty AS counterparty, t.pays_for AS pays_for, \
           t.height AS height, \
           CASE WHEN t.height IS NULL THEN t.sort_time ELSE t.timestamp END AS timestamp, \
           t.sort_time AS sort_time \
         FROM txs t \
         WHERE t.wallet_id = ?1 \
           AND (?2 IS NULL OR t.chain = ?2) \
           AND (?2 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM wallet_chain_state c \
             WHERE c.wallet_id = t.wallet_id AND c.chain = t.chain AND c.enabled = 0)) \
           AND (?3 IS NULL OR t.asset = ?3) \
           AND (?4 IS NULL OR t.address_id = ?4) \
           AND (?5 IS NULL OR t.txid = ?5) \
           AND (?6 IS NULL OR t.direction = ?6) \
           AND (t.sort_time, t.id) < (?7, ?8) \
         ORDER BY t.sort_time DESC, t.id DESC \
         LIMIT ?9";

    /// Keyset paging over lead rows, as `LIST` pages over rows, and leaving out the
    /// same chains.
    ///
    /// There is one lead row per transaction and asset. A transaction's lead row is its
    /// earliest, so its place in the history does not move as later rows of it arrive.
    GROUP_LIST =
        concat!(
            "SELECT t.id AS id, t.chain AS chain, t.asset AS asset, t.txid AS txid, \
               t.sort_time AS sort_time, ",
            group_direction!(),
            " AS direction \
             FROM txs t \
             WHERE t.wallet_id = ?1 \
               AND (?2 IS NULL OR t.chain = ?2) \
               AND (?2 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM wallet_chain_state c \
                 WHERE c.wallet_id = t.wallet_id AND c.chain = t.chain AND c.enabled = 0)) \
               AND (?3 IS NULL OR t.asset = ?3) \
               AND (?4 IS NULL OR t.txid = ?4) \
               AND NOT EXISTS (SELECT 1 FROM txs e \
                 WHERE e.chain = t.chain AND e.txid = t.txid AND e.wallet_id = t.wallet_id \
                   AND e.asset = t.asset AND (e.sort_time, e.id) < (t.sort_time, t.id)) \
               AND (?5 IS NULL OR ",
            group_direction!(),
            " = ?5) \
               AND (t.sort_time, t.id) < (?6, ?7) \
             ORDER BY t.sort_time DESC, t.id DESC \
             LIMIT ?8",
        );

    /// Every row of one transaction and asset in the wallet, with whether each names
    /// another of the wallet's addresses as its counterparty.
    MEMBERS =
        "SELECT t.id AS id, t.address_id AS address_id, t.chain AS chain, \
           t.asset AS asset, t.txid AS txid, t.direction AS direction, \
           t.outcome AS outcome, t.detection AS detection, t.amount AS amount, \
           t.fee AS fee, t.rent AS rent, t.counterparty AS counterparty, t.pays_for AS pays_for, \
           t.height AS height, \
           CASE WHEN t.height IS NULL THEN t.sort_time ELSE t.timestamp END AS timestamp, \
           own.id IS NOT NULL AS is_counterparty_own \
         FROM txs t \
         LEFT JOIN addresses own ON own.wallet_id = t.wallet_id AND own.chain = t.chain \
           AND own.address = t.counterparty \
         WHERE t.chain = ?1 AND t.txid = ?2 AND t.wallet_id = ?3 AND t.asset = ?4 \
         ORDER BY t.id ASC";
}

/// What writing one record did to the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxWrite {
    /// The wallet held no row of the transaction for the asset before, so it is news.
    Inserted,
    /// The wallet already held a row of it, at this address or another, so the record
    /// is a later sighting or another address's share of a known transaction. A record
    /// that a row of another asset already says everything of is no news either.
    Updated,
}

/// Confirmations are absent because they are derived from the current tip at read time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRow {
    pub id: i64,
    pub address_id: i64,
    pub chain: ChainId,
    pub asset: AssetId,
    pub txid: String,
    pub direction: TxDirection,
    pub outcome: TxOutcome,
    pub detection: TxDetection,
    pub amount: Amount,
    pub fee: Option<Amount>,
    pub rent: Option<Amount>,
    pub counterparty: Option<String>,
    /// The unconfirmed transaction a child-pays-for-parent speed-up pays for.
    pub pays_for: Option<String>,
    pub height: Option<u64>,
    /// The block's time, or while unmined the first sighting: the time a sighting
    /// reported, else the time the row was first stored. Only a mined row whose block
    /// time is unknown has none.
    pub timestamp_unix_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxFilter {
    pub wallet_id: i64,
    /// `None` lists every chain the wallet has not switched off.
    pub chain: Option<ChainId>,
    pub asset: Option<AssetId>,
    pub address_id: Option<i64>,
    pub txid: Option<String>,
    pub direction: Option<TxDirection>,
    pub cursor: Option<PageCursor>,
    /// Clamped to `1..=MAX_LIMIT`. Callers that treat zero as the default must map it to
    /// `DEFAULT_LIMIT` first.
    pub limit: u32,
}

impl TxFilter {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 500;

    pub const fn new(wallet_id: i64) -> Self {
        Self {
            wallet_id,
            chain: None,
            asset: None,
            address_id: None,
            txid: None,
            direction: None,
            cursor: None,
            limit: Self::DEFAULT_LIMIT,
        }
    }
}

/// A history filter over whole transactions rather than one address's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxGroupFilter {
    pub wallet_id: i64,
    /// `None` lists every chain the wallet has not switched off.
    pub chain: Option<ChainId>,
    pub asset: Option<AssetId>,
    pub txid: Option<String>,
    /// Matched against the direction the whole group takes.
    pub direction: Option<TxDirection>,
    pub cursor: Option<PageCursor>,
    /// Clamped to `1..=TxFilter::MAX_LIMIT`.
    pub limit: u32,
}

/// One row of a transaction, with whether its counterparty is another of the wallet's
/// addresses on the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxMember {
    pub row: TxRow,
    pub is_counterparty_own: bool,
}

/// One transaction of one asset as the wallet sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxGroup {
    /// The direction the members give the transaction together.
    pub direction: TxDirection,
    /// Every row the wallet holds of it, oldest first, never empty.
    pub members: Vec<TxMember>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxGroupPage {
    pub groups: Vec<TxGroup>,
    pub next_cursor: Option<PageCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxPage {
    pub rows: Vec<TxRow>,
    pub next_cursor: Option<PageCursor>,
}

/// An opaque keyset position over `(sort_time DESC, id DESC)`. The `id` tiebreak makes
/// the order total, so no page repeats a row.
///
/// `sort_time` is the transaction's own timestamp, or the time it was first stored when
/// it has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    sort_time: i64,
    id: i64,
}

impl PageCursor {
    const TOKEN_BYTES: usize = 16;

    /// The position before the newest row. Binding it instead of branching on a missing
    /// cursor keeps the comparison an index range.
    const START: Self = Self { sort_time: i64::MAX, id: i64::MAX };

    const fn new(sort_time: i64, id: i64) -> Self {
        Self { sort_time, id }
    }

    pub fn parse(token: &str) -> Option<Self> {
        let raw = <[u8; Self::TOKEN_BYTES]>::from_hex(token).ok()?;
        let (sort_time, id) = raw.split_at(8);
        Some(Self::new(
            i64::from_be_bytes(sort_time.try_into().ok()?),
            i64::from_be_bytes(id.try_into().ok()?),
        ))
    }

    pub fn token(self) -> String {
        hex::encode([self.sort_time.to_be_bytes(), self.id.to_be_bytes()].concat())
    }
}

const CHAIN: Column = Column::new(TX, "chain");
const ASSET: Column = Column::new(TX, "asset");
const DIRECTION: Column = Column::new(TX, "direction");
const OUTCOME: Column = Column::new(TX, "outcome");
const DETECTION: Column = Column::new(TX, "detection");
const AMOUNT: Column = Column::new(TX, "amount");
const FEE: Column = Column::new(TX, "fee");
const RENT: Column = Column::new(TX, "rent");
const HEIGHT: Column = Column::new(TX, "height");

fn map(row: &Row<'_>) -> Result<TxRow, StoreError> {
    Ok(TxRow {
        id: row.get("id")?,
        address_id: row.get("address_id")?,
        chain: codec::wire(row, CHAIN, ChainId::parse)?,
        asset: codec::wire(row, ASSET, AssetId::parse_key)?,
        txid: row.get("txid")?,
        direction: codec::wire(row, DIRECTION, TxDirection::parse)?,
        outcome: codec::wire(row, OUTCOME, TxOutcome::parse)?,
        detection: codec::wire(row, DETECTION, TxDetection::parse)?,
        amount: codec::amount(row, AMOUNT)?,
        fee: codec::optional_amount(row, FEE)?,
        rent: codec::optional_amount(row, RENT)?,
        counterparty: row.get("counterparty")?,
        pays_for: row.get("pays_for")?,
        height: codec::optional_int(row, HEIGHT)?,
        timestamp_unix_secs: row.get("timestamp")?,
    })
}

impl Writer<'_> {
    /// The wallet and chain come from the address row, so a record cannot be filed under a
    /// wallet that does not own its address.
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
        // broadcast reports an unknown outcome and cannot undo a mined position.
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
            AssetId::Native(_) if record.amount.is_zero() => {
                record.counterparty.as_deref().and_then(|party| {
                    TokenId::ALL
                        .into_iter()
                        .find(|token| token.chain() == owner.chain && token.contract() == party)
                })
            }
            AssetId::Native(_) => None,
            AssetId::Token(token) => record.fee.is_some().then_some(token),
        };
        let dropped = match called {
            Some(token) => sql::run(
                self.connection(),
                DROP_REDUNDANT_NATIVE,
                params![
                    owner.chain.as_str(),
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
    /// every wallet, that records `txid` on its chain, and nothing else.
    pub fn clear_tx_inclusion(&self, txid: &TxId) -> Result<(), StoreError> {
        sql::run(self.connection(), CLEAR_INCLUSION, params![txid.chain().as_str(), txid.value()])?;
        Ok(())
    }
}

impl Reader<'_> {
    /// Newest first.
    pub(crate) fn list_txs(&self, filter: TxFilter) -> Result<TxPage, StoreError> {
        let TxFilter { wallet_id, chain, asset, address_id, txid, direction, cursor, limit } =
            filter;
        let chain = chain.map(ChainId::as_str);
        let asset = asset.map(AssetId::key);
        let direction = direction.map(TxDirection::as_str);
        let (rows, next_cursor) = self.page(
            LIST,
            params![wallet_id, chain, asset, address_id, txid, direction],
            cursor,
            limit,
            |row| {
                let tx = map(row)?;
                let position = PageCursor::new(row.get("sort_time")?, tx.id);
                Ok((tx, position))
            },
        )?;
        Ok(TxPage { rows, next_cursor })
    }

    /// Newest first, one entry per transaction and asset.
    pub(crate) fn list_tx_groups(&self, filter: TxGroupFilter) -> Result<TxGroupPage, StoreError> {
        let TxGroupFilter { wallet_id, chain, asset, txid, direction, cursor, limit } = filter;
        let chain = chain.map(ChainId::as_str);
        let asset = asset.map(AssetId::key);
        let direction = direction.map(TxDirection::as_str);
        let (leads, next_cursor) = self.page(
            GROUP_LIST,
            params![wallet_id, chain, asset, txid, direction],
            cursor,
            limit,
            |row| {
                let lead = Lead {
                    chain: codec::wire(row, CHAIN, ChainId::parse)?,
                    asset: row.get("asset")?,
                    txid: row.get("txid")?,
                    direction: codec::wire(row, DIRECTION, TxDirection::parse)?,
                };
                Ok((lead, PageCursor::new(row.get("sort_time")?, row.get("id")?)))
            },
        )?;

        let mut groups = Vec::with_capacity(leads.len());
        for lead in leads {
            let members = sql::all(
                self.connection(),
                MEMBERS,
                params![lead.chain.as_str(), lead.txid, wallet_id, lead.asset],
                |row| {
                    Ok(TxMember {
                        row: map(row)?,
                        is_counterparty_own: row.get("is_counterparty_own")?,
                    })
                },
            )?;
            groups.push(TxGroup { direction: lead.direction, members });
        }
        Ok(TxGroupPage { groups, next_cursor })
    }

    /// One page of `list` from `cursor` on, bound to `filters`. `map` answers each row
    /// with its keyset position.
    ///
    /// `limit` is clamped to `1..=TxFilter::MAX_LIMIT`. The page asks for one row past
    /// it, which detects a next page without a second query.
    fn page<T>(
        &self,
        list: &str,
        filters: &[&dyn ToSql],
        cursor: Option<PageCursor>,
        limit: u32,
        map: impl FnMut(&Row<'_>) -> Result<(T, PageCursor), StoreError>,
    ) -> Result<(Vec<T>, Option<PageCursor>), StoreError> {
        let limit = limit.clamp(1, TxFilter::MAX_LIMIT);
        let position = cursor.unwrap_or(PageCursor::START);
        let probe = i64::from(limit).saturating_add(1);
        let mut rows = sql::all(
            self.connection(),
            list,
            params_from_iter(filters.iter().chain(params![position.sort_time, position.id, probe])),
            map,
        )?;

        let wanted = usize::try_from(limit).unwrap_or(usize::MAX);
        let next_cursor = if rows.len() > wanted {
            rows.truncate(wanted);
            rows.last().map(|&(_, position)| position)
        } else {
            None
        };
        Ok((rows.into_iter().map(|(item, _)| item).collect(), next_cursor))
    }
}

/// A transaction's lead row, which names the rows of its group.
struct Lead {
    chain: ChainId,
    asset: String,
    txid: String,
    direction: TxDirection,
}

#[cfg(test)]
mod tests {
    use super::PageCursor;

    #[test]
    fn a_page_token_survives_a_round_trip_through_its_text() {
        for cursor in [
            PageCursor::START,
            PageCursor::new(0, 0),
            PageCursor::new(1_700_000_000, 42),
            PageCursor::new(i64::MIN, i64::MIN),
        ] {
            assert_eq!(PageCursor::parse(&cursor.token()), Some(cursor));
            assert_eq!(cursor.token().len(), PageCursor::TOKEN_BYTES * 2);
        }
    }

    #[test]
    fn text_this_build_did_not_issue_is_not_a_position() {
        for token in ["", "00", "not-hex", &"00".repeat(15), &"00".repeat(17)] {
            assert_eq!(PageCursor::parse(token), None, "{token}");
        }
    }
}
