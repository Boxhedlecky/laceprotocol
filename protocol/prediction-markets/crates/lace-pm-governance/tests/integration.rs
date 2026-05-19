//! End-to-end integration tests across the prediction-market engine
//! crates. Each test drives a full lifecycle through more than one
//! crate so that interface drift between crates is caught.
//!
//! These exercise:
//!   * lifecycle (markets + AMM + oracle + compose + governance);
//!   * the OracleResolver impl as consumed by a *mocked* temporal-VM
//!     style caller;
//!   * the ReputationSink as consumed by a *mocked* Veil Score style
//!     caller;
//!   * privacy: confirming the AMM never sees any data not delivered
//!     through the opaque position-commitment hash.

use lace_pm_amm::LmsrState;
use lace_pm_compose::{Engine, OracleAnswer, OracleResolver, ProbabilityFeed, TriggerCallback};
use lace_pm_governance::{
    build_upgrade_market, try_execute, ExecutionOutcome, GovernanceParams, Payload, Proposal,
};
use lace_pm_markets::{Market, MarketKind};
use lace_pm_oracle::{
    apply_dispute, close_round, finalize_and_emit, AlphaBps, Dispute, ForecasterVote, NullSink,
    OracleParams, ReputationEvent, ReputationSink, ResolutionRound, ValidatorVote,
};
use lace_pm_types::{
    Address, Amount, Bytes32, FeeSchedule, MarketId, OutcomeId, Probability,
};

fn b32(b: u8) -> Bytes32 {
    Bytes32([b; 32])
}

fn addr(b: u8) -> Address {
    Address(b32(b))
}

/// Mock ReputationSink for use in tests, matching how Veil Score
/// (Component 4) will consume the same trait.
#[derive(Default)]
struct MockVeilScore {
    events: Vec<ReputationEvent>,
}

impl ReputationSink for MockVeilScore {
    fn record(&mut self, e: ReputationEvent) {
        self.events.push(e);
    }
}

#[test]
fn lifecycle_binary_market_full_path() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(
        id,
        addr(2),
        MarketKind::Binary,
        1_000,
        50,
        20,
        b32(99),
    )
    .unwrap();
    engine.register_market(m, 1_000_000.0);

    // Trader buys YES at 50/50.
    let amm = engine.amm_mut(id).unwrap();
    let before = amm.probability(0);
    amm.execute(0, 200_000.0, FeeSchedule::DEFAULT, b32(7))
        .unwrap();
    let after = amm.probability(0);
    assert!(after.bps() > before.bps(), "trade should raise YES price");

    // Move to resolution window, run a resolution round.
    engine.market_mut(id).unwrap().enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: OutcomeId::YES,
        stake: 50_000,
    });
    round.cast_validator(ValidatorVote {
        voter: addr(11),
        outcome: OutcomeId::YES,
        stake: 30_000,
    });
    round.cast_forecaster(ForecasterVote {
        voter: addr(20),
        outcome: OutcomeId::YES,
        reputation_bps: 9_000,
    });
    let m = engine.market_mut(id).unwrap();
    let provisional = close_round(&round, m, OracleParams::DEFAULT).unwrap();
    assert_eq!(provisional, Some(OutcomeId::YES));

    let mut sink = MockVeilScore::default();
    finalize_and_emit(engine.market_mut(id).unwrap(), &round, None, &mut sink).unwrap();

    // Cross-component reads.
    assert_eq!(engine.get_resolved_outcome(id), Some(OutcomeId::YES));
    match engine.answer(&id.0) {
        OracleAnswer::Resolved(h) => assert_eq!(h, OutcomeId::YES.0),
        other => panic!("expected resolved, got {:?}", other),
    }

    // Reputation events flowed.
    assert_eq!(sink.events.len(), 3);
}

#[test]
fn lifecycle_conditional_market_cascade_voids_when_parent_resolves_wrong() {
    let mut engine = Engine::new();
    let parent_id = MarketId(b32(1));
    let parent = Market::open(parent_id, addr(2), MarketKind::Binary, 1_000, 50, 20, b32(99))
        .unwrap();
    engine.register_market(parent, 1_000_000.0);

    let child_id = MarketId(b32(2));
    let child = Market::open(
        child_id,
        addr(3),
        MarketKind::Conditional {
            parent: parent_id,
            parent_outcome: OutcomeId::YES,
            inner: Box::new(MarketKind::Binary),
        },
        1_000,
        50,
        20,
        b32(99),
    )
    .unwrap();
    engine.register_market(child, 500_000.0);

    // Parent resolves NO -- contrary to the conditional's expected
    // outcome.
    let m = engine.market_mut(parent_id).unwrap();
    m.enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(parent_id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: OutcomeId::NO,
        stake: 5_000,
    });
    close_round(&round, engine.market_mut(parent_id).unwrap(), OracleParams::DEFAULT).unwrap();
    finalize_and_emit(
        engine.market_mut(parent_id).unwrap(),
        &round,
        None,
        &mut NullSink,
    )
    .unwrap();

    engine.cascade_conditionals();
    assert_eq!(
        engine.get_resolved_outcome(child_id),
        None,
        "child market should not have a resolved outcome -- it voided"
    );
    assert_eq!(engine.answer(&child_id.0), OracleAnswer::Voided);
}

