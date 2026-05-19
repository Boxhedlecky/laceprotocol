//! Conditional release logic for time + external outcomes.
//!
//! A `Condition` is a small boolean algebra over two kinds of leaves:
//!
//! 1. **Time leaves** -- `Before(ts)`, `After(ts)`, `Deadline(ts)`,
//!    `Within(interval)`. Resolved purely from the [`Clock`].
//! 2. **External leaves** -- `External(oracle_ref, expected)`.
//!    Resolved by Component 3 (the prediction-market engine), which
//!    implements [`OracleResolver`].
//!
//! Conditions compose with `And`, `Or`, `Not`. The whole tree is
//! evaluated to one of three states:
//!
//! - `Ready` -- the condition is satisfied right now.
//! - `Pending` -- not satisfied yet, but could become satisfied later.
//! - `Failed` -- can no longer become satisfied (e.g. a deadline
//!   passed in an `And`-branch). The contract layer treats `Failed`
//!   as a terminal state and either reverts the locked funds or
//!   triggers the dispute path, per the contract's policy.
//!
//! The three-valued logic is the entire point of this crate. A naive
//! "true / false" evaluation would prevent the contract layer from
//! distinguishing "wait" from "give up", and we need that distinction
//! for both correctness (don't slash a recurring payment that simply
//! hasn't reached its first tick) and UX ("we are waiting on the
//! market" is a very different message from "this can never resolve").

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

use lace_time::{Clock, Interval, Timestamp};
use lace_vm::Bytes32;
use serde::{Deserialize, Serialize};

/// Three-valued evaluation result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Condition is satisfied. The contract may proceed.
    Ready,
    /// Not satisfied yet, but may become satisfied. The contract
    /// should wait and re-evaluate later.
    Pending,
    /// Can no longer become satisfied. The contract must transition
    /// to its failure path.
    Failed,
}

impl Resolution {
    /// `And` over two resolutions. `Failed` is absorbing for `And`.
    pub fn and(self, other: Resolution) -> Resolution {
        match (self, other) {
            (Resolution::Failed, _) | (_, Resolution::Failed) => Resolution::Failed,
            (Resolution::Pending, _) | (_, Resolution::Pending) => Resolution::Pending,
            (Resolution::Ready, Resolution::Ready) => Resolution::Ready,
        }
    }

    /// `Or` over two resolutions. `Ready` is absorbing for `Or`.
    pub fn or(self, other: Resolution) -> Resolution {
        match (self, other) {
            (Resolution::Ready, _) | (_, Resolution::Ready) => Resolution::Ready,
            (Resolution::Failed, Resolution::Failed) => Resolution::Failed,
            _ => Resolution::Pending,
        }
    }

    /// `Not` over a resolution. `Pending` stays `Pending` -- we don't
    /// know yet, and that uncertainty inverts to uncertainty, not to
    /// `Ready`. Symmetric with [`Resolution::and`] / [`Resolution::or`]
    /// rather than mirroring `std::ops::Not` -- a free function would
    /// break the algebra's call style.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Resolution {
        match self {
            Resolution::Ready => Resolution::Failed,
            Resolution::Failed => Resolution::Ready,
            Resolution::Pending => Resolution::Pending,
        }
    }
}

/// A time-based leaf condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeCond {
    /// Satisfied iff `now > ts`. Symmetric with [`lace_vm::Op::After`].
    After(Timestamp),
    /// Satisfied iff `now < ts`. Transitions to `Failed` once `now >= ts`.
    Before(Timestamp),
    /// Like `Before`, but the *contract*'s deadline -- treated as a
    /// hard failure rather than a soft "missed window" when it expires.
    /// Functionally identical to `Before` at the resolver level; the
    /// distinction is in how the contract handles the transition.
    Deadline(Timestamp),
    /// Satisfied iff `now` falls within the half-open interval.
    /// Becomes `Failed` once `now >= interval.end`.
    Within(Interval),
}

impl TimeCond {
    fn resolve(&self, clock: &dyn Clock) -> Resolution {
        let now = clock.now();
        match self {
            TimeCond::After(ts) => {
                if now > *ts {
                    Resolution::Ready
                } else {
                    Resolution::Pending
                }
            }
            TimeCond::Before(ts) | TimeCond::Deadline(ts) => {
                if now < *ts {
                    Resolution::Ready
                } else {
                    Resolution::Failed
                }
            }
            TimeCond::Within(interval) => {
                if interval.contains(now) {
                    Resolution::Ready
                } else if now < interval.start {
                    Resolution::Pending
                } else {
                    Resolution::Failed
                }
            }
        }
    }
}

/// An external (oracle / prediction-market) leaf condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCond {
    /// Opaque identifier the prediction-market engine recognises.
    pub oracle: Bytes32,
    /// Expected outcome hash. The condition becomes `Ready` iff the
    /// oracle reports this exact outcome.
    pub expected: Bytes32,
}

/// One outcome reported by an oracle.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OracleAnswer {
    /// The oracle has resolved to a concrete outcome.
    Resolved(Bytes32),
    /// The oracle has not resolved yet.
    Pending,
    /// The oracle has resolved with no outcome (e.g. market voided).
    /// External conditions referencing such an oracle transition to
    /// `Failed`.
    Voided,
}

/// Component 3 implements this trait to feed external answers into
/// condition resolution.
pub trait OracleResolver {
    /// Look up the current answer for the given oracle reference.
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer;
}

/// A no-op resolver: every oracle is `Pending`. Useful for tests that
/// exercise only the time-based branches and want to be sure the
/// external branch is short-circuited by the boolean structure.
pub struct PendingResolver;

