//! Lace temporal VM -- time, duration, and interval primitives.
//!
//! Every other crate in the temporal VM workspace builds on the types
//! defined here. Time is a *first-class type* in the Lace VM, so these
//! primitives are not just convenience wrappers around `u64` -- they
//! enforce monotonicity, saturate on overflow, and round-trip through
//! the wire format used by the executor and scheduler.
//!
//! ## Canonical time source
//!
//! `Timestamp` is unsigned Unix seconds. Inside the VM, the value
//! returned by [`Clock::now`] is supplied by consensus -- specifically,
//! the median timestamp of the validators that signed the current
//! block, clamped to be strictly greater than the previous block's
//! timestamp. Validators therefore cannot move time backward, and an
//! individual malicious validator cannot skew time by more than the
//! median permits. The opcode layer treats this value as authoritative;
//! it does not consult wall clocks.
//!
//! Block height is exposed in parallel via [`BlockHeight`]; some
//! contracts prefer height-based deadlines (e.g. "release at block
//! N + 1000") because heights are strictly monotonic by construction.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

use core::fmt;
use serde::{Deserialize, Serialize};

/// Unix seconds since the epoch (1970-01-01 UTC).
///
/// 64 bits is enough to represent any time the protocol will ever
/// reach, with a margin of roughly 5*10^11 years.
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// The zero timestamp.
    pub const ZERO: Self = Self(0);
    /// The latest timestamp representable.
    pub const MAX: Self = Self(u64::MAX);

    /// Construct a timestamp from raw Unix seconds.
    #[inline]
    pub const fn from_secs(s: u64) -> Self {
        Self(s)
    }

    /// Return the underlying Unix seconds.
    #[inline]
    pub const fn as_secs(self) -> u64 {
        self.0
    }

    /// Add a duration, saturating at [`Timestamp::MAX`].
    ///
    /// Saturating arithmetic is deliberate. The contracts layer encodes
    /// "no upper bound" as `Timestamp::MAX`, and we want that to remain
    /// stable under further addition rather than silently wrapping.
    #[inline]
    pub const fn saturating_add(self, d: Duration) -> Self {
        Self(self.0.saturating_add(d.0))
    }

    /// Subtract a duration, saturating at [`Timestamp::ZERO`].
    #[inline]
    pub const fn saturating_sub(self, d: Duration) -> Self {
        Self(self.0.saturating_sub(d.0))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ts:{}", self.0)
    }
}

/// A signed difference between two timestamps, in seconds.
///
/// `TIMEDELTA` is one of the five native opcodes and must support both
/// "t1 before t2" and "t1 after t2", so the result type is signed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeDelta(pub i64);

impl TimeDelta {
    /// Compute `later - earlier`, saturating at `i64::MIN` / `i64::MAX`.
    pub const fn between(earlier: Timestamp, later: Timestamp) -> Self {
        let e = earlier.0;
        let l = later.0;
        if l >= e {
            let diff = l - e;
            if diff > i64::MAX as u64 {
                Self(i64::MAX)
            } else {
                Self(diff as i64)
            }
        } else {
            let diff = e - l;
            if diff > i64::MAX as u64 {
                Self(i64::MIN)
            } else {
                Self(-(diff as i64))
            }
        }
    }

    /// Whether this delta is positive (`later` was actually later).
    #[inline]
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }
}

/// An unsigned duration in seconds.
///
/// Durations are unsigned because every place the VM accepts a
/// duration (`RECURRING` interval, escrow abort window, dead-man
/// inactivity threshold) is conceptually nonnegative. Use
/// [`TimeDelta`] for signed comparisons between two timestamps.
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Duration(pub u64);

impl Duration {
    /// Zero duration.
    pub const ZERO: Self = Self(0);
    /// One second.
    pub const SECOND: Self = Self(1);
    /// One minute.
    pub const MINUTE: Self = Self(60);
    /// One hour.
    pub const HOUR: Self = Self(60 * 60);
    /// One day.
    pub const DAY: Self = Self(60 * 60 * 24);
    /// One (non-leap) year.
    pub const YEAR: Self = Self(60 * 60 * 24 * 365);

    /// Construct from raw seconds.
    #[inline]
    pub const fn from_secs(s: u64) -> Self {
        Self(s)
    }

    /// Raw seconds.
    #[inline]
    pub const fn as_secs(self) -> u64 {
        self.0
    }

    /// Whether the duration is zero. The scheduler treats a
    /// zero-interval recurring schedule as an error rather than as a
    /// degenerate hot-loop, which is the entire purpose of this
    /// predicate.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// Block height, exposed as a parallel monotonic time axis.
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct BlockHeight(pub u64);

/// A half-open `[start, end)` time window.
///
/// Used by `BEFORE` / `AFTER` composition and by recurring schedules
/// to encode the active window. `start == end` represents the empty
/// interval; the scheduler treats this as "never fires" rather than as
/// an error.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    /// Inclusive lower bound.
    pub start: Timestamp,
    /// Exclusive upper bound.
    pub end: Timestamp,
}

