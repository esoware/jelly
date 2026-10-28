use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jelly_core::{Clock, duration_millis};
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::retry_after::RetryAfter;

/// Per-endpoint admission control, separate from health. Health decides which endpoint
/// is worth trying, and pacing decides whether it may be touched yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacerPolicy {
    /// How often the bucket earns one token.
    pub min_interval: Duration,
    /// How many attempts the bucket lets through back to back.
    pub burst: u32,
    /// Attempts allowed to be in flight against one endpoint at once.
    pub max_concurrent: usize,
    /// Shortest backoff after a failure with no `Retry-After`.
    pub backoff_floor: Duration,
    /// Longest backoff, whatever the failure count.
    pub backoff_ceiling: Duration,
}

impl PacerPolicy {
    /// What a public endpoint is paced at.
    pub(crate) const DEFAULT: Self = Self {
        min_interval: Duration::from_millis(200),
        burst: 4,
        max_concurrent: 4,
        backoff_floor: Duration::from_millis(500),
        backoff_ceiling: Duration::from_secs(60),
    };

    /// No admission control, for tests of the failover walk rather than of pacing.
    #[cfg(any(test, feature = "test-util"))]
    pub const UNTHROTTLED: Self = Self {
        min_interval: Duration::from_millis(1),
        burst: u32::MAX,
        max_concurrent: usize::MAX,
        backoff_floor: Duration::from_millis(1),
        backoff_ceiling: Duration::from_millis(1),
    };
}

#[derive(Debug)]
struct Lane {
    tokens: u32,
    last_refill_millis: u64,
    cooldown_until_millis: u64,
    consecutive_failures: u32,
    slots: Arc<Semaphore>,
}

impl Lane {
    fn new(policy: &PacerPolicy, now_millis: u64) -> Self {
        Self {
            tokens: policy.burst.max(1),
            last_refill_millis: now_millis,
            cooldown_until_millis: 0,
            consecutive_failures: 0,
            slots: Arc::new(Semaphore::new(policy.max_concurrent.clamp(1, Semaphore::MAX_PERMITS))),
        }
    }

    fn refill(&mut self, policy: &PacerPolicy, now_millis: u64) {
        let interval_millis = min_interval_millis(policy);
        let elapsed_millis = now_millis.saturating_sub(self.last_refill_millis);
        let earned = elapsed_millis.checked_div(interval_millis).unwrap_or(0);
        if earned == 0 {
            return;
        }
        let gained = u32::try_from(earned).unwrap_or(u32::MAX);
        self.tokens = self.tokens.saturating_add(gained).min(policy.burst.max(1));
        self.last_refill_millis =
            self.last_refill_millis.saturating_add(earned.saturating_mul(interval_millis));
    }

    /// The cooldown if there is one, otherwise the time until the bucket earns its next
    /// token.
    fn wait(&self, policy: &PacerPolicy, now_millis: u64) -> Duration {
        let cooldown_millis = self.cooldown_until_millis.saturating_sub(now_millis);
        if cooldown_millis > 0 {
            return Duration::from_millis(cooldown_millis);
        }
        if self.tokens > 0 {
            return Duration::ZERO;
        }
        let interval_millis = min_interval_millis(policy);
        let since_millis = now_millis.saturating_sub(self.last_refill_millis);
        let used_millis = since_millis.checked_rem(interval_millis).unwrap_or(0);
        Duration::from_millis(interval_millis.saturating_sub(used_millis))
    }
}

/// Each endpoint URL gets one token bucket, one cooldown, and one concurrency cap.
///
/// Lanes are keyed by URL across chains, because a provider's rate limit doesn't know
/// which chain asked.
///
/// Nothing here waits. [`Pacer::try_acquire`] admits the attempt or says how long it
/// would wait, and the pool moves on to another endpoint instead.
#[derive(Debug)]
pub(crate) struct Pacer {
    policy: PacerPolicy,
    clock: Arc<dyn Clock>,
    lanes: Mutex<HashMap<String, Lane>>,
    jitter_state: AtomicU64,
}

impl Pacer {
    pub(crate) fn new(policy: PacerPolicy, clock: Arc<dyn Clock>) -> Self {
        let seed = clock.now_millis().unwrap_or(1).max(1);
        Self {
            policy,
            clock,
            lanes: Mutex::new(HashMap::new()),
            jitter_state: AtomicU64::new(seed),
        }
    }

