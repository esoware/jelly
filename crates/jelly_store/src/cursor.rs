use jelly_core::{
    CursorBytes, CursorScope, HeldReservation, ReservationSlot, ScanAdvance, ScanStream,
};
use rusqlite::{Row, params};
use serde_json::Value;

use crate::address::ADDRESS;
use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

const SCAN_CURSOR: &str = "scan cursor";

statements! {
    UPSERT =
        "INSERT INTO scan_cursors (address_id, namespace, cursor, updated_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (address_id, namespace) DO UPDATE SET \
           cursor = excluded.cursor, updated_at = excluded.updated_at";

    /// The two lists arrive as JSON arrays, so one cached statement serves any number of
    /// addresses.
    LIST =
        "SELECT address_id, namespace, cursor FROM scan_cursors \
         WHERE address_id IN (SELECT value FROM json_each(?1)) \
           AND namespace IN (SELECT value FROM json_each(?2))";

    /// The caller names only scan streams, because reservations share this table and are
    /// not safe to drop.
    FORGET_SCANS =
        "DELETE FROM scan_cursors \
         WHERE address_id IN (SELECT value FROM json_each(?1)) \
           AND namespace IN (SELECT value FROM json_each(?2))";
}

const NAMESPACE: Column = Column::new(SCAN_CURSOR, "namespace");

fn json_array<T: Into<Value>>(values: impl IntoIterator<Item = T>) -> String {
    values.into_iter().collect::<Value>().to_string()
}

impl Reader<'_> {
    /// Pairs that were never written are absent rather than empty.
    pub(crate) fn load_scans(
        &self,
        scopes: &[CursorScope],
        streams: &[ScanStream],
    ) -> Result<Vec<ScanAdvance>, StoreError> {
        let names = streams.iter().copied().map(ScanStream::as_str);
        self.load_cursors(scopes, names, |row| {
            Ok(ScanAdvance {
                scope: CursorScope::Address(row.get("address_id")?),
                stream: codec::wire(row, NAMESPACE, ScanStream::parse)?,
                cursor: CursorBytes::new(row.get("cursor")?),
            })
        })
    }

    pub(crate) fn load_reservations(
        &self,
        scopes: &[CursorScope],
        slots: &[ReservationSlot],
    ) -> Result<Vec<HeldReservation>, StoreError> {
        let names = slots.iter().copied().map(ReservationSlot::as_str);
        self.load_cursors(scopes, names, |row| {
            Ok(HeldReservation {
                scope: CursorScope::Address(row.get("address_id")?),
                slot: codec::wire(row, NAMESPACE, ReservationSlot::parse)?,
                payload: CursorBytes::new(row.get("cursor")?),
            })
        })
    }

    /// The rows of `scopes` under `names`. Scan streams and reservation slots share the
    /// table, so the caller decodes the namespace as the kind it asked for.
    ///
    /// A cursor is filed under its address row, so a wallet scope has nothing here.
    fn load_cursors<T>(
        &self,
        scopes: &[CursorScope],
        names: impl IntoIterator<Item = &'static str>,
        map: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        let address_ids = scopes.iter().filter_map(|scope| scope.address_id());
        let lists = params![json_array(address_ids), json_array(names)];
        sql::all(self.connection(), LIST, lists, map)
    }
}

impl Writer<'_> {
    pub(crate) fn put_scan(&self, advance: &ScanAdvance) -> Result<(), StoreError> {
        self.put_cursor(advance.scope, advance.stream.as_str(), &advance.cursor)
    }

    /// The one cursor write that does not ride a [`crate::SyncBatch`]. Every adapter that
    /// holds a plain `ReservationSlot` calls it before signed bytes leave the process, so
    /// its transaction must commit before broadcast.
    pub(crate) fn reserve(
        &self,
        scope: CursorScope,
        slot: ReservationSlot,
        payload: &CursorBytes,
    ) -> Result<(), StoreError> {
        self.put_cursor(scope, slot.as_str(), payload)
    }

    /// A wallet scope has no address row to file the cursor under, so it is refused.
    fn put_cursor(
        &self,
        scope: CursorScope,
        namespace: &str,
        cursor: &CursorBytes,
    ) -> Result<(), StoreError> {
        let address_id =
            scope.address_id().ok_or(StoreError::MissingReference { entity: SCAN_CURSOR })?;
        cursor.with_bytes(|bytes| {
            sql::run(
                self.connection(),
                UPSERT,
                params![address_id, namespace, bytes, self.now_unix_secs()],
            )
            .referring_to(ADDRESS, address_id)
        })?;
        Ok(())
    }

    /// Forgets the reconstructible streams of `address_ids` and keeps every other stream
    /// and every reservation.
    pub(crate) fn forget_reconstructible_scans(
        &self,
        address_ids: &[i64],
    ) -> Result<(), StoreError> {
        if address_ids.is_empty() {
            return Ok(());
        }
        let names = ScanStream::ALL
            .into_iter()
            .filter(|stream| stream.is_reconstructible())
            .map(ScanStream::as_str);
        sql::run(
            self.connection(),
            FORGET_SCANS,
            params![json_array(address_ids.iter().copied()), json_array(names)],
        )?;
        Ok(())
    }
}
