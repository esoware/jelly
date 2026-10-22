use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, Clock, EndpointHealth, duration_millis};
use parking_lot::Mutex;

use crate::endpoint::Endpoint;
use crate::error::NetError;

const TARGET: &str = "jelly::net::health";

/// How an endpoint is behaving. The declaration order is preference order, so `Ord`
/// ranks endpoints directly and a never-tried endpoint cannot outrank a proven one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthClass {
    Healthy,
    /// No samples yet.
    Unknown,
    Degraded,
    Quarantined,
}

const FULL_PERMILLE: i64 = 1000;

const ALPHA_PERMILLE: u16 = 300;
const HEALTHY_MIN_PERMILLE: u16 = 600;
const QUARANTINE_AFTER_FAILURES: u32 = 3;
const QUARANTINE_FOR: Duration = Duration::from_secs(20);
const DECAY_HALF_LIFE_SECS: i64 = 3600;
const MAX_HALVINGS: u32 = 16;

/// How many outcomes `health` is built from. Zero means never tried, which is what
/// separates [`HealthClass::Unknown`] from a real reading.
const fn samples(health: &EndpointHealth) -> u64 {
    health.successes.saturating_add(health.failures)
}

/// Coarse on purpose, so ordering does not flap because one call was 30 ms slower than
/// the last.
const fn latency_tier(health: &EndpointHealth) -> u8 {
    match health.latency_ms {
        None => 1,
        Some(millis) if millis < 250 => 0,
        Some(millis) if millis < 750 => 1,
        Some(millis) if millis < 2000 => 2,
        Some(_) => 3,
    }
}

/// The most recent moment the endpoint was heard from, either way.
const fn last_seen_unix_secs(health: &EndpointHealth) -> Option<i64> {
    match (health.last_success_unix_secs, health.last_failure_unix_secs) {
        (Some(success), Some(failure)) if failure > success => Some(failure),
        (Some(success), _) => Some(success),
        (None, failure) => failure,
    }
}

/// Pulls a stored score back towards the untried baseline, one halving per half life.
fn decayed(mut health: EndpointHealth, age_secs: i64) -> EndpointHealth {
    if age_secs <= 0 {
        return health;
    }
    let halvings = u32::try_from(age_secs.checked_div(DECAY_HALF_LIFE_SECS).unwrap_or(0))
        .unwrap_or(u32::MAX)
        .min(MAX_HALVINGS);
    let baseline = i64::from(EndpointHealth::BASELINE_PERMILLE);
    let distance = i64::from(health.score_permille).saturating_sub(baseline);
    let distance =
        1i64.checked_shl(halvings).and_then(|divisor| distance.checked_div(divisor)).unwrap_or(0);
    health.score_permille = clamp_permille(baseline.saturating_add(distance));
    health
}

/// One endpoint's health as a reader sees it, the stored record plus the facts that
/// depend on the current time and on this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthEntry {
    /// The canonical URL this record is keyed on.
    pub url: String,
    pub class: HealthClass,
    /// Whether an outcome reached this endpoint in this run. Until one does, a restored
    /// record's score is aged to this run while its timestamps still say when it was last
    /// heard from, so writing it back would have the next restore age it a second time.
    pub is_touched: bool,
    pub health: EndpointHealth,
}

#[derive(Debug, Default)]
struct Record {
    health: EndpointHealth,
    /// A point on the clock's monotonic scale, so it is meaningful only within
    /// one run and is never written out.
    quarantine_until_millis: Option<u64>,
    /// Set by an outcome and never by a restore.
    is_touched: bool,
}

/// The one place endpoint outcomes are recorded. The failover walk ranks on it, and the
/// shell persists and restores it across runs.
#[derive(Debug)]
pub struct HealthTracker {
    clock: Arc<dyn Clock>,
    records: Mutex<HashMap<ChainId, HashMap<String, Record>>>,
}

