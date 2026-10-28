use jelly_core::{ChainId, EndpointHealth, EndpointHealthRecord, EndpointLabel, EndpointOverride};
use rusqlite::{Row, params};

use crate::codec::{self, Column};
use crate::error::{Classify as _, StoreError};
use crate::sql::{self, statements};
use crate::store::{Reader, Writer};

const ENDPOINT: &str = "endpoint";
const HEALTH: &str = "endpoint health";
const UNIQUE_COLUMNS: &str = "chain, url";

statements! {
    /// Re-adding a listed endpoint re-enables and relabels it but keeps its pin, since
    /// typing it again isn't a way to unpin it.
    ADD =
        "INSERT INTO endpoint_overrides (chain, url, display, enabled, pinned) \
         VALUES (?1, ?2, ?3, 1, 0) \
         ON CONFLICT (chain, url) DO UPDATE SET \
           display = excluded.display, enabled = 1";

    /// Creates the row if it's missing, and otherwise touches only this flag.
    SET_ENABLED =
        "INSERT INTO endpoint_overrides (chain, url, display, enabled, pinned) \
         VALUES (?1, ?2, ?2, ?3, 0) \
         ON CONFLICT (chain, url) DO UPDATE SET enabled = excluded.enabled";

    SET_PINNED =
        "INSERT INTO endpoint_overrides (chain, url, display, enabled, pinned) \
         VALUES (?1, ?2, ?2, 1, ?3) \
         ON CONFLICT (chain, url) DO UPDATE SET pinned = excluded.pinned";

    LIST =
        "SELECT chain, url, display, enabled, pinned FROM endpoint_overrides \
         ORDER BY chain ASC, url ASC";

    DELETE = "DELETE FROM endpoint_overrides WHERE chain = ?1 AND url = ?2";

    UPSERT_HEALTH =
        "INSERT INTO endpoint_health \
           (chain, url, score_permille, successes, failures, consecutive_failures, latency_ms, \
            last_success, last_failure, last_error) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
         ON CONFLICT (chain, url) DO UPDATE SET \
           score_permille = excluded.score_permille, \
           successes = excluded.successes, \
           failures = excluded.failures, \
           consecutive_failures = excluded.consecutive_failures, \
           latency_ms = excluded.latency_ms, \
           last_success = excluded.last_success, \
           last_failure = excluded.last_failure, \
           last_error = excluded.last_error";

    LIST_HEALTH =
        "SELECT chain, url, score_permille, successes, failures, consecutive_failures, \
           latency_ms, last_success, last_failure, last_error FROM endpoint_health \
         ORDER BY chain ASC, url ASC";
}

const CHAIN: Column = Column::new(ENDPOINT, "chain");
const HEALTH_CHAIN: Column = Column::new(HEALTH, "chain");
const SCORE_PERMILLE: Column = Column::new(HEALTH, "score_permille");
const SUCCESSES: Column = Column::new(HEALTH, "successes");
const FAILURES: Column = Column::new(HEALTH, "failures");
const CONSECUTIVE_FAILURES: Column = Column::new(HEALTH, "consecutive_failures");
const LATENCY_MS: Column = Column::new(HEALTH, "latency_ms");

fn map(row: &Row<'_>) -> Result<(ChainId, EndpointOverride), StoreError> {
    let chain = codec::wire(row, CHAIN, ChainId::parse)?;
    let endpoint = EndpointOverride {
        url: row.get("url")?,
        display: row.get("display")?,
        is_enabled: row.get("enabled")?,
        is_pinned: row.get("pinned")?,
    };
    Ok((chain, endpoint))
}

fn map_health(row: &Row<'_>) -> Result<EndpointHealthRecord, StoreError> {
    Ok(EndpointHealthRecord {
        chain: codec::wire(row, HEALTH_CHAIN, ChainId::parse)?,
        url: row.get("url")?,
        health: EndpointHealth {
            score_permille: codec::int(row, SCORE_PERMILLE)?,
            successes: codec::int(row, SUCCESSES)?,
            failures: codec::int(row, FAILURES)?,
            consecutive_failures: codec::int(row, CONSECUTIVE_FAILURES)?,
            latency_ms: codec::optional_int(row, LATENCY_MS)?,
            last_success_unix_secs: row.get("last_success")?,
            last_failure_unix_secs: row.get("last_failure")?,
            last_error: row.get("last_error")?,
        },
    })
}

impl Reader<'_> {
    pub(crate) fn list_endpoints(&self) -> Result<Vec<(ChainId, EndpointOverride)>, StoreError> {
        sql::all(self.connection(), LIST, [], map)
    }

    pub(crate) fn list_endpoint_health(&self) -> Result<Vec<EndpointHealthRecord>, StoreError> {
        sql::all(self.connection(), LIST_HEALTH, [], map_health)
    }
}

impl Writer<'_> {
    pub(crate) fn add_endpoint(
        &self,
        chain: ChainId,
        url: &str,
        display: &str,
    ) -> Result<(), StoreError> {
        sql::run(self.connection(), ADD, params![chain.as_str(), url, display])?;
        Ok(())
    }

    pub(crate) fn set_endpoint_enabled(
        &self,
        chain: ChainId,
        url: &str,
        is_enabled: bool,
    ) -> Result<(), StoreError> {
        sql::run(self.connection(), SET_ENABLED, params![chain.as_str(), url, is_enabled])?;
        Ok(())
    }

    pub(crate) fn set_endpoint_pinned(
        &self,
        chain: ChainId,
        url: &str,
        is_pinned: bool,
    ) -> Result<(), StoreError> {
        sql::run(self.connection(), SET_PINNED, params![chain.as_str(), url, is_pinned])?;
        Ok(())
    }

    pub(crate) fn delete_endpoint(&self, chain: ChainId, url: &str) -> Result<(), StoreError> {
        let removed = sql::run(self.connection(), DELETE, params![chain.as_str(), url])?;
        if removed == 0 {
            return Err(StoreError::NotFound { entity: ENDPOINT, id: redacted(url) });
        }
        Ok(())
    }

    pub fn put_endpoint_health(&self, record: &EndpointHealthRecord) -> Result<(), StoreError> {
        let EndpointHealthRecord { chain, url, health } = record;
        let EndpointHealth {
            score_permille,
            successes,
            failures,
            consecutive_failures,
            latency_ms,
            last_success_unix_secs,
            last_failure_unix_secs,
            last_error,
        } = health;
        sql::run(
            self.connection(),
            UPSERT_HEALTH,
            params![
                chain.as_str(),
                url,
                i64::from(*score_permille),
                codec::u64_to_db(*successes, SUCCESSES)?,
                codec::u64_to_db(*failures, FAILURES)?,
                i64::from(*consecutive_failures),
                codec::optional_u64_to_db(*latency_ms, LATENCY_MS)?,
                last_success_unix_secs,
                last_failure_unix_secs,
                last_error,
            ],
        )
        .constrained(HEALTH, UNIQUE_COLUMNS)?;
        Ok(())
    }
}

/// A user-added endpoint can carry a credential in userinfo, the path, or the query, so
/// an error names only the [`EndpointLabel`] form.
///
/// The store itself keys on the exact string it's given, and `jelly_net` owns the
/// canonical form.
fn redacted(url: &str) -> String {
    EndpointLabel::from_url(url).to_string()
}
