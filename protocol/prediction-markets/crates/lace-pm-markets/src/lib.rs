//! Market types and the market state machine.
//!
//! Four market shapes are supported. The first three are familiar:
//!
//! * [`Binary`] -- yes / no.
//! * [`Scalar`] -- a value within a closed numeric range.
//! * [`MultiOutcome`] -- exactly one of N enumerated outcomes.
//!
//! The fourth is the protocol's lever for *composability*:
//!
//! * [`Conditional`] -- only resolves if a designated parent market
//!   first resolves a specified way.  This is how prediction markets
//!   feed into timelocks (Component 2): a timelock can be gated on a
//!   `Conditional` whose parent is "will event E happen by T".
//!
//! Each market type exposes the same live-probability surface so that
//! the rest of the protocol does not branch on shape. The probability
//! values come from the AMM in `lace-pm-amm`; this crate owns the
//! *state machine* around them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use lace_pm_types::{Address, Bytes32, MarketId, OutcomeId, Probability};
use serde::{Deserialize, Serialize};

/// The kind of market.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketKind {
    /// Yes / no.
    Binary,
    /// Continuous value within `[lo, hi]`. Resolution reports a value
    /// in that range, which the AMM converts to a probability of the
    /// "long" position settling at the high end.
    Scalar {
        /// Inclusive lower bound of the range.
        lo: i128,
        /// Inclusive upper bound of the range.
        hi: i128,
    },
    /// Exactly one of `n` outcomes. The `outcomes` vector records the
    /// canonical [`OutcomeId`] of each branch.
    MultiOutcome {
        /// The list of outcome identifiers for this market.
        outcomes: Vec<OutcomeId>,
    },
    /// Conditional on another market resolving to `parent_outcome`. If
    /// the parent resolves any other way (or is voided), this market
    /// is voided and all positions refunded.
    Conditional {
        /// Parent market.
        parent: MarketId,
        /// Outcome of the parent that must occur for this market to
        /// settle normally.
        parent_outcome: OutcomeId,
        /// The kind of payout this market itself describes once the
        /// parent activates it. Boxed to keep the outer enum small.
        inner: alloc::boxed::Box<MarketKind>,
    },
}

impl MarketKind {
    /// Returns true iff this is a (top-level or wrapped) binary market.
    pub fn is_binary(&self) -> bool {
        matches!(self, MarketKind::Binary)
            || matches!(self, MarketKind::Conditional { inner, .. } if inner.is_binary())
    }

    /// Number of distinct settlement outcomes the AMM should expose.
    pub fn n_outcomes(&self) -> usize {
        match self {
            MarketKind::Binary => 2,
            MarketKind::Scalar { .. } => 2,
            MarketKind::MultiOutcome { outcomes } => outcomes.len(),
            MarketKind::Conditional { inner, .. } => inner.n_outcomes(),
        }
    }
}

/// State machine of a market.
///
/// Transitions:
///
/// ```text
///     Open -> ResolutionWindow -> Resolved
///          \-> Disputed -> Resolved | Voided
///          \-> Voided
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketStatus {
    /// Trading is open and the AMM is quoting.
    Open,
    /// Trading has closed; the oracle is collecting resolution votes.
    ResolutionWindow,
    /// A resolution was reported but is under challenge.
    Disputed,
    /// Settled. The market has a final outcome (or is `Voided`).
    Resolved,
    /// Settled with no outcome. All positions refund.
    Voided,
}

/// The (immutable) descriptor of a market plus its (mutable) state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Market {
    /// Engine-assigned identifier.
    pub id: MarketId,
    /// Creator. Reputation events on bad market design route here.
    pub creator: Address,
    /// Kind.
    pub kind: MarketKind,
    /// Lifecycle phase.
    pub status: MarketStatus,
    /// Block height at which trading closes.
    pub close_height: u64,
    /// Length of the resolution-window phase, in blocks. Within this
    /// window, validators and forecasters submit resolution votes
    /// before any actual settlement.
    pub resolution_window_blocks: u64,
    /// Length of the dispute window after a resolution is reported.
    pub dispute_window_blocks: u64,
    /// Hash of the off-chain question text. Resolvers compare against
    /// this hash to confirm they're answering the right question.
    pub question_hash: Bytes32,
    /// Optional reported outcome -- present iff `status == Resolved`.
    pub resolved_outcome: Option<OutcomeId>,
    /// Optional reported scalar value -- present iff this is a scalar
    /// market that has resolved.
    pub resolved_scalar: Option<i128>,
}

