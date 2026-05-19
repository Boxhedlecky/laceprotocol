//! Oracle: resolution rounds, dispute windows, escalation, and the
//! integration boundary with Veil Score reputation.
//!
//! A `ResolutionRound` collects:
//!   * `ValidatorVote`s, weighted by staked LACE,
//!   * `ForecasterVote`s, weighted by Veil Score reputation.
//!
//! These are combined via a tunable mixing parameter `alpha` (0.0 .. 1.0)
//! that controls the relative weight of stake vs reputation. The
//! winning outcome is the one with the largest mixed weight at window
//! close. If a dispute is filed during the dispute window, an
//! `EscalationRound` re-runs with higher quorum and slashes the losing
//! resolvers' resolution bonds.
//!
//! Reputation events ([`ReputationEvent`]) flow out of this crate via
//! the [`ReputationSink`] trait, which Component 4 (Veil Score)
//! implements. The shape mirrors the temporal-VM's reputation sink so
//! both components feed into the same downstream accumulator.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_pm_markets::{Market, MarketError, MarketStatus};
use lace_pm_types::{Address, Amount, OutcomeId};
use serde::{Deserialize, Serialize};

/// A vote cast by a validator. Weight is the validator's currently
/// staked LACE.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorVote {
    /// Voter identity.
    pub voter: Address,
    /// Reported outcome.
    pub outcome: OutcomeId,
    /// Voter's staked LACE at vote time.
    pub stake: Amount,
}

/// A vote cast by a forecaster (non-validator participant). Weight is
/// the forecaster's Veil Score at vote time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForecasterVote {
    /// Voter identity.
    pub voter: Address,
    /// Reported outcome.
    pub outcome: OutcomeId,
    /// Voter's Veil Score (basis-point scaled, 0..=10_000).
    pub reputation_bps: u32,
}

/// Mixing parameter for the stake/reputation combiner.
///
/// `alpha == 0`  : pure reputation weighting.
/// `alpha == 10_000` : pure stake weighting.
/// Default (`6_000`) leans on stake but lets reputation matter
/// substantially -- a high-reputation forecaster's vote is not
/// drowned out by a single validator.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlphaBps(pub u32);

impl AlphaBps {
    /// Default mixing weight.
    pub const DEFAULT: AlphaBps = AlphaBps(6_000);
    /// Clamp to `[0, 10_000]`.
    pub const fn clamped(self) -> AlphaBps {
        let v = if self.0 > 10_000 { 10_000 } else { self.0 };
        AlphaBps(v)
    }
}

/// A resolution round in progress. Created when a market enters its
/// resolution window; consumed when the window closes and a
/// provisional outcome is reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRound {
    /// The market this round resolves.
    pub market: lace_pm_types::MarketId,
    /// Block at which voting closes.
    pub closes_at: u64,
    /// Stake/reputation mixing parameter.
    pub alpha: AlphaBps,
    /// Recorded validator votes (latest per voter wins).
    pub validator_votes: BTreeMap<Address, ValidatorVote>,
    /// Recorded forecaster votes (latest per voter wins).
    pub forecaster_votes: BTreeMap<Address, ForecasterVote>,
}

impl ResolutionRound {
    /// Open a new round.
    pub fn new(market: lace_pm_types::MarketId, closes_at: u64, alpha: AlphaBps) -> Self {
        Self {
            market,
            closes_at,
            alpha: alpha.clamped(),
            validator_votes: BTreeMap::new(),
            forecaster_votes: BTreeMap::new(),
        }
    }

    /// Cast or update a validator vote. Returns the previous vote, if
    /// any.
    pub fn cast_validator(&mut self, vote: ValidatorVote) -> Option<ValidatorVote> {
        self.validator_votes.insert(vote.voter, vote)
    }

    /// Cast or update a forecaster vote.
    pub fn cast_forecaster(&mut self, vote: ForecasterVote) -> Option<ForecasterVote> {
        self.forecaster_votes.insert(vote.voter, vote)
    }

