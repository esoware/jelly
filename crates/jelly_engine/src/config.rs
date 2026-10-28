use std::time::Duration;

use jelly_vault::Argon2Params;

/// Engine tunables, with shipped defaults the app can override.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The budget one pass is handed as a [`jelly_core::Deadline`]. An adapter
    /// is expected to stop and report partial progress when it runs out.
    pub pass_budget: Duration,
    /// How much longer than its budget a pass may run before the engine stops waiting.
    /// It only guards against an adapter that ignores its deadline. It should exceed
    /// [`jelly_core::Deadline::GRACE`], so an adapter that cuts its own pass off reports
    /// that failure itself.
    pub pass_grace: Duration,
    /// Capacity of the bounded event sink and of the broadcast the interface
    /// subscribes to. Zero is treated as one.
    pub event_capacity: usize,
    /// Coalescing window of the event pump.
    pub event_flush: Duration,
    /// Total attempts at a write the database may refuse, either a window of sync results
    /// or the record of an already broadcast transaction. Zero is treated as one.
    pub write_attempts: u32,
    /// How long to wait between those attempts.
    pub retry_delay: Duration,
    /// Argon2id profile for vaults this engine creates.
    ///
    /// Production uses [`Argon2Params::RECOMMENDED`]. Tests may drop to
    /// [`Argon2Params::MINIMUM`], the lowest the vault accepts.
    pub vault_kdf: Argon2Params,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            pass_budget: Duration::from_secs(120),
            pass_grace: Duration::from_secs(30),
            event_capacity: 1024,
            event_flush: Duration::from_millis(100),
            write_attempts: 5,
            retry_delay: Duration::from_secs(2),
            vault_kdf: Argon2Params::RECOMMENDED,
        }
    }
}

impl EngineConfig {
    /// The most wall-clock time a pass may take. Always longer than the adapter's budget,
    /// so an adapter that honors its deadline reports its own failure before the engine
    /// cuts it off, as long as `pass_grace` exceeds `Deadline::GRACE`.
    pub(crate) fn pass_watchdog(&self) -> Duration {
        self.pass_budget.saturating_add(self.pass_grace.max(Duration::from_millis(1)))
    }

    /// Never zero, because a bounded channel of zero can't be built.
    pub(crate) const fn channel_capacity(&self) -> usize {
        if self.event_capacity == 0 { 1 } else { self.event_capacity }
    }

    /// How many write attempts to make, never zero. Named apart from the field it
    /// clamps, so reaching for the raw value can't be a typo.
    pub(crate) const fn attempts(&self) -> u32 {
        if self.write_attempts == 0 { 1 } else { self.write_attempts }
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::Deadline;

    use super::*;

    #[test]
    fn the_watchdog_always_outlasts_the_budget() {
        for grace in [Duration::ZERO, Duration::from_millis(1), Duration::from_secs(30)] {
            let config = EngineConfig { pass_grace: grace, ..EngineConfig::default() };
            assert!(config.pass_watchdog() > config.pass_budget, "grace {grace:?}");
        }
    }

    #[test]
    fn the_watchdog_waits_longer_than_an_adapter_waits_on_itself() {
        let config = EngineConfig::default();
        assert!(config.pass_grace > Deadline::GRACE);
    }

    #[test]
    fn zero_capacity_and_attempts_clamp_to_one() {
        let config = EngineConfig { event_capacity: 0, ..EngineConfig::default() };
        assert_eq!(config.channel_capacity(), 1);
        let config = EngineConfig { write_attempts: 0, ..EngineConfig::default() };
        assert_eq!(config.attempts(), 1);
    }
}