impl HealthTracker {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock, records: Mutex::new(HashMap::new()) }
    }

    pub fn record_success(&self, endpoint: &Endpoint, latency: Duration) {
        let now_unix_secs = self.clock.now_secs().unwrap_or_default();
        let observed_millis = duration_millis(latency);
        self.update(endpoint, |record| {
            let score = i64::from(record.health.score_permille);
            record.health.score_permille = clamp_permille(ewma(score, FULL_PERMILLE));
            record.health.successes = record.health.successes.saturating_add(1);
            record.health.consecutive_failures = 0;
            record.health.last_success_unix_secs = Some(now_unix_secs);
            record.health.last_error = None;
            record.health.latency_ms = Some(match record.health.latency_ms {
                None => observed_millis,
                Some(previous_millis) => {
                    let previous = i64::try_from(previous_millis).unwrap_or(i64::MAX);
                    let observed = i64::try_from(observed_millis).unwrap_or(i64::MAX);
                    u64::try_from(ewma(previous, observed).max(0)).unwrap_or(0)
                }
            });
            record.quarantine_until_millis = None;
        });
    }

    pub fn record_failure(&self, endpoint: &Endpoint, error: &NetError) {
        let now_unix_secs = self.clock.now_secs().unwrap_or_default();
        let now_millis = self.clock.elapsed_millis();
        let message = error.to_string();
        tracing::debug!(
            target: TARGET,
            chain = endpoint.chain().as_str(),
            endpoint = %endpoint.label(),
            kind = error.kind().as_str(),
            "endpoint failure"
        );
        self.update(endpoint, |record| {
            let score = i64::from(record.health.score_permille);
            record.health.score_permille = clamp_permille(ewma(score, 0));
            record.health.failures = record.health.failures.saturating_add(1);
            record.health.consecutive_failures =
                record.health.consecutive_failures.saturating_add(1);
            record.health.last_failure_unix_secs = Some(now_unix_secs);
            record.health.last_error = Some(message);
            if record.health.consecutive_failures >= QUARANTINE_AFTER_FAILURES {
                let window_millis = duration_millis(QUARANTINE_FOR);
                record.quarantine_until_millis = Some(now_millis.saturating_add(window_millis));
            }
        });
    }

    /// Applies one outcome and logs the endpoint's class if it moved. Both outcomes go
    /// through here, so the class-change log line lives in one place.
    fn update(&self, endpoint: &Endpoint, change: impl FnOnce(&mut Record)) {
        let (before, after) = {
            let mut records = self.records.lock();
            let record = records
                .entry(endpoint.chain())
                .or_default()
                .entry(endpoint.url().to_owned())
                .or_default();
            let before = self.class_of(record);
            change(record);
            record.is_touched = true;
            (before, self.class_of(record))
        };
        if before != after {
            tracing::debug!(
                target: TARGET,
                chain = endpoint.chain().as_str(),
                endpoint = %endpoint.label(),
                class = ?after,
                "endpoint health changed"
            );
        }
    }

    /// Everything known about one endpoint right now.
    pub fn entry(&self, chain: ChainId, url: &str) -> HealthEntry {
        let records = self.records.lock();
        self.entry_of(url, records.get(&chain).and_then(|urls| urls.get(url)))
    }

    /// The ordering key the failover walk sorts on, class first and then latency tier.
    pub(crate) fn rank(&self, chain: ChainId, url: &str) -> (HealthClass, u8) {
        let records = self.records.lock();
        records.get(&chain).and_then(|urls| urls.get(url)).map_or_else(
            || (HealthClass::Unknown, latency_tier(&EndpointHealth::default())),
            |record| (self.class_of(record), latency_tier(&record.health)),
        )
    }

    /// Everything known about one chain, ordered by canonical URL.
    pub fn snapshot(&self, chain: ChainId) -> Vec<HealthEntry> {
        let mut rows: Vec<HealthEntry> = self
            .records
            .lock()
            .get(&chain)
            .into_iter()
            .flatten()
            .map(|(url, record)| self.entry_of(url, Some(record)))
            .collect();
        rows.sort_by(|a, b| a.url.cmp(&b.url));
        rows
    }

    /// Seeds a record read back from the database, aged by how long ago the endpoint was
    /// last heard from. A quarantine is never restored, because it is a judgment about
    /// one run.
    pub fn restore(&self, chain: ChainId, url: impl Into<String>, health: EndpointHealth) {
        let now_unix_secs = self.clock.now_secs().unwrap_or_default();
        let age_secs =
            last_seen_unix_secs(&health).map_or(0, |seen| now_unix_secs.saturating_sub(seen));
        let health = decayed(health, age_secs);
        self.records.lock().entry(chain).or_default().insert(
            url.into(),
            Record { health, quarantine_until_millis: None, is_touched: false },
        );
    }

    fn entry_of(&self, url: &str, record: Option<&Record>) -> HealthEntry {
        HealthEntry {
            url: url.to_owned(),
            class: record.map_or(HealthClass::Unknown, |record| self.class_of(record)),
            is_touched: record.is_some_and(|record| record.is_touched),
            health: record.map(|record| record.health.clone()).unwrap_or_default(),
        }
    }

    fn class_of(&self, record: &Record) -> HealthClass {
        if record.quarantine_until_millis.is_some_and(|until| self.clock.elapsed_millis() < until) {
            return HealthClass::Quarantined;
        }
        if samples(&record.health) == 0 {
            return HealthClass::Unknown;
        }
        if record.health.score_permille >= HEALTHY_MIN_PERMILLE {
            HealthClass::Healthy
        } else {
            HealthClass::Degraded
        }
    }
}

fn ewma(current: i64, target: i64) -> i64 {
    let step = target
        .saturating_sub(current)
        .saturating_mul(i64::from(ALPHA_PERMILLE))
        .checked_div(FULL_PERMILLE)
        .unwrap_or(0);
    current.saturating_add(step)
}

fn clamp_permille(value: i64) -> u16 {
    u16::try_from(value.clamp(0, FULL_PERMILLE)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use jelly_core::test_util::FakeClock;

    use super::*;

    #[test]
    fn latency_tiers_are_coarse_enough_not_to_flap() {
        let tier = |ms: u64| {
            latency_tier(&EndpointHealth { latency_ms: Some(ms), ..EndpointHealth::default() })
        };
        assert_eq!(tier(10), tier(240));
        assert_eq!(tier(300), tier(700));
        assert!(tier(200) < tier(300));
        assert!(tier(1000) < tier(5000));
        assert_eq!(latency_tier(&EndpointHealth::default()), 1);
    }

    #[test]
    fn an_untried_endpoint_ranks_as_unknown_in_the_middle_latency_tier() {
        let tracker = HealthTracker::new(Arc::new(FakeClock::default()));
        assert_eq!(tracker.rank(ChainId::Ethereum, "https://a.example"), (HealthClass::Unknown, 1));
    }
}