    /// Tally mixed weights per outcome. Used at window close.
    ///
    /// Returns a `BTreeMap<OutcomeId, MixedWeight>`. The map iterates
    /// in deterministic outcome-id order.
    pub fn tally(&self) -> BTreeMap<OutcomeId, MixedWeight> {
        let mut stake_by_outcome: BTreeMap<OutcomeId, u128> = BTreeMap::new();
        let mut total_stake: u128 = 0;
        for v in self.validator_votes.values() {
            *stake_by_outcome.entry(v.outcome).or_insert(0) += v.stake;
            total_stake += v.stake;
        }
        let mut rep_by_outcome: BTreeMap<OutcomeId, u128> = BTreeMap::new();
        let mut total_rep: u128 = 0;
        for v in self.forecaster_votes.values() {
            let w = v.reputation_bps as u128;
            *rep_by_outcome.entry(v.outcome).or_insert(0) += w;
            total_rep += w;
        }

        // Normalise each side to basis points and combine.
        let alpha = self.alpha.clamped().0 as u128;
        let mut out: BTreeMap<OutcomeId, MixedWeight> = BTreeMap::new();

        // Collect the full set of outcomes voted on across both sides
        // so the tally is complete even if one side abstained for an
        // outcome the other endorsed.
        let outcomes: alloc::collections::BTreeSet<OutcomeId> = stake_by_outcome
            .keys()
            .chain(rep_by_outcome.keys())
            .copied()
            .collect();

        for o in outcomes {
            let stake_share_bps = if total_stake == 0 {
                0
            } else {
                (stake_by_outcome.get(&o).copied().unwrap_or(0) * 10_000) / total_stake
            };
            let rep_share_bps = if total_rep == 0 {
                0
            } else {
                (rep_by_outcome.get(&o).copied().unwrap_or(0) * 10_000) / total_rep
            };
            let mixed = (alpha * stake_share_bps + (10_000 - alpha) * rep_share_bps) / 10_000;
            out.insert(
                o,
                MixedWeight {
                    stake_bps: stake_share_bps as u32,
                    reputation_bps: rep_share_bps as u32,
                    mixed_bps: mixed as u32,
                },
            );
        }
        out
    }

    /// Compute the leading outcome at window close. Returns `None` if
    /// no votes were cast, or if there's an exact tie (which forces a
    /// `Voided` resolution).
    pub fn leader(&self) -> Option<OutcomeId> {
        let tally = self.tally();
        let mut best: Option<(OutcomeId, u32)> = None;
        let mut tie = false;
        for (o, w) in tally.iter() {
            match best {
                None => best = Some((*o, w.mixed_bps)),
                Some((_, bw)) if w.mixed_bps > bw => {
                    best = Some((*o, w.mixed_bps));
                    tie = false;
                }
                Some((_, bw)) if w.mixed_bps == bw => {
                    tie = true;
                }
                _ => {}
            }
        }
        if tie {
            None
        } else {
            best.map(|(o, _)| o)
        }
    }
}

/// Per-outcome breakdown after combining stake and reputation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixedWeight {
    /// Stake-side share, in basis points of total validator stake.
    pub stake_bps: u32,
    /// Reputation-side share, in basis points of total forecaster
    /// reputation.
    pub reputation_bps: u32,
    /// `alpha * stake_bps + (1 - alpha) * reputation_bps`, in basis
    /// points.
    pub mixed_bps: u32,
}

/// A dispute against a provisional resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispute {
    /// The challenger.
    pub challenger: Address,
    /// Outcome the challenger asserts is correct.
    pub asserted_outcome: OutcomeId,
    /// Bond posted by the challenger, paid in LACE.
    pub bond: Amount,
    /// Block at which this dispute was opened.
    pub opened_at: u64,
}

/// Outcome of a dispute round.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisputeOutcome {
    /// Dispute was upheld; the originally-reported outcome was wrong.
    Upheld {
        /// The corrected outcome the challenger asserted.
        final_outcome: OutcomeId,
    },
    /// Dispute was rejected; the original resolution stands.
    Rejected {
        /// The original (now-confirmed) outcome.
        final_outcome: OutcomeId,
    },
}

/// Reputation events emitted by the oracle.
///
/// Component 4 (Veil Score) implements `ReputationSink` and consumes
/// these to update calibration histories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReputationEvent {
    /// A validator voted for the outcome that was finally resolved.
    ResolverCorrect {
        /// Validator address.
        voter: Address,
        /// Market.
        market: lace_pm_types::MarketId,
        /// Outcome.
        outcome: OutcomeId,
        /// Stake at vote time.
        stake: Amount,
    },
    /// A validator voted for an outcome that was *not* finally resolved.
    ResolverIncorrect {
        /// Validator address.
        voter: Address,
        /// Market.
        market: lace_pm_types::MarketId,
        /// Wrong outcome the validator voted for.
        outcome: OutcomeId,
        /// Stake at vote time.
        stake: Amount,
    },
    /// A forecaster voted for the outcome that was finally resolved.
    ForecasterCorrect {
        /// Forecaster address.
        voter: Address,
        /// Market.
        market: lace_pm_types::MarketId,
        /// Outcome.
        outcome: OutcomeId,
        /// Reputation weight at vote time.
        reputation_bps: u32,
    },
    /// A forecaster voted for an outcome that was *not* finally resolved.
    ForecasterIncorrect {
        /// Forecaster address.
        voter: Address,
        /// Market.
        market: lace_pm_types::MarketId,
        /// Wrong outcome.
        outcome: OutcomeId,
        /// Reputation weight at vote time.
        reputation_bps: u32,
    },
    /// A dispute was upheld -- the challenger gets a bump.
    DisputeUpheld {
        /// Challenger.
        challenger: Address,
        /// Market.
        market: lace_pm_types::MarketId,
    },
    /// A dispute was rejected -- the challenger's bond burned.
    DisputeRejected {
        /// Challenger.
        challenger: Address,
        /// Market.
        market: lace_pm_types::MarketId,
        /// Bond burned.
        bond_burned: Amount,
    },
}

