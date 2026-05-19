//! Prediction-gated governance.
//!
//! Two responsibilities:
//!
//! 1. **Prediction-gated upgrade execution.** A governance proposal
//!    binds an upgrade payload to a binary forecast market: "will the
//!    network adopt this upgrade safely within window W?" The
//!    proposal executes iff the market clears an adoption threshold
//!    (default 65 % YES at window close) *and* the market's traded
//!    volume exceeds a liquidity threshold. The liquidity gate is
//!    what prevents a thin market with a single manipulator from
//!    pushing through an upgrade.
//!
//! 2. **Parameter governance.** The on-chain mutable parameters of
//!    the prediction-market engine -- fees, the stake/reputation
//!    mixer alpha, default resolution / dispute window lengths,
//!    default LMSR liquidity `b` -- live in [`GovernanceParams`].
//!    Updates go through the same prediction-gated path so a
//!    parameter change is itself a forecastable event.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_pm_amm::LmsrState;
use lace_pm_compose::Engine;
use lace_pm_markets::{Market, MarketKind};
use lace_pm_oracle::{AlphaBps, OracleParams};
use lace_pm_types::{Address, Amount, Bytes32, FeeSchedule, MarketId, OutcomeId, Probability};
use serde::{Deserialize, Serialize};

/// All on-chain mutable parameters of the prediction-market engine.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GovernanceParams {
    /// Trade fee + sub-bps routing.
    pub fees: FeeSchedule,
    /// Stake/reputation mixing constant.
    pub alpha: AlphaBps,
    /// Default resolution-window length in blocks for newly-created
    /// markets.
    pub default_resolution_window_blocks: u64,
    /// Default dispute window in blocks for newly-created markets.
    pub default_dispute_window_blocks: u64,
    /// Default LMSR liquidity `b` for newly-created markets.
    pub default_liquidity_b: f64,
    /// Adoption threshold (YES probability, in basis points) for
    /// upgrade markets. Default: 65 %.
    pub adoption_threshold_bps: u32,
    /// Minimum cumulative pool cost a market must accrue before its
    /// resolution can gate an upgrade. Default: equivalent to
    /// 100 LACE moved through the AMM.
    pub min_market_volume: Amount,
    /// Resolver-side slashing rate. Mirrors `OracleParams`.
    pub resolver_slash_bps: u32,
    /// Minimum resolution-round stake. Mirrors `OracleParams`.
    pub min_resolution_stake: Amount,
}

impl GovernanceParams {
    /// Sensible mainnet defaults.
    pub const DEFAULT: GovernanceParams = GovernanceParams {
        fees: FeeSchedule::DEFAULT,
        alpha: AlphaBps::DEFAULT,
        default_resolution_window_blocks: 1_440, // ~ a day at 1 block / minute
        default_dispute_window_blocks: 720,      // ~ 12 hours
        default_liquidity_b: 1_000_000.0,
        adoption_threshold_bps: 6_500,
        min_market_volume: 100,
        resolver_slash_bps: 1_000,
        min_resolution_stake: 1_000,
    };

    /// Project the params into the flat [`OracleParams`] consumed by
    /// the oracle crate.
    pub fn oracle_params(self) -> OracleParams {
        OracleParams {
            alpha: self.alpha,
            resolver_slash_bps: self.resolver_slash_bps,
            min_resolution_stake: self.min_resolution_stake,
        }
    }

    /// Returns true iff every field passes a basic sanity check.
    pub fn is_well_formed(self) -> bool {
        self.fees.is_well_formed()
            && self.alpha.0 <= 10_000
            && self.default_resolution_window_blocks > 0
            && self.default_dispute_window_blocks > 0
            && self.default_liquidity_b > 0.0
            && self.adoption_threshold_bps <= 10_000
            && self.resolver_slash_bps <= 10_000
    }
}

/// What a passed proposal does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    /// Replace the governance params atomically.
    UpdateParams(GovernanceParams),
    /// Opaque upgrade hash for off-chain executable upgrades (binary
    /// rollout, contract migration, etc.). The runtime layer
    /// interprets this hash.
    Upgrade(Bytes32),
}

