use jelly_core::{ChainId, SpkiPin};
use rusqlite::{Row, params};

use crate::codec::{self, Column};
use crate::error::StoreError;
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

const PIN: &str = "tls pin";

statements! {
    /// The certificate verifier in `jelly_net` decides whether a changed key may be
    /// recorded. A second rule here would let the stored pin drift from the one the
    /// running session compares against.
    UPSERT =
        "INSERT INTO tls_pins (chain, host, spki_sha256, first_seen) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (chain, host) DO UPDATE SET \
           spki_sha256 = excluded.spki_sha256, first_seen = excluded.first_seen";

    LIST = "SELECT chain, host, spki_sha256, first_seen FROM tls_pins ORDER BY chain ASC, host ASC";
}

/// Host names are case insensitive, so they are stored trimmed and lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRow {
    pub chain: ChainId,
    pub host: String,
    pub pin: SpkiPin,
}

const CHAIN: Column = Column::new(PIN, "chain");
const SPKI_SHA256: Column = Column::new(PIN, "spki_sha256");

fn map(row: &Row<'_>) -> Result<PinRow, StoreError> {
    Ok(PinRow {
        chain: codec::wire(row, CHAIN, ChainId::parse)?,
        host: row.get("host")?,
        pin: SpkiPin::new(codec::sha256(row, SPKI_SHA256)?, row.get("first_seen")?),
    })
}

impl Reader<'_> {
    pub(crate) fn list_pins(&self) -> Result<Vec<PinRow>, StoreError> {
        sql::all(self.connection(), LIST, [], map)
    }
}

impl Writer<'_> {
    pub(crate) fn put_pin(&self, row: &PinRow) -> Result<(), StoreError> {
        let PinRow { chain, host, pin } = row;
        sql::run(
            self.connection(),
            UPSERT,
            params![
                chain.as_str(),
                host.trim().to_ascii_lowercase(),
                pin.sha256().as_slice(),
                pin.first_seen_unix_secs(),
            ],
        )?;
        Ok(())
    }
}
