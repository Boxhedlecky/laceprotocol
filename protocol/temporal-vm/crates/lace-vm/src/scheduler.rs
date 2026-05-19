//! The recurring-event scheduler.
//!
//! `RECURRING` does not run a loop inside the executor; that would
//! make the gas cost of a contract depend on wall-clock time, which
//! is a denial-of-service surface. Instead the opcode emits a
//! [`Schedule`] descriptor to the executor's outbox, and a separate
//! scheduler -- block-driven -- replays the contract's body at each
//! tick.
//!
//! This split is important enough that it gets its own module: it is
//! the structural reason recurring contracts (salary, subscription)
//! are safe under arbitrarily long pauses, since the scheduler
//! catches up on missed ticks deterministically without rerunning
//! the producing program.

use lace_time::{Duration, Interval, Timestamp};
use serde::{Deserialize, Serialize};

use crate::value::VmError;

/// A descriptor emitted by `RECURRING`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Interval between ticks.
    pub interval: Duration,
    /// Active window. The first tick fires at `window.start`, the
    /// last tick fires at the largest `start + k*interval` strictly
    /// less than `window.end`.
    pub window: Interval,
}

impl Schedule {
    /// Validate the schedule. Used by `RECURRING` to reject zero
    /// intervals and reversed windows before they reach the scheduler
    /// queue.
    pub fn validate(&self) -> Result<(), VmError> {
        if self.interval.is_zero() {
            return Err(VmError::InvalidSchedule("interval must be > 0"));
        }
        if self.window.is_empty() {
            return Err(VmError::InvalidSchedule("window is empty"));
        }
        Ok(())
    }

    /// Compute the number of ticks that should have fired strictly
    /// before `now`, given the schedule. Used by recurring-payment
    /// contracts to deterministically catch up on missed ticks.
    pub fn ticks_before(&self, now: Timestamp) -> u64 {
        if now <= self.window.start || self.interval.is_zero() {
            return 0;
        }
        let cap = if now >= self.window.end {
            self.window.end
        } else {
            now
        };
        let elapsed = cap.as_secs() - self.window.start.as_secs();
        elapsed / self.interval.as_secs()
    }

    /// Timestamp of the `k`-th tick (0-indexed). Returns `None` if the
    /// tick would fall outside the active window.
    pub fn tick_at(&self, k: u64) -> Option<Timestamp> {
        if self.interval.is_zero() {
            return None;
        }
        let offset = self.interval.as_secs().checked_mul(k)?;
        let ts = self.window.start.saturating_add(Duration::from_secs(offset));
        if self.window.contains(ts) {
            Some(ts)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(start: u64, end: u64, interval: u64) -> Schedule {
        Schedule {
            interval: Duration::from_secs(interval),
            window: Interval::new(Timestamp::from_secs(start), Timestamp::from_secs(end)),
        }
    }

    #[test]
    fn validate_rejects_zero_interval() {
        let s = schedule(0, 100, 0);
        assert!(matches!(s.validate(), Err(VmError::InvalidSchedule(_))));
    }

    #[test]
    fn validate_rejects_empty_window() {
        let s = schedule(100, 100, 10);
        assert!(matches!(s.validate(), Err(VmError::InvalidSchedule(_))));
    }

    #[test]
    fn ticks_before_counts_completed_intervals() {
        let s = schedule(0, 1000, 100);
        assert_eq!(s.ticks_before(Timestamp::from_secs(0)), 0);
        assert_eq!(s.ticks_before(Timestamp::from_secs(50)), 0);
        assert_eq!(s.ticks_before(Timestamp::from_secs(100)), 1);
        assert_eq!(s.ticks_before(Timestamp::from_secs(550)), 5);
    }

    #[test]
    fn ticks_before_caps_at_window_end() {
        let s = schedule(0, 500, 100);
        assert_eq!(s.ticks_before(Timestamp::from_secs(10_000)), 5);
    }

    #[test]
    fn tick_at_returns_none_outside_window() {
        let s = schedule(0, 500, 100);
        assert_eq!(s.tick_at(0), Some(Timestamp::from_secs(0)));
        assert_eq!(s.tick_at(4), Some(Timestamp::from_secs(400)));
        assert_eq!(s.tick_at(5), None);
    }
}
