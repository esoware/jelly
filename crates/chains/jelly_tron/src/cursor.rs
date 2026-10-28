//! History cursors, each with a forward frontier and a backfill position.
//!
//! The forward walk reads from the newest entry down to the previous frontier, or
//! until the page budget runs out. A walk cut short resumes on the next pass, and
//! the frontier only moves once it gets there. It never moves above a transaction
//! reported with no block, so a later walk reads that one again and places it.
//!
//! The backfill resumes from the stored fingerprint and ends at an empty page or a
//! missing fingerprint. An unreadable cursor costs a rescan.

use serde::{Deserialize, Serialize};

use crate::route::Route;

/// The three history streams a walk covers.
///
/// Walks match on this instead of [`Route`], so every match pairing a stream with
/// its cursor, row shape, and path is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Stream {
    Native,
    Token,
    /// The transfers contracts made to or from the address.
    Internal,
}

impl Stream {
    /// Every stream, in the order a pass walks them. Internal goes last so it can
    /// skip a transaction the native stream already reported whole.
    pub(crate) const ALL: [Self; 3] = [Self::Native, Self::Token, Self::Internal];

    pub(crate) const fn route(self) -> Route {
        match self {
            Self::Native => Route::NativeHistory,
            Self::Token => Route::TokenHistory,
            Self::Internal => Route::InternalHistory,
        }
    }
}

/// One stream's forward frontier and backfill position.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamCursor {
    /// The newest txid reported on this stream that's below every one reported
    /// with no block. The forward walk stops when it sees it again.
    pub(crate) newest: Option<String>,
    /// The block time of that transaction, in milliseconds.
    ///
    /// A row older than this is behind the frontier even if the indexer has
    /// pruned the txid, which keeps the walk bounded on an endpoint that trims
    /// history.
    #[serde(rename = "newestMs")]
    pub(crate) newest_millis: Option<i64>,
    /// Set when a forward walk ran out of pages or time short of the frontier.
    pub(crate) resume: Option<Resume>,
    /// The indexer fingerprint the backfill resumes from.
    pub(crate) backfill: Option<String>,
    /// True once the backfill has reached the end of the stream.
    #[serde(rename = "complete")]
    pub(crate) is_complete: bool,
}

/// Where an interrupted forward walk picks up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Resume {
    /// The indexer fingerprint of the first page the walk hasn't read.
    pub(crate) fingerprint: String,
    /// The txid that becomes the frontier once the walk reaches the old one: the
    /// newest it read below every row it reported with no block. `None` if it
    /// hasn't read one yet.
    pub(crate) newest: Option<String>,
    #[serde(rename = "newestMs")]
    pub(crate) newest_millis: Option<i64>,
}

impl StreamCursor {
    /// Whether a row is at or behind the forward frontier, where the forward walk
    /// stops.
    pub(crate) fn is_caught_up(&self, txid: &str, timestamp_millis: Option<i64>) -> bool {
        if self.newest.as_deref() == Some(txid) {
            return true;
        }
        match (self.newest_millis, timestamp_millis) {
            (Some(frontier), Some(seen)) => seen < frontier,
            _ => false,
        }
    }

    /// Call only after the row is emitted, so the frontier never moves over
    /// unreported history.
    pub(crate) fn advance_forward(&mut self, txid: &str, timestamp_millis: Option<i64>) {
        self.newest = Some(txid.to_owned());
        self.newest_millis = timestamp_millis;
    }

    /// Records where the backfill stopped after a page of `row_count` rows, or
    /// that it has finished.
    pub(crate) fn advance_backfill(&mut self, fingerprint: Option<&str>, row_count: usize) {
        if row_count == 0 {
            self.finish_backfill();
        } else {
            self.backfill_from(fingerprint);
        }
    }

    /// Records that the stream has been walked all the way back.
    pub(crate) const fn finish_backfill(&mut self) {
        self.is_complete = true;
    }