impl Market {
    /// Construct a new market in the `Open` state. Validates the
    /// inputs and returns `Err` if the market shape is malformed.
    pub fn open(
        id: MarketId,
        creator: Address,
        kind: MarketKind,
        close_height: u64,
        resolution_window_blocks: u64,
        dispute_window_blocks: u64,
        question_hash: Bytes32,
    ) -> Result<Self, MarketError> {
        validate_kind(&kind)?;
        if resolution_window_blocks == 0 {
            return Err(MarketError::ZeroResolutionWindow);
        }
        if dispute_window_blocks == 0 {
            return Err(MarketError::ZeroDisputeWindow);
        }
        Ok(Self {
            id,
            creator,
            kind,
            status: MarketStatus::Open,
            close_height,
            resolution_window_blocks,
            dispute_window_blocks,
            question_hash,
            resolved_outcome: None,
            resolved_scalar: None,
        })
    }

    /// Advance the market to the resolution-window phase. Idempotent
    /// in the `ResolutionWindow` state; rejects from terminal states.
    pub fn enter_resolution_window(&mut self) -> Result<(), MarketError> {
        match self.status {
            MarketStatus::Open => {
                self.status = MarketStatus::ResolutionWindow;
                Ok(())
            }
            MarketStatus::ResolutionWindow => Ok(()),
            _ => Err(MarketError::InvalidTransition),
        }
    }

    /// Record a *provisional* resolution -- the dispute window now
    /// opens.
    pub fn report_resolution(
        &mut self,
        outcome: OutcomeId,
        scalar: Option<i128>,
    ) -> Result<(), MarketError> {
        if self.status != MarketStatus::ResolutionWindow
            && self.status != MarketStatus::Disputed
        {
            return Err(MarketError::InvalidTransition);
        }
        // Sanity checks on scalar bounds.
        if let MarketKind::Scalar { lo, hi } = &self.kind {
            let s = scalar.ok_or(MarketError::MissingScalar)?;
            if s < *lo || s > *hi {
                return Err(MarketError::ScalarOutOfRange);
            }
        }
        if let MarketKind::MultiOutcome { outcomes } = &self.kind {
            if !outcomes.contains(&outcome) {
                return Err(MarketError::UnknownOutcome);
            }
        }
        self.resolved_outcome = Some(outcome);
        self.resolved_scalar = scalar;
        self.status = MarketStatus::Disputed;
        Ok(())
    }

    /// Mark the market as finally resolved. Called by the oracle crate
    /// once the dispute window closes uncontested or a contested
    /// resolution is upheld.
    pub fn finalize(&mut self) -> Result<(), MarketError> {
        match self.status {
            MarketStatus::Disputed => {
                if self.resolved_outcome.is_none() {
                    return Err(MarketError::NoResolution);
                }
                self.status = MarketStatus::Resolved;
                Ok(())
            }
            MarketStatus::Resolved => Ok(()),
            _ => Err(MarketError::InvalidTransition),
        }
    }

    /// Void the market. All trading positions refund at the AMM. May
    /// be called from any non-terminal state.
    pub fn void(&mut self) -> Result<(), MarketError> {
        match self.status {
            MarketStatus::Voided | MarketStatus::Resolved => Err(MarketError::InvalidTransition),
            _ => {
                self.status = MarketStatus::Voided;
                self.resolved_outcome = None;
                self.resolved_scalar = None;
                Ok(())
            }
        }
    }

    /// Is the market trading?
    pub fn is_open(&self) -> bool {
        self.status == MarketStatus::Open
    }

    /// Has the market terminated with an outcome that downstream
    /// callers can read?
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, MarketStatus::Resolved | MarketStatus::Voided)
    }
}

/// Compute the "long" probability of a scalar market given a sample
/// of the scalar value -- linear interpolation across `[lo, hi]`.
///
/// Returns `Probability::ZERO` if `s <= lo`, `Probability::ONE` if
/// `s >= hi`, and a linear interpolation otherwise. Range collapses
/// (`lo == hi`) collapse to the midpoint `0.5`.
pub fn scalar_to_probability(lo: i128, hi: i128, s: i128) -> Probability {
    if lo == hi {
        return Probability::from_bps(5_000);
    }
    if s <= lo {
        return Probability::ZERO;
    }
    if s >= hi {
        return Probability::ONE;
    }
    let num = s - lo;
    let den = hi - lo;
    // Convert to basis points with integer math to keep this
    // deterministic across architectures.
    let bps = (num.saturating_mul(10_000) / den) as u32;
    Probability::from_bps(bps)
}

/// Validation errors used by the state machine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MarketError {
    /// `MarketKind::MultiOutcome` had fewer than two outcomes.
    TooFewOutcomes,
    /// `MarketKind::Scalar` was created with `hi < lo`.
    InvertedScalarRange,
    /// `resolution_window_blocks == 0` is not permitted.
    ZeroResolutionWindow,
    /// `dispute_window_blocks == 0` is not permitted.
    ZeroDisputeWindow,
    /// Attempted state transition is not allowed from the current state.
    InvalidTransition,
    /// `report_resolution` was missing a scalar for a scalar market.
    MissingScalar,
    /// Reported scalar fell outside the market's `[lo, hi]`.
    ScalarOutOfRange,
    /// Outcome reported is not in the multi-outcome list.
    UnknownOutcome,
    /// Attempted to finalize a market that has no provisional
    /// resolution.
    NoResolution,
    /// Conditional market's inner kind is itself conditional (we ban
    /// chains).
    NestedConditional,
}

