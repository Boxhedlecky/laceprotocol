//! End-to-end integration tests that compose the VM, conditions, and
//! contracts crates together. The unit tests in each crate exercise
//! one layer; these tests prove the layers compose without dropping
//! invariants on the seams between them.

use lace_conditions::{Condition, ExternalCond, OracleAnswer, OracleResolver, TimeCond};
use lace_contracts::escrow::{Escrow, EscrowConfig, EscrowState};
use lace_contracts::milestone::{Milestone, MilestoneConfig, Stage};
use lace_contracts::recurring::{RecurringConfig, RecurringPayment};
use lace_contracts::{Address, ContractError};
use lace_time::{Duration, Interval, ManualClock, Timestamp};
use lace_vm::executor::Executor;
use lace_vm::opcode::{Op, Program};
use lace_vm::value::Value;
use lace_vm::Bytes32;
use std::collections::HashMap;

fn addr(b: u8) -> Address {
    let mut x = [0u8; 32];
    x[0] = b;
    Bytes32(x)
}

struct StaticOracles(HashMap<Bytes32, OracleAnswer>);
impl OracleResolver for StaticOracles {
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer {
        self.0.get(oracle).copied().unwrap_or(OracleAnswer::Pending)
    }
}

#[test]
fn timestamp_cannot_be_manipulated_backwards() {
    // Validators cannot move time backwards inside the VM; the
    // executor consults Clock::now and trusts what consensus
    // supplied. We model the attack by having the contract call
    // advance() twice at the same clock value; no extra payouts
    // can be siphoned just by replaying the call.
    let mut r = RecurringPayment::new(RecurringConfig {
        payer: addr(1),
        payee: addr(2),
        amount_per_tick: 50,
        interval: Duration::from_secs(60),
        window: Interval::new(Timestamp::from_secs(0), Timestamp::from_secs(600)),
    })
    .unwrap();
    r.fund(1_000).unwrap();
    let clock = ManualClock::at(Timestamp::from_secs(180));
    // First call should pay 3 ticks (t=0, 60, 120 all < 180).
    let p1 = r.advance(&clock);
    assert_eq!(p1.len(), 3);
    // Replaying with the same clock pays nothing: the contract
    // tracks processed ticks, not wall time. A malicious validator
    // would need to *advance* time to extract additional payouts.
    let p2 = r.advance(&clock);
    assert!(p2.is_empty());
}

#[test]
fn deadline_attack_is_a_hard_revert_not_a_soft_skip() {
    // A user trying to sneak a transaction in after a deadline must
    // see a distinct error so the explorer can label the failure
    // honestly. GuardFailed is a recoverable retry; DeadlineExceeded
    // is a terminal revert. We pin that contract here.
    let mut p = Program::new();
    p.time(Timestamp::from_secs(1_000)).push(Op::Deadline);
    let clock = ManualClock::at(Timestamp::from_secs(1_001));
    let err = Executor::new(&clock).run(&p).unwrap_err();
    match err {
        lace_vm::VmError::DeadlineExceeded { deadline, now } => {
            assert_eq!(deadline.as_secs(), 1_000);
            assert_eq!(now.as_secs(), 1_001);
        }
        other => panic!("expected DeadlineExceeded, got {:?}", other),
    }
}

#[test]
fn simultaneous_confirm_and_abort_resolves_deterministically() {
    // Edge case: parties race to confirm and abort. Whichever
    // transition is observed first by consensus wins; the contract
    // state machine rejects the second transition cleanly with
    // InvalidState rather than corrupting itself.
    let mut e = Escrow::new(EscrowConfig {
        buyer: addr(1),
        seller: addr(2),
        buyer_deposit: 100,
        seller_bond: 0,
        abort_deadline: Timestamp::from_secs(1_000),
    });
    e.fund(addr(1)).unwrap();
    e.fund(addr(2)).unwrap();
    e.confirm(addr(1)).unwrap();
    e.confirm(addr(2)).unwrap();
    assert_eq!(e.state, EscrowState::Released);
    let clock = ManualClock::at(Timestamp::from_secs(500));
    // Late abort against a Released escrow is rejected.
    let err = e.request_abort(addr(1), &clock).unwrap_err();
    assert!(matches!(err, ContractError::InvalidState(_)));
}

#[test]
fn milestone_with_time_and_oracle_conditions_composes() {
    // A milestone where stage 1 is time-only and stage 2 is
    // (time AND oracle) verifies the conditions crate composes
    // correctly when consumed by the contracts crate.
    let oracle = addr(50);
    let expected = addr(51);
    let mut m = Milestone::new(MilestoneConfig {
        payer: addr(1),
        payee: addr(2),
        stages: vec![
            Stage {
                amount: 40,
                condition: Condition::Time(TimeCond::After(Timestamp::from_secs(100))),
            },
            Stage {
                amount: 60,
                condition: Condition::And(
                    Box::new(Condition::Time(TimeCond::After(Timestamp::from_secs(200)))),
                    Box::new(Condition::External(ExternalCond {
                        oracle,
                        expected,
                    })),
                ),
            },
        ],
        deposit: 100,
    })
    .unwrap();
    let mut oracles = StaticOracles(HashMap::new());
    let mut clock = ManualClock::at(Timestamp::from_secs(150));
    let p = m.advance(&clock, &oracles);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].amount, 40);
    // Time has passed but oracle is pending -> no further payouts.
    clock.set(Timestamp::from_secs(250));
    let p = m.advance(&clock, &oracles);
    assert!(p.is_empty());
    // Oracle resolves -> remainder releases.
    oracles
        .0
        .insert(oracle, OracleAnswer::Resolved(expected));
    let p = m.advance(&clock, &oracles);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].amount, 60);
}

#[test]
fn recurring_window_caps_tick_count_against_grief() {
    // Grief vector: an attacker calls advance() far in the future
    // hoping to provoke unbounded iteration. The schedule's
    // ticks_before is capped at window.end, so the contract does
    // at most (window_duration / interval) work regardless of how
    // far the clock has drifted.
    let mut r = RecurringPayment::new(RecurringConfig {
        payer: addr(1),
        payee: addr(2),
        amount_per_tick: 1,
        interval: Duration::from_secs(60),
        window: Interval::new(Timestamp::from_secs(0), Timestamp::from_secs(600)),
    })
    .unwrap();
    r.fund(10_000).unwrap();
    let clock = ManualClock::at(Timestamp::from_secs(10_000_000));
    let p = r.advance(&clock);
    // Exactly window_duration / interval = 600 / 60 = 10 ticks.
    // Not 10_000_000 / 60.
    assert_eq!(p.len(), 10);
}

#[test]
fn vm_program_composes_with_time_window_check() {
    // Build a program that asserts "now is inside [start, end)" by
    // combining AFTER and BEFORE, push a sentinel bool, and check
    // the executor leaves the bool on the stack. This is the
    // protocol-level "AND of time conditions" pattern.
    let mut p = Program::new();
    p.time(Timestamp::from_secs(1_000))
        .push(Op::After)
        .time(Timestamp::from_secs(2_000))
        .push(Op::Before)
        .literal(Value::Bool(true));
    let clock = ManualClock::at(Timestamp::from_secs(1_500));
    let out = Executor::new(&clock).run(&p).unwrap();
    assert_eq!(out.stack, vec![Value::Bool(true)]);
}