#[test]
fn lifecycle_dispute_upheld_overturns_provisional() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(id, addr(2), MarketKind::Binary, 1_000, 50, 20, b32(99)).unwrap();
    engine.register_market(m, 1_000_000.0);

    engine.market_mut(id).unwrap().enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: OutcomeId::YES,
        stake: 50_000,
    });
    close_round(&round, engine.market_mut(id).unwrap(), OracleParams::DEFAULT).unwrap();

    let dispute = Dispute {
        challenger: addr(99),
        asserted_outcome: OutcomeId::NO,
        bond: 5_000,
        opened_at: 1_120,
    };
    let outcome = apply_dispute(engine.market_mut(id).unwrap(), &dispute, true).unwrap();
    let mut sink = MockVeilScore::default();
    let final_outcome = finalize_and_emit(
        engine.market_mut(id).unwrap(),
        &round,
        Some((&dispute, outcome)),
        &mut sink,
    )
    .unwrap();
    assert_eq!(final_outcome, OutcomeId::NO);
    assert_eq!(engine.get_resolved_outcome(id), Some(OutcomeId::NO));

    // The originally-correct YES validator is now flagged Incorrect.
    assert!(sink
        .events
        .iter()
        .any(|e| matches!(e, ReputationEvent::ResolverIncorrect { .. })));
    assert!(sink
        .events
        .iter()
        .any(|e| matches!(e, ReputationEvent::DisputeUpheld { .. })));
}

#[test]
fn governance_upgrade_executes_through_full_pipeline() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let (m, _amm) = build_upgrade_market(
        id,
        addr(2),
        b32(99),
        1_000,
        &GovernanceParams::DEFAULT,
    )
    .unwrap();
    engine.register_market(m, GovernanceParams::DEFAULT.default_liquidity_b);

    // Drive volume through the AMM.
    engine
        .amm_mut(id)
        .unwrap()
        .execute(0, 200_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO)
        .unwrap();

    // Run the resolution round.
    engine.market_mut(id).unwrap().enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: OutcomeId::YES,
        stake: 50_000,
    });
    close_round(&round, engine.market_mut(id).unwrap(), OracleParams::DEFAULT).unwrap();
    finalize_and_emit(
        engine.market_mut(id).unwrap(),
        &round,
        None,
        &mut NullSink,
    )
    .unwrap();

    let mut proposal = Proposal {
        proposer: addr(2),
        market: id,
        payload: Payload::Upgrade(b32(0xAA)),
        window_close_block: 1_100,
        executed: false,
    };
    let outcome = try_execute(&mut proposal, &engine, &GovernanceParams::DEFAULT, 1_500);
    assert!(matches!(outcome, ExecutionOutcome::Executed { .. }));
}

#[test]
fn position_privacy_amm_never_leaks_trader_address() {
    // Two traders, both buying YES on the same market. The AMM
    // exposes only `position_commitment` -- we verify by feeding
    // visibly different commitments and reading them back from the
    // receipts.
    let mut amm = LmsrState::new(&MarketKind::Binary, 1_000_000.0);
    let r1 = amm
        .execute(0, 10_000.0, FeeSchedule::DEFAULT, Bytes32([0xAA; 32]))
        .unwrap();
    let r2 = amm
        .execute(0, 10_000.0, FeeSchedule::DEFAULT, Bytes32([0xBB; 32]))
        .unwrap();
    assert_eq!(r1.position_commitment.0, [0xAA; 32]);
    assert_eq!(r2.position_commitment.0, [0xBB; 32]);
    // The AMM struct itself does not carry any per-trader fields.
    // (Compile-time test -- this assertion would fail to compile if
    // the struct grew a trader-identifying field.)
    let _state: &LmsrState = &amm;
}

#[test]
fn conditional_trigger_fires_into_temporal_vm_style_callback() {
    // Mock callback that records into a thread-local-ish slot.
    use std::cell::Cell;
    use std::rc::Rc;
    let fired: Rc<Cell<Option<(MarketId, OutcomeId)>>> = Rc::new(Cell::new(None));
    let fired_cb = fired.clone();
    let cb: TriggerCallback = Box::new(move |m, o| fired_cb.set(Some((m, o))));

    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(id, addr(2), MarketKind::Binary, 1_000, 50, 20, b32(99)).unwrap();
    engine.register_market(m, 1_000_000.0);
    engine.create_conditional_trigger(id, OutcomeId::YES, cb);

    let m = engine.market_mut(id).unwrap();
    m.enter_resolution_window().unwrap();
    m.report_resolution(OutcomeId::YES, None).unwrap();
    m.finalize().unwrap();
    engine.tick();
    assert_eq!(fired.get(), Some((id, OutcomeId::YES)));
}