    /// How long before `url` may be tried again. Zero means now.
    #[cfg(test)]
    pub(crate) fn ready_in(&self, url: &str) -> Duration {
        let now_millis = self.clock.elapsed_millis();
        let mut lanes = self.lanes.lock();
        Self::ready_lane(&mut lanes, url, &self.policy, now_millis).wait(&self.policy, now_millis)
    }

    /// Takes one concurrency slot and one token, or reports how long the caller would
    /// wait for one. The slot is held until the permit is dropped, which the pool does
    /// after one attempt.
    pub(crate) fn try_acquire(&self, url: &str) -> Result<OwnedSemaphorePermit, Duration> {
        let now_millis = self.clock.elapsed_millis();
        let mut lanes = self.lanes.lock();
        let lane = Self::ready_lane(&mut lanes, url, &self.policy, now_millis);
        let wait = lane.wait(&self.policy, now_millis);
        if wait > Duration::ZERO {
            return Err(wait);
        }
        // Non-blocking, so the lane lock may be held across it.
        let slot =
            Arc::clone(&lane.slots).try_acquire_owned().map_err(|_| self.policy.min_interval)?;
        lane.tokens = lane.tokens.saturating_sub(1);
        Ok(slot)
    }

    /// Clears the backoff after an endpoint answers.
    pub(crate) fn record_success(&self, url: &str) {
        let now_millis = self.clock.elapsed_millis();
        let mut lanes = self.lanes.lock();
        let lane = Self::lane(&mut lanes, url, &self.policy, now_millis);
        lane.consecutive_failures = 0;
        lane.cooldown_until_millis = 0;
    }

    /// Applies the server's own cooldown when it sent one, otherwise a
    /// full-jitter exponential backoff from the configured floor.
    pub(crate) fn record_failure(&self, url: &str, retry_after: Option<RetryAfter>) {
        let now_unix_secs = self.clock.now_secs().unwrap_or_default();
        let now_millis = self.clock.elapsed_millis();
        let jitter = self.next_jitter();
        let mut lanes = self.lanes.lock();
        let lane = Self::lane(&mut lanes, url, &self.policy, now_millis);
        lane.consecutive_failures = lane.consecutive_failures.saturating_add(1);
        let wait = retry_after.map_or_else(
            || backoff(&self.policy, lane.consecutive_failures, jitter),
            |value| value.resolve(now_unix_secs),
        );
        let wait_millis = duration_millis(wait);
        lane.cooldown_until_millis = now_millis.saturating_add(wait_millis);
    }

    /// Drops every endpoint's state, for a proxy change.
    pub(crate) fn reset(&self) {
        self.lanes.lock().clear();
    }

    /// Steps an xorshift64 generator, a cheap non-cryptographic spread for backoff only.
    ///
    /// The step is one atomic update, so two failures at once draw different values.
    fn next_jitter(&self) -> u64 {
        let step = |mut value: u64| {
            value ^= value.wrapping_shl(13);
            value ^= value.wrapping_shr(7);
            value ^= value.wrapping_shl(17);
            value.max(1)
        };
        step(self.jitter_state.update(Ordering::Relaxed, Ordering::Relaxed, step))
    }

    fn lane<'a>(
        lanes: &'a mut HashMap<String, Lane>,
        url: &str,
        policy: &PacerPolicy,
        now_millis: u64,
    ) -> &'a mut Lane {
        lanes.entry(url.to_owned()).or_insert_with(|| Lane::new(policy, now_millis))
    }

    /// The lane for `url`, brought up to date. Both readers go through this, so what
    /// `ready_in` reports and what `try_acquire` decides can't disagree.
    fn ready_lane<'a>(
        lanes: &'a mut HashMap<String, Lane>,
        url: &str,
        policy: &PacerPolicy,
        now_millis: u64,
    ) -> &'a mut Lane {
        let lane = Self::lane(lanes, url, policy, now_millis);
        lane.refill(policy, now_millis);
        lane
    }
}

fn min_interval_millis(policy: &PacerPolicy) -> u64 {
    duration_millis(policy.min_interval).max(1)
}

/// A jittered pick from `[floor, floor * 2^(failures - 1)]`, capped at the
/// ceiling, so a run of failures spreads out instead of retrying in lockstep.
fn backoff(policy: &PacerPolicy, failures: u32, jitter: u64) -> Duration {
    let floor_millis = duration_millis(policy.backoff_floor).max(1);
    let ceiling_millis = duration_millis(policy.backoff_ceiling).max(floor_millis);
    let shift = failures.saturating_sub(1).min(16);
    let step = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let window_millis = floor_millis.saturating_mul(step).min(ceiling_millis);
    let picked_millis = jitter.checked_rem(window_millis.max(1)).unwrap_or(0);
    Duration::from_millis(picked_millis.max(floor_millis.min(window_millis)))
}