impl Interval {
    /// Construct an interval, normalising `end < start` to the empty
    /// interval `[start, start)`. We never construct a backward
    /// interval; callers that need to error on `end < start` should
    /// check that themselves and return an error rather than relying
    /// on this constructor to do it for them.
    pub const fn new(start: Timestamp, end: Timestamp) -> Self {
        if end.0 < start.0 {
            Self { start, end: start }
        } else {
            Self { start, end }
        }
    }

    /// Whether the given timestamp falls within the half-open window.
    #[inline]
    pub const fn contains(self, ts: Timestamp) -> bool {
        ts.0 >= self.start.0 && ts.0 < self.end.0
    }

    /// Whether the interval is empty (`start == end`).
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    /// Duration of the interval, saturating at `u64::MAX`.
    #[inline]
    pub const fn duration(self) -> Duration {
        Duration(self.end.0.saturating_sub(self.start.0))
    }
}

/// A clock the VM executor consults for "now".
///
/// In production this is wired to consensus state; in tests it is a
/// hand-rolled `ManualClock` that advances under the test's control.
/// Splitting this behind a trait is what makes deterministic testing
/// of time-dependent opcodes possible without re-introducing wall
/// clocks.
pub trait Clock {
    /// Current consensus time.
    fn now(&self) -> Timestamp;
    /// Current block height.
    fn height(&self) -> BlockHeight;
}

/// A clock that returns a fixed timestamp until mutated.
///
/// `ManualClock` is the workhorse for tests: it lets a test march time
/// forward, jump over a deadline, or replay the same instant twice
/// (which a real clock can never do).
#[derive(Clone, Debug)]
pub struct ManualClock {
    now: Timestamp,
    height: BlockHeight,
}

impl ManualClock {
    /// Construct a manual clock at the given timestamp and height 0.
    pub const fn at(now: Timestamp) -> Self {
        Self {
            now,
            height: BlockHeight(0),
        }
    }

    /// Advance the clock by `d` and bump the block height by 1.
    pub fn advance(&mut self, d: Duration) {
        self.now = self.now.saturating_add(d);
        self.height.0 = self.height.0.saturating_add(1);
    }

    /// Jump directly to a specified timestamp. Panics if `to` is
    /// strictly less than the current time: time inside the VM is
    /// strictly monotonic and we surface that constraint in tests by
    /// crashing loudly rather than letting a test silently rewind.
    pub fn set(&mut self, to: Timestamp) {
        assert!(to >= self.now, "ManualClock::set may not rewind time");
        self.now = to;
        self.height.0 = self.height.0.saturating_add(1);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        self.now
    }
    fn height(&self) -> BlockHeight {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_delta_signs_match_relative_order() {
        let early = Timestamp::from_secs(1_000);
        let late = Timestamp::from_secs(1_500);
        assert_eq!(TimeDelta::between(early, late).0, 500);
        assert_eq!(TimeDelta::between(late, early).0, -500);
        assert_eq!(TimeDelta::between(early, early).0, 0);
    }

    #[test]
    fn timestamp_saturating_add_clamps_at_max() {
        let near_max = Timestamp::from_secs(u64::MAX - 5);
        assert_eq!(near_max.saturating_add(Duration::from_secs(100)), Timestamp::MAX);
    }

    #[test]
    fn interval_normalises_backwards_range() {
        let i = Interval::new(Timestamp::from_secs(100), Timestamp::from_secs(50));
        assert!(i.is_empty());
        assert!(!i.contains(Timestamp::from_secs(75)));
    }

    #[test]
    fn interval_contains_is_half_open() {
        let i = Interval::new(Timestamp::from_secs(10), Timestamp::from_secs(20));
        assert!(i.contains(Timestamp::from_secs(10)));
        assert!(i.contains(Timestamp::from_secs(19)));
        assert!(!i.contains(Timestamp::from_secs(20)));
    }

    #[test]
    fn manual_clock_advances() {
        let mut clock = ManualClock::at(Timestamp::from_secs(1_000));
        assert_eq!(clock.now().as_secs(), 1_000);
        clock.advance(Duration::HOUR);
        assert_eq!(clock.now().as_secs(), 1_000 + 3600);
        assert_eq!(clock.height().0, 1);
    }

    #[test]
    #[should_panic]
    fn manual_clock_cannot_rewind() {
        let mut clock = ManualClock::at(Timestamp::from_secs(1_000));
        clock.set(Timestamp::from_secs(500));
    }
}