impl OracleResolver for PendingResolver {
    fn answer(&self, _: &Bytes32) -> OracleAnswer {
        OracleAnswer::Pending
    }
}

/// The condition tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    /// Always satisfied. Useful as a degenerate base case.
    True,
    /// Never satisfied -- and never *will* be, so this resolves to
    /// `Failed`, not `Pending`.
    False,
    /// Time-based leaf.
    Time(TimeCond),
    /// External oracle leaf.
    External(ExternalCond),
    /// Both children must resolve to `Ready`.
    And(Box<Condition>, Box<Condition>),
    /// Either child must resolve to `Ready`.
    Or(Box<Condition>, Box<Condition>),
    /// Inverts the child resolution.
    Not(Box<Condition>),
}

impl Condition {
    /// Convenience constructor for the `Time AND external` pattern
    /// the brief calls out as the primary use case.
    pub fn time_and_external(time: TimeCond, oracle: Bytes32, expected: Bytes32) -> Condition {
        Condition::And(
            Box::new(Condition::Time(time)),
            Box::new(Condition::External(ExternalCond { oracle, expected })),
        )
    }

    /// Convenience constructor for the `Time OR external` pattern.
    pub fn time_or_external(time: TimeCond, oracle: Bytes32, expected: Bytes32) -> Condition {
        Condition::Or(
            Box::new(Condition::Time(time)),
            Box::new(Condition::External(ExternalCond { oracle, expected })),
        )
    }

    /// Resolve the condition against the supplied clock and oracle
    /// resolver. The result is always three-valued.
    pub fn resolve(&self, clock: &dyn Clock, oracles: &dyn OracleResolver) -> Resolution {
        match self {
            Condition::True => Resolution::Ready,
            Condition::False => Resolution::Failed,
            Condition::Time(t) => t.resolve(clock),
            Condition::External(e) => match oracles.answer(&e.oracle) {
                OracleAnswer::Resolved(actual) => {
                    if actual == e.expected {
                        Resolution::Ready
                    } else {
                        Resolution::Failed
                    }
                }
                OracleAnswer::Pending => Resolution::Pending,
                OracleAnswer::Voided => Resolution::Failed,
            },
            Condition::And(a, b) => a.resolve(clock, oracles).and(b.resolve(clock, oracles)),
            Condition::Or(a, b) => a.resolve(clock, oracles).or(b.resolve(clock, oracles)),
            Condition::Not(c) => c.resolve(clock, oracles).not(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_time::{Duration, ManualClock};
    use std::collections::HashMap;

    struct MockOracles(HashMap<Bytes32, OracleAnswer>);

    impl OracleResolver for MockOracles {
        fn answer(&self, oracle: &Bytes32) -> OracleAnswer {
            self.0.get(oracle).copied().unwrap_or(OracleAnswer::Pending)
        }
    }

    fn make_oracle(b: u8) -> Bytes32 {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    #[test]
    fn and_pending_until_both_ready() {
        let clock = ManualClock::at(Timestamp::from_secs(1_500));
        let oracle = make_oracle(1);
        let expected = make_oracle(2);
        let cond = Condition::time_and_external(
            TimeCond::After(Timestamp::from_secs(1_000)),
            oracle,
            expected,
        );
        let mut oracles = MockOracles(HashMap::new());
        // Time ready, oracle pending -> Pending.
        assert_eq!(cond.resolve(&clock, &oracles), Resolution::Pending);
        // Oracle resolved with wrong answer -> Failed.
        oracles.0.insert(oracle, OracleAnswer::Resolved(make_oracle(99)));
        assert_eq!(cond.resolve(&clock, &oracles), Resolution::Failed);
        // Oracle resolved with expected answer -> Ready.
        oracles.0.insert(oracle, OracleAnswer::Resolved(expected));
        assert_eq!(cond.resolve(&clock, &oracles), Resolution::Ready);
    }

    #[test]
    fn or_short_circuits_on_ready() {
        let clock = ManualClock::at(Timestamp::from_secs(1_500));
        let cond = Condition::time_or_external(
            TimeCond::After(Timestamp::from_secs(1_000)),
            make_oracle(1),
            make_oracle(2),
        );
        // Time ready -> whole expression is Ready regardless of oracle.
        assert_eq!(
            cond.resolve(&clock, &MockOracles(HashMap::new())),
            Resolution::Ready
        );
    }

    #[test]
    fn deadline_transitions_to_failed() {
        let clock = ManualClock::at(Timestamp::from_secs(2_001));
        let cond = Condition::Time(TimeCond::Deadline(Timestamp::from_secs(2_000)));
        assert_eq!(cond.resolve(&clock, &PendingResolver), Resolution::Failed);
    }

    #[test]
    fn within_window_lifecycle() {
        let cond = Condition::Time(TimeCond::Within(Interval::new(
            Timestamp::from_secs(100),
            Timestamp::from_secs(200),
        )));
        let mut clock = ManualClock::at(Timestamp::from_secs(50));
        assert_eq!(cond.resolve(&clock, &PendingResolver), Resolution::Pending);
        clock.advance(Duration::from_secs(75));
        assert_eq!(cond.resolve(&clock, &PendingResolver), Resolution::Ready);
        clock.advance(Duration::from_secs(200));
        assert_eq!(cond.resolve(&clock, &PendingResolver), Resolution::Failed);
    }

    #[test]
    fn not_preserves_pending() {
        let cond = Condition::Not(Box::new(Condition::External(ExternalCond {
            oracle: make_oracle(1),
            expected: make_oracle(2),
        })));
        let clock = ManualClock::at(Timestamp::from_secs(0));
        assert_eq!(cond.resolve(&clock, &PendingResolver), Resolution::Pending);
    }
}