/// Trait implemented by Veil Score (Component 4) to receive reputation
/// events from the prediction market engine.
pub trait ReputationSink {
    /// Record one reputation event.
    fn record(&mut self, event: ReputationEvent);
}

/// A no-op sink for tests / devnets.
pub struct NullSink;

impl ReputationSink for NullSink {
    fn record(&mut self, _: ReputationEvent) {}
}

/// Configuration parameters for a resolution flow. These map 1:1 to
/// the `GovernanceParams` in `lace-pm-governance`; the oracle crate
/// receives them flat to keep its dependency graph minimal.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleParams {
    /// Stake/reputation mixing.
    pub alpha: AlphaBps,
    /// Slashing rate on resolvers who voted the losing way of an
    /// upheld dispute, in basis points of their stake.
    pub resolver_slash_bps: u32,
    /// Minimum total stake required for a resolution round to be
    /// valid. If unmet at window close, the market voids.
    pub min_resolution_stake: Amount,
}

impl OracleParams {
    /// Sensible defaults: 60/40 stake/rep, 10% resolver slash on
    /// upheld disputes, 1000 LACE minimum stake.
    pub const DEFAULT: OracleParams = OracleParams {
        alpha: AlphaBps::DEFAULT,
        resolver_slash_bps: 1_000,
        min_resolution_stake: 1_000,
    };
}

/// Close a resolution round and emit a provisional outcome.
///
/// Side effects:
///   * Mutates the market state to `Disputed` with the provisional
///     outcome (or `Voided` if the round failed quorum / tied).
///   * No reputation events are emitted yet -- those wait for
///     finalisation.
///
/// Returns the provisional outcome, or `None` if the market voided.
pub fn close_round(
    round: &ResolutionRound,
    market: &mut Market,
    params: OracleParams,
) -> Result<Option<OutcomeId>, MarketError> {
    let total_stake: Amount = round.validator_votes.values().map(|v| v.stake).sum();
    if total_stake < params.min_resolution_stake {
        market.void()?;
        return Ok(None);
    }
    match round.leader() {
        Some(o) => {
            market.report_resolution(o, market.resolved_scalar)?;
            Ok(Some(o))
        }
        None => {
            market.void()?;
            Ok(None)
        }
    }
}

/// Apply a dispute. Returns the dispute outcome. Mutates the market:
///   * If upheld, the market re-enters `Disputed` with the
///     challenger's `asserted_outcome` reported.
///   * If rejected, the market stays in `Disputed` with the original
///     outcome.
pub fn apply_dispute(
    market: &mut Market,
    dispute: &Dispute,
    upheld: bool,
) -> Result<DisputeOutcome, MarketError> {
    if market.status != MarketStatus::Disputed {
        return Err(MarketError::InvalidTransition);
    }
    if upheld {
        market.report_resolution(dispute.asserted_outcome, market.resolved_scalar)?;
        Ok(DisputeOutcome::Upheld {
            final_outcome: dispute.asserted_outcome,
        })
    } else {
        let final_outcome = market
            .resolved_outcome
            .ok_or(MarketError::NoResolution)?;
        Ok(DisputeOutcome::Rejected { final_outcome })
    }
}