fn validate_kind(kind: &MarketKind) -> Result<(), MarketError> {
    match kind {
        MarketKind::Binary => Ok(()),
        MarketKind::Scalar { lo, hi } => {
            if hi < lo {
                Err(MarketError::InvertedScalarRange)
            } else {
                Ok(())
            }
        }
        MarketKind::MultiOutcome { outcomes } => {
            if outcomes.len() < 2 {
                Err(MarketError::TooFewOutcomes)
            } else {
                Ok(())
            }
        }
        MarketKind::Conditional { inner, .. } => {
            if matches!(**inner, MarketKind::Conditional { .. }) {
                return Err(MarketError::NestedConditional);
            }
            validate_kind(inner)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b32(byte: u8) -> Bytes32 {
        Bytes32([byte; 32])
    }

    fn mk_binary() -> Market {
        Market::open(
            MarketId(b32(1)),
            Address(b32(2)),
            MarketKind::Binary,
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap()
    }

    #[test]
    fn binary_open_transitions_to_resolution_then_dispute_then_resolved() {
        let mut m = mk_binary();
        assert!(m.is_open());
        m.enter_resolution_window().unwrap();
        m.report_resolution(OutcomeId::YES, None).unwrap();
        assert_eq!(m.status, MarketStatus::Disputed);
        m.finalize().unwrap();
        assert!(m.is_terminal());
    }

    #[test]
    fn cannot_report_resolution_from_open() {
        let mut m = mk_binary();
        let err = m.report_resolution(OutcomeId::YES, None).unwrap_err();
        assert_eq!(err, MarketError::InvalidTransition);
    }

    #[test]
    fn scalar_market_requires_scalar_value() {
        let mut m = Market::open(
            MarketId(b32(1)),
            Address(b32(2)),
            MarketKind::Scalar { lo: 0, hi: 100 },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        m.enter_resolution_window().unwrap();
        assert_eq!(
            m.report_resolution(OutcomeId::YES, None),
            Err(MarketError::MissingScalar)
        );
        assert_eq!(
            m.report_resolution(OutcomeId::YES, Some(101)),
            Err(MarketError::ScalarOutOfRange)
        );
        m.report_resolution(OutcomeId::YES, Some(42)).unwrap();
        assert_eq!(m.resolved_scalar, Some(42));
    }

    #[test]
    fn multi_outcome_validates_outcome_id() {
        let a = OutcomeId(b32(10));
        let b = OutcomeId(b32(11));
        let c = OutcomeId(b32(12));
        let mut m = Market::open(
            MarketId(b32(1)),
            Address(b32(2)),
            MarketKind::MultiOutcome {
                outcomes: vec![a, b, c],
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        m.enter_resolution_window().unwrap();
        let bogus = OutcomeId(b32(99));
        assert_eq!(
            m.report_resolution(bogus, None),
            Err(MarketError::UnknownOutcome)
        );
        m.report_resolution(b, None).unwrap();
    }

    #[test]
    fn conditional_rejects_nested_conditional() {
        let parent = MarketId(b32(1));
        let inner_kind = MarketKind::Conditional {
            parent,
            parent_outcome: OutcomeId::YES,
            inner: Box::new(MarketKind::Binary),
        };
        let outer_kind = MarketKind::Conditional {
            parent,
            parent_outcome: OutcomeId::YES,
            inner: Box::new(inner_kind),
        };
        let err = Market::open(
            MarketId(b32(2)),
            Address(b32(2)),
            outer_kind,
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap_err();
        assert_eq!(err, MarketError::NestedConditional);
    }

    #[test]
    fn multi_outcome_requires_two_outcomes() {
        let err = Market::open(
            MarketId(b32(2)),
            Address(b32(2)),
            MarketKind::MultiOutcome {
                outcomes: vec![OutcomeId::YES],
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap_err();
        assert_eq!(err, MarketError::TooFewOutcomes);
    }

    #[test]
    fn scalar_to_probability_linear_interpolation() {
        assert_eq!(scalar_to_probability(0, 100, -5).bps(), 0);
        assert_eq!(scalar_to_probability(0, 100, 150).bps(), 10_000);
        assert_eq!(scalar_to_probability(0, 100, 50).bps(), 5_000);
        assert_eq!(scalar_to_probability(0, 100, 25).bps(), 2_500);
    }

    #[test]
    fn scalar_to_probability_collapsed_range_is_midpoint() {
        assert_eq!(scalar_to_probability(42, 42, 99).bps(), 5_000);
    }

    #[test]
    fn voided_market_rejects_finalize() {
        let mut m = mk_binary();
        m.void().unwrap();
        assert_eq!(m.finalize(), Err(MarketError::InvalidTransition));
    }
}