    /// Starts the backfill at `fingerprint`, or records that there's nothing older
    /// to read.
    pub(crate) fn backfill_from(&mut self, fingerprint: Option<&str>) {
        match fingerprint {
            Some(cursor) => self.backfill = Some(cursor.to_owned()),
            None => self.finish_backfill(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_stream_names_one_history_route() {
        assert_eq!(Stream::Native.route(), Route::NativeHistory);
        assert_eq!(Stream::Token.route(), Route::TokenHistory);
        assert_eq!(Stream::Internal.route(), Route::InternalHistory);
        for stream in Stream::ALL {
            assert!(stream.route().is_indexer(), "{stream:?}");
        }
    }

    #[test]
    fn a_fresh_cursor_is_caught_up_to_nothing() {
        let cursor = StreamCursor::default();
        assert!(!cursor.is_complete);
        assert!(!cursor.is_caught_up("aa", Some(9)));
        assert_eq!(cursor.backfill, None);
    }

    #[test]
    fn the_forward_frontier_stops_on_the_id_it_last_reported() {
        let mut cursor = StreamCursor::default();
        cursor.advance_forward("aa", Some(1_000));
        assert!(cursor.is_caught_up("aa", Some(1_000)));
        assert!(!cursor.is_caught_up("bb", Some(1_001)), "a newer row is not behind the frontier");
        assert!(
            cursor.is_caught_up("cc", Some(999)),
            "an older row is behind the frontier even under a different id"
        );
        assert!(
            !cursor.is_caught_up("cc", None),
            "a row with no time of its own is never assumed to be old"
        );
    }

    #[test]
    fn an_empty_page_ends_the_backfill_and_a_fingerprint_continues_it() {
        let mut cursor = StreamCursor::default();
        cursor.advance_backfill(Some("Fp1"), 2);
        assert_eq!(cursor.backfill.as_deref(), Some("Fp1"));
        assert!(!cursor.is_complete);

        cursor.advance_backfill(Some("Fp2"), 0);
        assert!(cursor.is_complete, "a fingerprint with no rows is the end of the stream");
        assert_eq!(cursor.backfill.as_deref(), Some("Fp1"), "the last useful cursor is kept");

        let mut dry = StreamCursor::default();
        dry.advance_backfill(None, 3);
        assert!(dry.is_complete, "no fingerprint at all is also the end");
    }

    #[test]
    fn a_backfill_starts_wherever_the_forward_walk_stopped() {
        let mut started = StreamCursor::default();
        started.backfill_from(Some("Fp1"));
        assert_eq!(started.backfill.as_deref(), Some("Fp1"));
        assert!(!started.is_complete);

        let mut ended = StreamCursor::default();
        ended.backfill_from(None);
        assert!(ended.is_complete, "a forward walk that reached the end leaves no backfill");
    }

    #[test]
    fn a_cursor_round_trips_through_its_stored_form() {
        let mut cursor = StreamCursor::default();
        cursor.advance_forward("aa", Some(7));
        cursor.advance_backfill(Some("Fp1"), 1);
        cursor.resume = Some(Resume {
            fingerprint: String::from("Fp2"),
            newest: Some(String::from("bb")),
            newest_millis: Some(9),
        });
        let json = serde_json::to_string(&cursor).expect("it encodes");
        assert_eq!(serde_json::from_str::<StreamCursor>(&json).expect("it decodes"), cursor);

        let stored: StreamCursor = serde_json::from_str(
            r#"{"newest":"aa","newestMs":7,"resume":{"fingerprint":"Fp2","newest":"bb",
                "newestMs":9},"backfill":null,"complete":true}"#,
        )
        .expect("a stored cursor");
        assert_eq!(stored.newest_millis, Some(7), "the stored key spellings read back");
        assert_eq!(stored.resume.and_then(|resume| resume.newest_millis), Some(9));
        assert!(stored.is_complete);
    }
}
