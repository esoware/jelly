use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Truncates sub-millisecond precision and saturates durations beyond `u64` milliseconds.
pub fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Injectable wall and monotonic clocks. A test can move the wall clock without moving the
/// monotonic scale.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Wall-clock seconds since the Unix epoch.
    fn now_secs(&self) -> Result<i64, ClockError> {
        i64::try_from(Duration::from_millis(self.now_millis()?).as_secs())
            .map_err(|_| ClockError::OutOfRange)
    }

    /// Wall-clock milliseconds since the Unix epoch.
    fn now_millis(&self) -> Result<u64, ClockError>;

    /// Milliseconds since a fixed origin chosen by the implementation. Never
    /// goes backwards, so deadlines survive a wall-clock adjustment.
    fn elapsed_millis(&self) -> u64;

    /// A deadline `budget` from now on this clock's monotonic scale.
    fn deadline_in(&self, budget: Duration) -> Deadline {
        let millis = duration_millis(budget);
        Deadline { at_elapsed_millis: self.elapsed_millis().saturating_add(millis) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ClockError {
    #[error("the system clock is before the Unix epoch")]
    BeforeEpoch,
    #[error("the system clock is outside the representable range")]
    OutOfRange,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(clippy::disallowed_methods, reason = "the only wall clock")]
    fn now_millis(&self) -> Result<u64, ClockError> {
        let since_epoch =
            SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| ClockError::BeforeEpoch)?;
        u64::try_from(since_epoch.as_millis()).map_err(|_| ClockError::OutOfRange)
    }

    #[expect(clippy::disallowed_methods, reason = "the only monotonic clock")]
    fn elapsed_millis(&self) -> u64 {
        // One process-wide origin, so every SystemClock agrees on the scale.
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let origin = ORIGIN.get_or_init(Instant::now);
        duration_millis(origin.elapsed())
    }
}

/// An absolute deadline on a `Clock`'s monotonic scale. Every endpoint attempt shares one,
/// so retries can't multiply the request budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Deadline {
    at_elapsed_millis: u64,
}

impl Deadline {
    #[cfg(feature = "test-util")]
    pub const NEVER: Self = Self { at_elapsed_millis: u64::MAX };

    pub const EXPIRED: Self = Self { at_elapsed_millis: 0 };

    /// How long a pass may overrun its deadline before it's abandoned. A pass checks the
    /// deadline only between requests, so this covers the request in flight and the writes
    /// that save its progress. It's shorter than the engine's watchdog grace, so a pass cut
    /// off here still reports its own failure.
    pub const GRACE: Duration = Duration::from_secs(15);

    pub fn remaining(self, clock: &dyn Clock) -> Duration {
        Duration::from_millis(self.at_elapsed_millis.saturating_sub(clock.elapsed_millis()))
    }

    /// The time from now until a pass under this deadline is abandoned, [`Self::GRACE`]
    /// past the deadline itself.
    pub fn cutoff(self, clock: &dyn Clock) -> Duration {
        self.remaining(clock).saturating_add(Self::GRACE)
    }

    pub fn is_expired(self, clock: &dyn Clock) -> bool {
        clock.elapsed_millis() >= self.at_elapsed_millis
    }
}