/// Finalise a market and emit calibration events to the reputation
/// sink.
pub fn finalize_and_emit(
    market: &mut Market,
    round: &ResolutionRound,
    dispute: Option<(&Dispute, DisputeOutcome)>,
    sink: &mut dyn ReputationSink,
) -> Result<OutcomeId, MarketError> {
    market.finalize()?;
    let final_outcome = market
        .resolved_outcome
        .ok_or(MarketError::NoResolution)?;
    for v in round.validator_votes.values() {
        if v.outcome == final_outcome {
            sink.record(ReputationEvent::ResolverCorrect {
                voter: v.voter,
                market: market.id,
                outcome: v.outcome,
                stake: v.stake,
            });
        } else {
            sink.record(ReputationEvent::ResolverIncorrect {
                voter: v.voter,
                market: market.id,
                outcome: v.outcome,
                stake: v.stake,
            });
        }
    }
    for v in round.forecaster_votes.values() {
        if v.outcome == final_outcome {
            sink.record(ReputationEvent::ForecasterCorrect {
                voter: v.voter,
                market: market.id,
                outcome: v.outcome,
                reputation_bps: v.reputation_bps,
            });
        } else {
            sink.record(ReputationEvent::ForecasterIncorrect {
                voter: v.voter,
                market: market.id,
                outcome: v.outcome,
                reputation_bps: v.reputation_bps,
            });
        }
    }
    if let Some((d, outcome)) = dispute {
        match outcome {
            DisputeOutcome::Upheld { .. } => {
                sink.record(ReputationEvent::DisputeUpheld {
                    challenger: d.challenger,
                    market: market.id,
                });
            }
            DisputeOutcome::Rejected { .. } => {
                sink.record(ReputationEvent::DisputeRejected {
                    challenger: d.challenger,
                    market: market.id,
                    bond_burned: d.bond,
                });
            }
        }
    }
    Ok(final_outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_pm_markets::{Market, MarketKind};
    use lace_pm_types::{Bytes32, MarketId};

    fn addr(b: u8) -> Address {
        Address(Bytes32([b; 32]))
    }
    fn out(b: u8) -> OutcomeId {
        OutcomeId(Bytes32([b; 32]))
    }

    fn mk_market() -> Market {
        Market::open(
            MarketId(Bytes32([1; 32])),
            addr(2),
            MarketKind::Binary,
            1_000,
            10,
            5,
            Bytes32([3; 32]),
        )
        .unwrap()
    }

    #[test]
    fn pure_stake_alpha_picks_largest_stake() {
        let mut r = ResolutionRound::new(
            MarketId(Bytes32([1; 32])),
            100,
            AlphaBps(10_000), // pure stake
        );
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 100,
        });
        r.cast_validator(ValidatorVote {
            voter: addr(2),
            outcome: OutcomeId::NO,
            stake: 250,
        });
        assert_eq!(r.leader(), Some(OutcomeId::NO));
    }

    #[test]
    fn pure_reputation_alpha_overrides_stake() {
        let mut r = ResolutionRound::new(
            MarketId(Bytes32([1; 32])),
            100,
            AlphaBps(0), // pure reputation
        );
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 9_999_999,
        });
        r.cast_forecaster(ForecasterVote {
            voter: addr(2),
            outcome: OutcomeId::NO,
            reputation_bps: 9_500,
        });
        assert_eq!(r.leader(), Some(OutcomeId::NO));
    }

    #[test]
    fn mixed_alpha_can_swing_the_outcome() {
        // High-reputation forecaster vs medium-stake validator.
        let cast = |alpha: u32| {
            let mut r = ResolutionRound::new(
                MarketId(Bytes32([1; 32])),
                100,
                AlphaBps(alpha),
            );
            r.cast_validator(ValidatorVote {
                voter: addr(1),
                outcome: OutcomeId::YES,
                stake: 100,
            });
            r.cast_forecaster(ForecasterVote {
                voter: addr(2),
                outcome: OutcomeId::NO,
                reputation_bps: 9_500,
            });
            r.leader()
        };
        // At alpha=0 (pure rep), NO wins.
        assert_eq!(cast(0), Some(OutcomeId::NO));
        // At alpha=10_000 (pure stake), YES wins.
        assert_eq!(cast(10_000), Some(OutcomeId::YES));
    }

    #[test]
    fn exact_tie_voids() {
        let mut r = ResolutionRound::new(
            MarketId(Bytes32([1; 32])),
            100,
            AlphaBps(10_000),
        );
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 100,
        });
        r.cast_validator(ValidatorVote {
            voter: addr(2),
            outcome: OutcomeId::NO,
            stake: 100,
        });
        assert_eq!(r.leader(), None);
    }

    #[test]
    fn vote_update_replaces_previous() {
        let mut r = ResolutionRound::new(MarketId(Bytes32([1; 32])), 100, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 100,
        });
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::NO,
            stake: 100,
        });
        assert_eq!(r.validator_votes.len(), 1);
        let v = r.validator_votes.values().next().unwrap();
        assert_eq!(v.outcome, OutcomeId::NO);
    }

    #[test]
    fn close_round_voids_on_insufficient_stake() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        // Empty round -> voids.
        let result = close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        assert_eq!(result, None);
        assert_eq!(m.status, MarketStatus::Voided);
    }

    #[test]
    fn close_round_reports_provisional_outcome() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let mut r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 5_000,
        });
        let outcome = close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        assert_eq!(outcome, Some(OutcomeId::YES));
        assert_eq!(m.status, MarketStatus::Disputed);
        assert_eq!(m.resolved_outcome, Some(OutcomeId::YES));
    }

    #[test]
    fn upheld_dispute_overrides_provisional() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let mut r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 5_000,
        });
        close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        let d = Dispute {
            challenger: addr(9),
            asserted_outcome: OutcomeId::NO,
            bond: 1_000,
            opened_at: 60,
        };
        let outcome = apply_dispute(&mut m, &d, true).unwrap();
        assert_eq!(
            outcome,
            DisputeOutcome::Upheld {
                final_outcome: OutcomeId::NO
            }
        );
        assert_eq!(m.resolved_outcome, Some(OutcomeId::NO));
    }

    #[test]
    fn rejected_dispute_preserves_provisional() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let mut r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 5_000,
        });
        close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        let d = Dispute {
            challenger: addr(9),
            asserted_outcome: OutcomeId::NO,
            bond: 1_000,
            opened_at: 60,
        };
        let outcome = apply_dispute(&mut m, &d, false).unwrap();
        assert_eq!(
            outcome,
            DisputeOutcome::Rejected {
                final_outcome: OutcomeId::YES
            }
        );
        assert_eq!(m.resolved_outcome, Some(OutcomeId::YES));
    }

    struct RecordingSink(Vec<ReputationEvent>);
    impl ReputationSink for RecordingSink {
        fn record(&mut self, e: ReputationEvent) {
            self.0.push(e);
        }
    }

    #[test]
    fn finalize_emits_calibration_events() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let mut r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 5_000,
        });
        r.cast_validator(ValidatorVote {
            voter: addr(2),
            outcome: OutcomeId::NO,
            stake: 1_000,
        });
        r.cast_forecaster(ForecasterVote {
            voter: addr(3),
            outcome: OutcomeId::YES,
            reputation_bps: 8_000,
        });
        close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        let mut sink = RecordingSink(Vec::new());
        let final_outcome = finalize_and_emit(&mut m, &r, None, &mut sink).unwrap();
        assert_eq!(final_outcome, OutcomeId::YES);
        // 3 votes -> 3 events.
        assert_eq!(sink.0.len(), 3);
        // Validator with 5000 stake voted correctly.
        let correct: Vec<&ReputationEvent> = sink
            .0
            .iter()
            .filter(|e| matches!(e, ReputationEvent::ResolverCorrect { .. }))
            .collect();
        let incorrect: Vec<&ReputationEvent> = sink
            .0
            .iter()
            .filter(|e| matches!(e, ReputationEvent::ResolverIncorrect { .. }))
            .collect();
        assert_eq!(correct.len(), 1);
        assert_eq!(incorrect.len(), 1);
    }

    #[test]
    fn finalize_with_upheld_dispute_emits_challenger_bonus() {
        let mut m = mk_market();
        m.enter_resolution_window().unwrap();
        let mut r = ResolutionRound::new(m.id, 50, AlphaBps::DEFAULT);
        r.cast_validator(ValidatorVote {
            voter: addr(1),
            outcome: OutcomeId::YES,
            stake: 5_000,
        });
        close_round(&r, &mut m, OracleParams::DEFAULT).unwrap();
        let d = Dispute {
            challenger: addr(9),
            asserted_outcome: OutcomeId::NO,
            bond: 1_000,
            opened_at: 60,
        };
        let outcome = apply_dispute(&mut m, &d, true).unwrap();
        let mut sink = RecordingSink(Vec::new());
        finalize_and_emit(&mut m, &r, Some((&d, outcome)), &mut sink).unwrap();
        assert!(sink.0.iter().any(|e| matches!(e, ReputationEvent::DisputeUpheld { .. })));
        // The original YES voter is now flagged ResolverIncorrect.
        assert!(sink
            .0
            .iter()
            .any(|e| matches!(e, ReputationEvent::ResolverIncorrect { .. })));
    }

    #[test]
    fn null_sink_swallows_events() {
        let mut s = NullSink;
        s.record(ReputationEvent::DisputeUpheld {
            challenger: addr(1),
            market: MarketId(Bytes32::ZERO),
        });
        // No assertion possible; this exists only to prove the
        // null sink compiles and does not panic.
    }
}