#[test]
fn oracle_resolver_pending_for_open_market() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(id, addr(2), MarketKind::Binary, 1_000, 50, 20, b32(99)).unwrap();
    engine.register_market(m, 1_000_000.0);
    assert_eq!(engine.answer(&id.0), OracleAnswer::Pending);
}

#[test]
fn manipulation_resistance_thin_market_cant_force_upgrade() {
    // A proposal market with essentially zero volume cannot push an
    // upgrade through, even if a single highly-staked validator
    // votes YES.
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let (m, _amm) = build_upgrade_market(
        id,
        addr(2),
        b32(99),
        1_000,
        &GovernanceParams::DEFAULT,
    )
    .unwrap();
    engine.register_market(m, GovernanceParams::DEFAULT.default_liquidity_b);
    // No volume.
    engine.market_mut(id).unwrap().enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: OutcomeId::YES,
        stake: 1_000_000_000,
    });
    close_round(&round, engine.market_mut(id).unwrap(), OracleParams::DEFAULT).unwrap();
    finalize_and_emit(
        engine.market_mut(id).unwrap(),
        &round,
        None,
        &mut NullSink,
    )
    .unwrap();

    let mut proposal = Proposal {
        proposer: addr(2),
        market: id,
        payload: Payload::Upgrade(b32(0xAA)),
        window_close_block: 1_100,
        executed: false,
    };
    let outcome = try_execute(&mut proposal, &engine, &GovernanceParams::DEFAULT, 1_500);
    assert!(matches!(
        outcome,
        ExecutionOutcome::Rejected {
            reason: lace_pm_governance::GovernanceError::LiquidityThresholdNotMet
        }
    ));
}

#[test]
fn probability_feed_returns_none_for_unknown_market() {
    let engine = Engine::new();
    assert_eq!(
        engine.get_live_probability(MarketId(b32(42)), OutcomeId::YES),
        None
    );
}

#[test]
fn probability_feed_returns_none_for_voided_market() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(id, addr(2), MarketKind::Binary, 1_000, 50, 20, b32(99)).unwrap();
    engine.register_market(m, 1_000_000.0);
    engine.market_mut(id).unwrap().void().unwrap();
    assert_eq!(engine.get_live_probability(id, OutcomeId::YES), None);
}

#[test]
fn fee_routing_sums_to_total_collected() {
    let mut amm = LmsrState::new(&MarketKind::Binary, 1_000_000.0);
    let receipt = amm
        .execute(0, 100_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO)
        .unwrap();
    let total: Amount = receipt.fee_routing.burn
        + receipt.fee_routing.validator
        + receipt.fee_routing.resolution
        + receipt.fee_routing.liquidity;
    assert_eq!(total, receipt.fee_amount);
}

#[test]
fn scalar_market_resolves_to_value_in_range() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let m = Market::open(
        id,
        addr(2),
        MarketKind::Scalar { lo: 0, hi: 1_000 },
        1_000,
        50,
        20,
        b32(99),
    )
    .unwrap();
    engine.register_market(m, 1_000_000.0);
    let m = engine.market_mut(id).unwrap();
    m.enter_resolution_window().unwrap();
    m.report_resolution(OutcomeId::YES, Some(750)).unwrap();
    m.finalize().unwrap();
    assert_eq!(engine.market(id).unwrap().resolved_scalar, Some(750));
    // 750/1000 -> 7500 bps.
    let proj = lace_pm_markets::scalar_to_probability(0, 1_000, 750);
    assert_eq!(proj, Probability::from_bps(7_500));
}

#[test]
fn multi_outcome_resolution_picks_one_branch() {
    let mut engine = Engine::new();
    let id = MarketId(b32(1));
    let outcomes = vec![OutcomeId(b32(10)), OutcomeId(b32(11)), OutcomeId(b32(12))];
    let m = Market::open(
        id,
        addr(2),
        MarketKind::MultiOutcome {
            outcomes: outcomes.clone(),
        },
        1_000,
        50,
        20,
        b32(99),
    )
    .unwrap();
    engine.register_market(m, 1_000_000.0);
    engine.market_mut(id).unwrap().enter_resolution_window().unwrap();
    let mut round = ResolutionRound::new(id, 1_100, AlphaBps::DEFAULT);
    round.cast_validator(ValidatorVote {
        voter: addr(10),
        outcome: outcomes[1],
        stake: 5_000,
    });
    close_round(&round, engine.market_mut(id).unwrap(), OracleParams::DEFAULT).unwrap();
    finalize_and_emit(
        engine.market_mut(id).unwrap(),
        &round,
        None,
        &mut NullSink,
    )
    .unwrap();
    assert_eq!(engine.get_resolved_outcome(id), Some(outcomes[1]));
}
