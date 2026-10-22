use std::time::Duration;

use jelly_core::duration_millis;

use crate::poll::PassReport;

/// Cadence after any pass that is not a failure. Jitter is added on top.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The shortest wait after a pass that got nothing, and the base the backoff
/// window doubles from.
const BACKOFF_FLOOR: Duration = Duration::from_secs(30);

/// The longest the backoff draw becomes. A longer Retry-After still wins.
const BACKOFF_CEILING: Duration = Duration::from_mins(15);

/// Caps the doubling so the shift and the multiplication stay in range.
const MAX_SHIFT: u32 = 16;

/// A pass that is not a failure waits [`POLL_INTERVAL`] plus up to a quarter of
/// it in jitter.
///
/// A failed pass draws uniformly from
/// `[BACKOFF_FLOOR, min(BACKOFF_CEILING, BACKOFF_FLOOR << failures)]` so retrying
/// clients do not fall into step, and waits longer when the provider asked to.
#[derive(Debug, Clone)]
pub(crate) struct Schedule {
    /// Consecutive passes that got nothing.
    failures: u32,
    rng_state: u64,
}

impl Schedule {
    pub(crate) const fn new(seed: u64) -> Self {
        Self { failures: 0, rng_state: seed }
    }

    pub(crate) fn next_delay(&mut self, report: PassReport) -> Duration {
        if report.is_failure() {
            self.failures = self.failures.saturating_add(1);
            let span = self.backoff_span();
            let jittered = BACKOFF_FLOOR.saturating_add(self.uniform(span));
            return report.retry_after.map_or(jittered, |asked| asked.max(jittered));
        }
        self.failures = 0;
        let span = POLL_INTERVAL.checked_div(4).unwrap_or(Duration::ZERO);
        POLL_INTERVAL.saturating_add(self.uniform(span))
    }

    /// How far above the floor a backoff may be drawn, so that the longest wait
    /// is the ceiling exactly and the shortest is the floor exactly.
    fn backoff_span(&self) -> Duration {
        let shift = self.failures.min(MAX_SHIFT);
        let step = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let top_millis = duration_millis(BACKOFF_FLOOR).saturating_mul(step);
        Duration::from_millis(top_millis).min(BACKOFF_CEILING).saturating_sub(BACKOFF_FLOOR)
    }

    /// Uniform in `[0, span]`. This generator only schedules requests and never
    /// draws key material.
    fn uniform(&mut self, span: Duration) -> Duration {
        let bound_millis = duration_millis(span).saturating_add(1);
        Duration::from_millis(self.next_u64().checked_rem(bound_millis).unwrap_or(0))
    }

    /// `SplitMix64`.
    const fn next_u64(&mut self) -> u64 {
        self.rng_state = self.rng_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng_state;
        z = (z ^ z.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ z.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ z.wrapping_shr(31)
    }
}

#[cfg(test)]
mod tests {
    use std::iter::repeat_with;

    use super::*;
    use crate::poll::PassOutcome;

    fn answered() -> PassReport {
        PassReport { outcome: PassOutcome::Answered, quoted: 9, moved: 15, ..PassReport::default() }
    }

    fn nothing_answered(retry_after: Option<Duration>) -> PassReport {
        PassReport {
            outcome: PassOutcome::Failed,
            missing: 9,
            retry_after,
            ..PassReport::default()
        }
    }

    #[test]
    fn a_pass_that_answered_keeps_the_cadence() {
        let mut schedule = Schedule::new(7);
        for _ in 0..64u16 {
            let delay = schedule.next_delay(answered());
            assert!(delay >= POLL_INTERVAL, "{delay:?}");
            assert!(delay <= Duration::from_secs(75), "{delay:?}");
        }
        assert_eq!(schedule.failures, 0);
    }

    #[test]
    fn a_pass_that_got_nothing_backs_off_and_stops_at_the_ceiling() {
        let mut schedule = Schedule::new(11);
        let mut longest = Duration::ZERO;
        for round in 1..=20u32 {
            let delay = schedule.next_delay(nothing_answered(None));
            assert_eq!(schedule.failures, round);
            assert!(delay <= BACKOFF_CEILING, "{delay:?}");
            longest = longest.max(delay);
        }
        for _ in 0..64u16 {
            longest = longest.max(schedule.next_delay(nothing_answered(None)));
        }
        assert!(longest > Duration::from_secs(450), "the window never neared the ceiling");
    }

    #[test]
    fn no_backoff_is_shorter_than_the_floor() {
        let mut schedule = Schedule::new(19);
        for _ in 0..256u16 {
            let delay = schedule.next_delay(nothing_answered(None));
            assert!(delay >= BACKOFF_FLOOR, "{delay:?}");
            assert!(delay <= BACKOFF_CEILING, "{delay:?}");
        }
    }

    #[test]
    fn a_pass_whose_every_answer_was_unusable_backs_off() {
        let mut schedule = Schedule::new(23);
        let unusable =
            PassReport { outcome: PassOutcome::Answered, rejected: 9, ..PassReport::default() };
        assert!(unusable.is_failure());
        assert!(schedule.next_delay(unusable) >= BACKOFF_FLOOR);
        assert_eq!(schedule.failures, 1);
    }

    #[test]
    fn one_pass_that_answered_resets_the_backoff() {
        let mut schedule = Schedule::new(3);
        for _ in 0..8u8 {
            schedule.next_delay(nothing_answered(None));
        }
        assert_eq!(schedule.failures, 8);
        let delay = schedule.next_delay(answered());
        assert_eq!(schedule.failures, 0);
        assert!(delay >= POLL_INTERVAL, "{delay:?}");
    }

    #[test]
    fn the_longer_of_the_retry_after_and_the_backoff_wins() {
        let mut schedule = Schedule::new(5);
        let asked = Duration::from_mins(20);
        assert_eq!(schedule.next_delay(nothing_answered(Some(asked))), asked);

        let backoff = schedule.clone().next_delay(nothing_answered(None));
        let brief = Duration::from_secs(1);
        assert_eq!(schedule.next_delay(nothing_answered(Some(brief))), backoff);
    }

    #[test]
    fn a_pass_held_back_by_a_rest_keeps_the_cadence() {
        let mut schedule = Schedule::new(13);
        let resting =
            PassReport { outcome: PassOutcome::Resting, missing: 9, ..PassReport::default() };
        assert!(!resting.is_failure(), "waiting is not failing");
        assert!(schedule.next_delay(resting) >= POLL_INTERVAL);
        assert_eq!(schedule.failures, 0);
    }

    #[test]
    fn two_seeds_do_not_produce_the_same_walk() {
        let mut left = Schedule::new(1);
        let mut right = Schedule::new(2);
        let walk = |schedule: &mut Schedule| {
            repeat_with(|| schedule.next_delay(answered())).take(8).collect::<Vec<Duration>>()
        };
        assert_ne!(walk(&mut left), walk(&mut right));
    }

    #[test]
    fn the_jitter_actually_moves() {
        let mut schedule = Schedule::new(17);
        let walk: Vec<Duration> =
            repeat_with(|| schedule.next_delay(answered())).take(16).collect();
        let first = walk.first().copied().unwrap_or(Duration::ZERO);
        assert!(walk.iter().any(|delay| *delay != first), "every pass was scheduled identically");
    }
}
