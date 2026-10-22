use std::collections::BTreeMap;

use jelly_core::CursorBytes;
use serde::{Deserialize, Serialize};

use crate::error::ZcashError;
use crate::record::Record;

const SCAN_VERSION: u8 = 1;

/// How far the history scan has read, stored per address as the `zcash.transactions` scan
/// cursor, so its field names, and those of the records in it, are a stored format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    version: u8,
    pub(crate) tip_height: u32,
    pub(crate) tip_hash: String,
    pub(crate) records: BTreeMap<String, Record>,
    /// Records read against a tip a reorg has since replaced that no pass has fetched again.
    /// They are never reused, and a complete pass either fetches each again or reports it
    /// dropped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) unverified: BTreeMap<String, Record>,
    pub(crate) is_complete: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new(0, String::new(), BTreeMap::new(), BTreeMap::new(), false)
    }
}

impl Cursor {
    pub(crate) const fn new(
        tip_height: u32,
        tip_hash: String,
        records: BTreeMap<String, Record>,
        unverified: BTreeMap<String, Record>,
        is_complete: bool,
    ) -> Self {
        Self { version: SCAN_VERSION, tip_height, tip_hash, records, unverified, is_complete }
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ZcashError> {
        let cursor: Self = serde_json::from_slice(bytes)
            .map_err(|_| ZcashError::Internal("stored zcash history is unreadable"))?;
        if cursor.version != SCAN_VERSION {
            return Err(ZcashError::Internal("stored zcash history has an unsupported format"));
        }
        Ok(cursor)
    }

    pub(crate) fn encode(&self) -> Result<CursorBytes, ZcashError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| ZcashError::Internal("the Zcash scan cursor did not serialize"))?;
        Ok(CursorBytes::new(bytes))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_cursor_round_trips_and_one_of_another_or_no_version_is_refused() {
        let bytes = Cursor::default().encode().unwrap().with_bytes(<[u8]>::to_vec);
        let mut wire: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&Cursor::decode(&bytes).unwrap()).unwrap(), bytes);
        wire["version"] = json!(SCAN_VERSION.saturating_add(1));
        Cursor::decode(&serde_json::to_vec(&wire).unwrap()).unwrap_err();
        wire.as_object_mut().unwrap().remove("version");
        Cursor::decode(&serde_json::to_vec(&wire).unwrap()).unwrap_err();
    }
}
