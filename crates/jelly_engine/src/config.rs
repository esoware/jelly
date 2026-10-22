use std::time::Duration;

use jelly_vault::Argon2Params;

/// Engine tunables. Every value has a shipped default that an application may override
/// at construction.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The budget one pass is handed as a [`jelly_core::Deadline`]. An adapter
    /// is expected to stop and report partial progress when it runs out.
    pub pass_budget: Duration,
    /// How much longer than its budget a pass may take before the engine stops waiting.
    /// It guards against an adapter that ignores its deadline and is not a second
    /// budget. It should exceed [`jelly_core::Deadline::GRACE`], so an adapter that cuts
    /// its own pass off reports that failure itself.
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
    /// Production keeps [`Argon2Params::RECOMMENDED`]. A test harness may drop
    /// to [`Argon2Params::MINIMUM`], which is the accepted floor rather than a
    /// weakened profile. The vault refuses anything below it.
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
    /// The wall clock a pass may occupy at most. Strictly longer than the budget
    /// the adapter was handed, so an adapter that honours its deadline reports its own
    /// failure rather than being cut off by the engine, provided `pass_grace` exceeds
    /// `Deadline::GRACE`.
    pub(crate) fn pass_watchdog(&self) -> Duration {
        self.pass_budget.saturating_add(self.pass_grace.max(Duration::from_millis(1)))
    }

    /// Never zero, because a bounded channel of zero cannot be built.
    pub(crate) const fn channel_capacity(&self) -> usize {
        if self.event_capacity == 0 { 1 } else { self.event_capacity }
    }

    /// How many write attempts to make, never zero. Named apart from the field
    /// it clamps, so reaching for the raw value cannot be a typo.
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
