use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, Clock as _, EndpointHealth, test_util::FakeClock};
use jelly_net::{Endpoint, HealthClass, HealthEntry, HealthTracker, NetError};

use crate::common::{ENDPOINT_A, ENDPOINT_B, endpoint};

fn failure(endpoint: &Endpoint) -> NetError {
    NetError::timed_out(endpoint.label(), Duration::from_secs(15))
}

struct Fixture {
    clock: Arc<FakeClock>,
    tracker: HealthTracker,
    endpoint: Endpoint,
}

impl Fixture {
    fn entry(&self) -> HealthEntry {
        self.tracker.entry(ChainId::Ethereum, ENDPOINT_A)
    }

    fn fail(&self) {
        self.tracker.record_failure(&self.endpoint, &failure(&self.endpoint));
    }

    fn answer(&self, latency: Duration) {
        self.tracker.record_success(&self.endpoint, latency);
    }
}

fn fixture() -> Result<Fixture, NetError> {
    let clock = Arc::new(FakeClock::default());
    let tracker = HealthTracker::new(Arc::<FakeClock>::clone(&clock));
    Ok(Fixture { clock, tracker, endpoint: endpoint(ENDPOINT_A)? })
}

#[test]
fn declaration_order_is_preference_order_among_health_classes() {
    assert!(HealthClass::Healthy < HealthClass::Unknown);
    assert!(HealthClass::Unknown < HealthClass::Degraded);
    assert!(HealthClass::Degraded < HealthClass::Quarantined);
}

#[test]
fn an_untried_endpoint_is_unknown() {
    let fixture = fixture().expect("a health fixture");
    assert_eq!(fixture.entry().class, HealthClass::Unknown);
}

#[test]
fn the_moving_average_follows_a_fixed_outcome_sequence() {
    let fixture = fixture().expect("a health fixture");

    fixture.fail();
    assert_eq!(fixture.entry().health.score_permille, 525);
    fixture.fail();
    assert_eq!(fixture.entry().health.score_permille, 368);
    assert_eq!(fixture.entry().class, HealthClass::Degraded);

    fixture.answer(Duration::from_millis(100));
    let health = fixture.entry().health;
    assert_eq!(health.score_permille, 557);
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.latency_ms, Some(100));

    fixture.answer(Duration::from_millis(100));
    assert_eq!(fixture.entry().health.score_permille, 689);
    assert_eq!(fixture.entry().class, HealthClass::Healthy);
}

#[test]
fn three_consecutive_failures_quarantine_until_the_window_passes() {
    let fixture = fixture().expect("a health fixture");

    for _ in 0..3u8 {
        fixture.fail();
    }
    assert_eq!(fixture.entry().class, HealthClass::Quarantined);

    fixture.clock.advance(Duration::from_millis(19_999));
    assert_eq!(fixture.entry().class, HealthClass::Quarantined);
    fixture.clock.advance(Duration::from_millis(1));
    assert_eq!(fixture.entry().class, HealthClass::Degraded);

    fixture.answer(Duration::from_millis(10));
    let entry = fixture.entry();
    assert_eq!(entry.health.consecutive_failures, 0);
    assert_ne!(entry.class, HealthClass::Quarantined);
}

#[test]
fn success_breaks_the_consecutive_failure_streak() {
    let fixture = fixture().expect("a health fixture");
    for _ in 0..2 {
        fixture.fail();
        assert_ne!(fixture.entry().class, HealthClass::Quarantined);
    }
    fixture.answer(Duration::from_millis(10));
    for _ in 0..2 {
        fixture.fail();
        assert_ne!(fixture.entry().class, HealthClass::Quarantined);
    }
    fixture.fail();
    assert_eq!(fixture.entry().class, HealthClass::Quarantined);
}

#[test]
fn a_stored_record_decays_toward_the_baseline_by_its_own_age() {
    let fixture = fixture().expect("a health fixture");
    let now = fixture.clock.now_secs().expect("a fake clock");
    let stored = |age: i64| EndpointHealth {
        score_permille: 100,
        successes: 1,
        failures: 9,
        last_failure_unix_secs: Some(now.saturating_sub(age)),
        ..EndpointHealth::default()
    };

    fixture.tracker.restore(ChainId::Ethereum, ENDPOINT_A, stored(0));
    assert_eq!(fixture.entry().health.score_permille, 100);

    fixture.tracker.restore(ChainId::Ethereum, ENDPOINT_A, stored(3600));
    assert_eq!(fixture.entry().health.score_permille, 425);

    fixture.tracker.restore(ChainId::Ethereum, ENDPOINT_A, stored(86_400));
    assert_eq!(fixture.entry().health.score_permille, 750);
}

#[test]
fn a_restored_record_is_never_quarantined() {
    let fixture = fixture().expect("a health fixture");
    for _ in 0..3u8 {
        fixture.fail();
    }
    assert_eq!(fixture.entry().class, HealthClass::Quarantined);

    fixture.tracker.restore(ChainId::Ethereum, ENDPOINT_A, fixture.entry().health);
    assert_ne!(fixture.entry().class, HealthClass::Quarantined);
}

#[test]
fn a_restored_record_is_touched_only_once_an_outcome_reaches_it() {
    let fixture = fixture().expect("a health fixture");
    let stored = EndpointHealth { successes: 4, ..EndpointHealth::default() };
    fixture.tracker.restore(ChainId::Ethereum, ENDPOINT_A, stored);
    assert!(!fixture.entry().is_touched);

    fixture.answer(Duration::from_millis(10));
    assert!(fixture.entry().is_touched);
}

#[test]
fn a_snapshot_reports_every_endpoint_of_one_chain() {
    let fixture = fixture().expect("a health fixture");
    let second = endpoint(ENDPOINT_B).expect("a canonical url");
    fixture.answer(Duration::from_millis(10));
    fixture.tracker.record_failure(&second, &failure(&second));

    let rows = fixture.tracker.snapshot(ChainId::Ethereum);
    let urls: Vec<&str> = rows.iter().map(|row| row.url.as_str()).collect();
    assert_eq!(urls, [ENDPOINT_A, ENDPOINT_B]);
    assert!(fixture.tracker.snapshot(ChainId::Bitcoin).is_empty());
}
