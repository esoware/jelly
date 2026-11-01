use hex::FromHex as _;
use jelly_core::{Amount, AssetId, ChainId, TxDetection, TxDirection, TxOutcome};
use rusqlite::{Row, ToSql, params, params_from_iter};

use crate::codec::{self, Column};
use crate::error::StoreError;
use crate::sql::{self, statements};
use crate::store::Reader;
use crate::tx::{HEIGHT, TX};

/// Every column `map` reads, from the row aliased `t`.
///
/// An unmined row reports its `sort_time` as its timestamp, so it shows a first-seen time
/// even when no sighting carried one or a reorg cleared its block time.
macro_rules! columns {
    () => {
        "t.address_id AS address_id, t.chain AS chain, t.asset AS asset, t.txid AS txid, \
         t.direction AS direction, t.outcome AS outcome, t.detection AS detection, \
         t.amount AS amount, t.fee AS fee, t.rent AS rent, t.counterparty AS counterparty, \
         t.pays_for AS pays_for, t.height AS height, \
         CASE WHEN t.height IS NULL THEN t.sort_time ELSE t.timestamp END AS timestamp"
    };
}

/// The direction of `t`'s transaction across all the wallet's rows of it: incoming when
/// none of them spent, outgoing when one paid an address outside the wallet or no address
/// at all, and self otherwise.
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
    /// Keyset paging. The position is always bound, so the comparison is an index range
    /// seek, not a scan.
    ///
    /// With no chain named, chains the wallet has switched off are filtered out here,
    /// before paging, so their rows never take up a page's slots.
    LIST =
        concat!(
            "SELECT t.id AS id, t.sort_time AS sort_time, ",
            columns!(),
            " FROM txs t \
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
             LIMIT ?9",
        );

    /// Keyset paging like `LIST`, over one lead row per transaction and asset, and leaving
    /// out the same switched-off chains.
    ///
    /// The lead row is the transaction's earliest, so its place in the history doesn't
    /// move as later rows of it arrive.
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
        concat!(
            "SELECT ",
            columns!(),
            ", own.id IS NOT NULL AS is_counterparty_own \
             FROM txs t \
             LEFT JOIN addresses own ON own.wallet_id = t.wallet_id AND own.chain = t.chain \
               AND own.address = t.counterparty \
             WHERE t.chain = ?1 AND t.txid = ?2 AND t.wallet_id = ?3 AND t.asset = ?4 \
             ORDER BY t.id ASC",
        );
}

pub const TX_PAGE_SIZE: u32 = 50;

/// No confirmation count, since that's derived from the current tip at read time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRow {
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
    /// The block time, or for an unmined row the first-seen time: the time a sighting
    /// reported, else when the row was first stored. Only a mined row with an unknown
    /// block time has none.
    pub timestamp_unix_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxFilter {
    pub wallet_id: i64,
    /// `None` lists every chain the wallet hasn't switched off.
    pub chain: Option<ChainId>,
    pub asset: Option<AssetId>,
    pub address_id: Option<i64>,
    pub txid: Option<String>,
    pub direction: Option<TxDirection>,
    pub cursor: Option<PageCursor>,
}

impl TxFilter {
    pub const fn new(wallet_id: i64) -> Self {
        Self {
            wallet_id,
            chain: None,
            asset: None,
            address_id: None,
            txid: None,
            direction: None,
            cursor: None,
        }
    }
}

/// A history filter over whole transactions rather than one address's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxGroupFilter {
    pub wallet_id: i64,
    /// `None` lists every chain the wallet hasn't switched off.
    pub chain: Option<ChainId>,
    pub asset: Option<AssetId>,
    pub txid: Option<String>,
    /// Matched against the direction the whole group takes.
    pub direction: Option<TxDirection>,
    pub cursor: Option<PageCursor>,
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

/// A transaction's lead row, which names the rows of its group.
struct Lead {
    chain: ChainId,
    asset: String,
    txid: String,
    direction: TxDirection,
}

const CHAIN: Column = Column::new(TX, "chain");
const ASSET: Column = Column::new(TX, "asset");
const DIRECTION: Column = Column::new(TX, "direction");
const OUTCOME: Column = Column::new(TX, "outcome");
const DETECTION: Column = Column::new(TX, "detection");
const AMOUNT: Column = Column::new(TX, "amount");
const FEE: Column = Column::new(TX, "fee");
const RENT: Column = Column::new(TX, "rent");

fn map(row: &Row<'_>) -> Result<TxRow, StoreError> {
    Ok(TxRow {
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

impl Reader<'_> {
    pub(crate) fn list_txs(&self, filter: TxFilter) -> Result<TxPage, StoreError> {
        let TxFilter { wallet_id, chain, asset, address_id, txid, direction, cursor } = filter;
        let filters = params![
            wallet_id,
            chain.map(ChainId::as_str),
            asset.map(AssetId::key),
            address_id,
            txid,
            direction.map(TxDirection::as_str),
        ];
        let (rows, next_cursor) = self.page(LIST, filters, cursor, map)?;
        Ok(TxPage { rows, next_cursor })
    }

    pub(crate) fn list_tx_groups(&self, filter: TxGroupFilter) -> Result<TxGroupPage, StoreError> {
        let TxGroupFilter { wallet_id, chain, asset, txid, direction, cursor } = filter;
        let filters = params![
            wallet_id,
            chain.map(ChainId::as_str),
            asset.map(AssetId::key),
            txid,
            direction.map(TxDirection::as_str),
        ];
        let (leads, next_cursor) = self.page(GROUP_LIST, filters, cursor, |row| {
            Ok(Lead {
                chain: codec::wire(row, CHAIN, ChainId::parse)?,
                asset: row.get("asset")?,
                txid: row.get("txid")?,
                direction: codec::wire(row, DIRECTION, TxDirection::parse)?,
            })
        })?;

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

    /// One page of `list` after `cursor`, with `filters` bound first. Each statement it
    /// pages selects `sort_time` and `id`, the row's keyset position.
    ///
    /// Fetching one row past the page tells whether there's a next page without a
    /// second query.
    fn page<T>(
        &self,
        list: &str,
        filters: &[&dyn ToSql],
        cursor: Option<PageCursor>,
        mut map: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<(Vec<T>, Option<PageCursor>), StoreError> {
        let position = cursor.unwrap_or(PageCursor::START);
        let probe = i64::from(TX_PAGE_SIZE).saturating_add(1);
        let mut rows = sql::all(
            self.connection(),
            list,
            params_from_iter(filters.iter().chain(params![position.sort_time, position.id, probe])),
            |row| Ok((map(row)?, PageCursor::new(row.get("sort_time")?, row.get("id")?))),
        )?;

        let wanted = usize::try_from(TX_PAGE_SIZE).unwrap_or(usize::MAX);
        let next_cursor = if rows.len() > wanted {
            rows.truncate(wanted);
            rows.last().map(|&(_, position)| position)
        } else {
            None
        };
        Ok((rows.into_iter().map(|(item, _)| item).collect(), next_cursor))
    }
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