#[cfg(test)]
mod tests {
    use jelly_core::test_util::FakeClock;

    use super::*;

    const URL: &str = "https://a.example";

    const fn policy() -> PacerPolicy {
        PacerPolicy { burst: 2, max_concurrent: 1, ..PacerPolicy::DEFAULT }
    }

    fn pacer(clock: &Arc<FakeClock>) -> Pacer {
        Pacer::new(policy(), Arc::<FakeClock>::clone(clock))
    }

    #[test]
    fn the_bucket_lets_a_burst_through_then_refills_on_the_interval() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        drop(pacer.try_acquire(URL).expect("token one"));
        drop(pacer.try_acquire(URL).expect("token two"));

        let wait = pacer.try_acquire(URL).expect_err("the burst is spent");
        assert_eq!(wait, Duration::from_millis(200));
        assert_eq!(pacer.ready_in(URL), Duration::from_millis(200));

        clock.advance(Duration::from_millis(200));
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
        drop(pacer.try_acquire(URL).expect("the bucket refilled"));
    }

    #[test]
    fn the_concurrency_cap_holds_one_attempt_at_a_time() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        let held = pacer.try_acquire(URL).expect("the only slot");
        assert!(pacer.try_acquire(URL).is_err(), "the second attempt has no slot");
        drop(held);
        drop(pacer.try_acquire(URL).expect("the slot came back"));
    }

    #[test]
    fn an_unthrottled_pacer_never_refuses() {
        let clock = Arc::new(FakeClock::default());
        let as_clock: Arc<dyn Clock> = clock;
        let pacer = Pacer::new(PacerPolicy::UNTHROTTLED, as_clock);

        for _ in 0..64u8 {
            drop(pacer.try_acquire(URL).expect("no admission control"));
        }
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
    }

    #[test]
    fn retry_after_in_seconds_sets_a_hard_cooldown() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        pacer.record_failure(URL, RetryAfter::parse("120"));
        assert_eq!(pacer.ready_in(URL), Duration::from_secs(120));

        clock.advance(Duration::from_secs(119));
        assert_eq!(pacer.ready_in(URL), Duration::from_secs(1));
        clock.advance(Duration::from_secs(1));
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
    }

    #[test]
    fn retry_after_as_a_date_resolves_against_the_clock() {
        let clock = Arc::new(FakeClock::new(FakeClock::DEFAULT_UNIX_MILLIS));
        let pacer = pacer(&clock);
        // The fake clock starts at 2026-01-01T00:00:00Z, so this asks for one minute later.
        pacer.record_failure(URL, Some(RetryAfter::AtUnixSecs(1_767_225_660)));
        assert_eq!(pacer.ready_in(URL), Duration::from_secs(60));
    }

    #[test]
    fn a_date_already_past_asks_for_no_wait() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        pacer.record_failure(URL, Some(RetryAfter::AtUnixSecs(784_111_777)));
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
    }

    #[test]
    fn a_failure_with_no_header_backs_off_within_the_configured_band() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        pacer.record_failure(URL, None);
        let first = pacer.ready_in(URL);
        assert!(first >= policy().backoff_floor, "at least the floor, got {first:?}");
        assert!(first <= policy().backoff_ceiling, "at most the ceiling, got {first:?}");

        clock.advance(policy().backoff_ceiling);
        pacer.record_success(URL);
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
    }

    #[test]
    fn a_run_of_failures_stays_inside_the_ceiling() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        for _ in 0..24u8 {
            pacer.record_failure(URL, None);
            let wait = pacer.ready_in(URL);
            assert!(wait <= policy().backoff_ceiling, "{wait:?}");
        }
    }

    #[test]
    fn a_reset_forgets_every_endpoint() {
        let clock = Arc::new(FakeClock::default());
        let pacer = pacer(&clock);

        pacer.record_failure(URL, RetryAfter::parse("600"));
        assert_eq!(pacer.ready_in(URL), Duration::from_secs(600));
        pacer.try_acquire(URL).expect_err("the endpoint is cooling down");

        pacer.reset();
        assert_eq!(pacer.ready_in(URL), Duration::ZERO);
        drop(pacer.try_acquire(URL).expect("the cooldown is gone"));
    }
}
