use jelly_core::CursorBytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::XrpError;

const SCAN_VERSION: u8 = 1;
// Mainnet ledgers before this one were lost, so no server can serve them.
const FIRST_MAINNET_LEDGER: u32 = 32_570;

/// How far the history walk has read, stored per address, so its field names are a
/// stored format.
///
/// `next_ledger` is the lower bound of the next page's range, or `None` before the first
/// walk. `last_ledger` is the upper bound pinned while a walk is in progress, and `marker`
/// is the node's opaque token for resuming within that range.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    version: u8,
    next_ledger: Option<u32>,
    last_ledger: Option<u32>,
    marker: Option<Value>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self { version: SCAN_VERSION, next_ledger: None, last_ledger: None, marker: None }
    }
}

impl Cursor {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, XrpError> {
        let cursor: Self = serde_json::from_slice(bytes)
            .map_err(|_| XrpError::Internal("stored xrp history is unreadable"))?;
        if cursor.version != SCAN_VERSION {
            return Err(XrpError::Internal("stored xrp history has an unsupported format"));
        }
        Ok(cursor)
    }

    pub(crate) fn encode(&self) -> Result<CursorBytes, XrpError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| XrpError::Internal("the XRP history cursor did not serialize"))?;
        Ok(CursorBytes::new(bytes))
    }

    /// The first and last ledger of the range the next page reads. A walk that is not in
    /// progress reads through the validated `tip`.
    pub(crate) fn range(&self, tip: u32) -> (u32, u32) {
        (self.next_ledger.unwrap_or(FIRST_MAINNET_LEDGER), self.last_ledger.unwrap_or(tip))
    }

    pub(crate) const fn marker(&self) -> Option<&Value> {
        self.marker.as_ref()
    }

    /// Whether the next page's range starts after `tip`.
    pub(crate) fn is_past(&self, tip: u32) -> bool {
        self.next_ledger.is_some_and(|next| next > tip)
    }

    /// Whether every ledger through `tip` is read, with no range left part read.
    pub(crate) fn has_read_through(&self, tip: u32) -> bool {
        self.marker.is_none() && self.is_past(tip)
    }

    /// Moves past the page just read of the range at `tip`, whose last row is in
    /// `reached_ledger`.
    ///
    /// A marker means something only to the kind of server that wrote it. rippled's names the
    /// next row it has not returned and Clio's the last row it returned, so the other kind
    /// resuming it can skip a row. A page that reached a later ledger is resumed from that
    /// ledger, read again in full, and only a page that finished no ledger from its marker.
    pub(crate) fn mark_read(
        &mut self,
        marker: Option<Value>,
        reached_ledger: Option<u32>,
        tip: u32,
    ) -> Result<(), XrpError> {
        let (first_ledger, last_ledger) = self.range(tip);
        if reached_ledger.is_some_and(|reached| !(first_ledger..=last_ledger).contains(&reached)) {
            return Err(XrpError::Protocol);
        }
        match (marker, reached_ledger) {
            (None, _) => {
                self.next_ledger = Some(last_ledger.checked_add(1).ok_or(XrpError::Protocol)?);
                self.last_ledger = None;
                self.marker = None;
            }
            (Some(_), Some(reached)) if reached > first_ledger => {
                self.next_ledger = Some(reached);
                self.last_ledger = Some(last_ledger);
                self.marker = None;
            }
            (Some(marker), _) => {
                if self.marker.as_ref() == Some(&marker) {
                    return Err(XrpError::Protocol);
                }
                self.last_ledger = Some(last_ledger);
                self.marker = Some(marker);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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

    #[test]
    fn a_walk_keeps_its_first_tip_and_resumes_from_the_ledger_a_marked_page_reached() {
        let mut cursor = Cursor::default();
        assert_eq!(cursor.range(90_000_000), (FIRST_MAINNET_LEDGER, 90_000_000));
        cursor.mark_read(Some(json!("next")), Some(50_000_000), 90_000_000).unwrap();
        assert_eq!(cursor.range(90_000_009), (50_000_000, 90_000_000));
        assert_eq!(cursor.marker(), None);
        cursor.mark_read(None, Some(90_000_000), 90_000_009).unwrap();
        assert!(cursor.has_read_through(90_000_000));
        assert!(!cursor.has_read_through(90_000_001));
        assert_eq!(cursor.range(90_000_009), (90_000_001, 90_000_009));
    }

    #[test]
    fn a_repeated_marker_or_a_row_outside_the_range_is_refused() {
        let mut cursor = Cursor::default();
        cursor.mark_read(Some(json!("next")), None, 90_000_000).unwrap();
        assert_eq!(cursor.marker(), Some(&json!("next")));
        for (marker, reached_ledger) in [
            (Some(json!("next")), None),
            (None, Some(FIRST_MAINNET_LEDGER.saturating_sub(1))),
            (None, Some(90_000_001)),
        ] {
            assert!(matches!(
                cursor.mark_read(marker, reached_ledger, 90_000_000),
                Err(XrpError::Protocol)
            ));
        }
    }
}