/// A governance proposal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// Submitter (creator of the forecast market and the proposal).
    pub proposer: Address,
    /// The forecast market that gates this proposal. Must be binary.
    pub market: MarketId,
    /// What this proposal changes.
    pub payload: Payload,
    /// Block at which the market window closes and the gate is checked.
    pub window_close_block: u64,
    /// True once executed; the proposal is consumed and the state
    /// transitions to `Executed`.
    pub executed: bool,
}

/// Why a proposal failed to execute. Returned by [`try_execute`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GovernanceError {
    /// The forecast market does not exist in the engine.
    MarketNotFound,
    /// The forecast market has not yet resolved.
    MarketPending,
    /// The forecast market resolved NO (or void).
    AdoptionThresholdNotMet,
    /// The forecast market resolved YES but did not accumulate enough
    /// volume to be considered binding.
    LiquidityThresholdNotMet,
    /// The proposal has already executed.
    AlreadyExecuted,
    /// The window close block hasn't yet arrived.
    WindowOpen,
    /// The gating market wasn't binary.
    NonBinaryGate,
}

/// Result of an attempted execution.
#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionOutcome {
    /// Executed; the runtime should apply the payload.
    Executed {
        /// The payload to apply.
        payload: Payload,
    },
    /// Rejected at the gate.
    Rejected {
        /// Why the proposal didn't pass.
        reason: GovernanceError,
    },
}

/// Inspect a proposal against the current engine state at block
/// `now` and decide whether it executes.
pub fn try_execute(
    proposal: &mut Proposal,
    engine: &Engine,
    params: &GovernanceParams,
    now: u64,
) -> ExecutionOutcome {
    if proposal.executed {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::AlreadyExecuted,
        };
    }
    if now < proposal.window_close_block {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::WindowOpen,
        };
    }
    let market = match engine.market(proposal.market) {
        None => {
            return ExecutionOutcome::Rejected {
                reason: GovernanceError::MarketNotFound,
            }
        }
        Some(m) => m,
    };
    if !market.kind.is_binary() {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::NonBinaryGate,
        };
    }
    if !market.is_terminal() {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::MarketPending,
        };
    }
    // Check that the market resolved YES.
    if market.resolved_outcome != Some(OutcomeId::YES) {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::AdoptionThresholdNotMet,
        };
    }
    // Volume gate.
    let amm = match engine.amm(proposal.market) {
        None => {
            return ExecutionOutcome::Rejected {
                reason: GovernanceError::MarketNotFound,
            }
        }
        Some(a) => a,
    };
    if amm.cumulative_fee_collected < params.min_market_volume {
        return ExecutionOutcome::Rejected {
            reason: GovernanceError::LiquidityThresholdNotMet,
        };
    }
    proposal.executed = true;
    ExecutionOutcome::Executed {
        payload: proposal.payload.clone(),
    }
}

/// Render the current YES probability of an upgrade market as it
/// stands. Convenience wrapper around [`Engine::get_live_probability`]
/// for callers that only want a single number.
pub fn live_adoption_probability(engine: &Engine, market: MarketId) -> Option<Probability> {
    use lace_pm_compose::ProbabilityFeed;
    engine.get_live_probability(market, OutcomeId::YES)
}

/// Build a fresh upgrade-gating market using the supplied governance
/// params. The market is binary YES/NO ("will the network adopt
/// this upgrade safely").
pub fn build_upgrade_market(
    id: MarketId,
    proposer: Address,
    question_hash: Bytes32,
    close_height: u64,
    params: &GovernanceParams,
) -> Result<(Market, LmsrState), lace_pm_markets::MarketError> {
    let m = Market::open(
        id,
        proposer,
        MarketKind::Binary,
        close_height,
        params.default_resolution_window_blocks,
        params.default_dispute_window_blocks,
        question_hash,
    )?;
    let amm = LmsrState::new(&MarketKind::Binary, params.default_liquidity_b);
    Ok((m, amm))
}

/// Index of executed proposal payloads by their executing block.
/// Useful for runtime layer audit.
pub type ExecutionLog = BTreeMap<u64, Payload>;

#[cfg(test)]
mod tests {
    use super::*;
    use lace_pm_types::FeeSchedule;

    fn b32(b: u8) -> Bytes32 {
        Bytes32([b; 32])
    }
    fn addr(b: u8) -> Address {
        Address(b32(b))
    }

    fn engine_with_resolved_market(yes: bool, volume: bool) -> (Engine, MarketId) {
        let mut e = Engine::new();
        let id = MarketId(b32(1));
        let (mkt, _amm) = build_upgrade_market(
            id,
            addr(2),
            b32(3),
            1_000,
            &GovernanceParams::DEFAULT,
        )
        .unwrap();
        e.register_market(mkt, GovernanceParams::DEFAULT.default_liquidity_b);
        if volume {
            // Push fees through the AMM.
            let amm = e.amm_mut(id).unwrap();
            amm.execute(0, 100_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        }
        let m = e.market_mut(id).unwrap();
        m.enter_resolution_window().unwrap();
        m.report_resolution(
            if yes { OutcomeId::YES } else { OutcomeId::NO },
            None,
        )
        .unwrap();
        m.finalize().unwrap();
        (e, id)
    }

    #[test]
    fn proposal_executes_when_yes_and_volume_clear() {
        let (engine, id) = engine_with_resolved_market(true, true);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 100,
            executed: false,
        };
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        assert!(
            matches!(outcome, ExecutionOutcome::Executed { .. }),
            "expected execution, got {:?}",
            outcome
        );
        assert!(p.executed);
    }

    #[test]
    fn proposal_rejected_when_market_resolves_no() {
        let (engine, id) = engine_with_resolved_market(false, true);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 100,
            executed: false,
        };
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        assert_eq!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: GovernanceError::AdoptionThresholdNotMet
            }
        );
        assert!(!p.executed);
    }

    #[test]
    fn proposal_rejected_when_volume_too_low() {
        let (engine, id) = engine_with_resolved_market(true, false);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 100,
            executed: false,
        };
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        assert_eq!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: GovernanceError::LiquidityThresholdNotMet
            }
        );
    }

    #[test]
    fn proposal_rejected_before_window_close() {
        let (engine, id) = engine_with_resolved_market(true, true);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 1_000,
            executed: false,
        };
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 500);
        assert_eq!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: GovernanceError::WindowOpen
            }
        );
    }

    #[test]
    fn double_execution_rejected() {
        let (engine, id) = engine_with_resolved_market(true, true);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 100,
            executed: false,
        };
        let _ = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        assert_eq!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: GovernanceError::AlreadyExecuted
            }
        );
    }

    #[test]
    fn parameter_governance_updates_params_atomically() {
        let (engine, id) = engine_with_resolved_market(true, true);
        let new_params = GovernanceParams {
            adoption_threshold_bps: 8_000,
            ..GovernanceParams::DEFAULT
        };
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::UpdateParams(new_params),
            window_close_block: 100,
            executed: false,
        };
        let outcome = try_execute(&mut p, &engine, &GovernanceParams::DEFAULT, 200);
        match outcome {
            ExecutionOutcome::Executed { payload: Payload::UpdateParams(applied) } => {
                assert_eq!(applied.adoption_threshold_bps, 8_000);
            }
            other => panic!("expected param update, got {:?}", other),
        }
    }

    #[test]
    fn governance_params_default_is_well_formed() {
        assert!(GovernanceParams::DEFAULT.is_well_formed());
    }

    #[test]
    fn non_binary_gate_rejected() {
        let mut e = Engine::new();
        let id = MarketId(b32(1));
        let m = Market::open(
            id,
            addr(2),
            MarketKind::MultiOutcome {
                outcomes: vec![OutcomeId(b32(10)), OutcomeId(b32(11)), OutcomeId(b32(12))],
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        e.register_market(m, 100.0);
        let mut p = Proposal {
            proposer: addr(2),
            market: id,
            payload: Payload::Upgrade(b32(99)),
            window_close_block: 100,
            executed: false,
        };
        let outcome = try_execute(&mut p, &e, &GovernanceParams::DEFAULT, 200);
        assert_eq!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: GovernanceError::NonBinaryGate
            }
        );
    }
}
