//! Milestone contract.
//!
//! A pre-funded escrow split into N ordered stages, each gated by a
//! [`Condition`] and carrying an amount that releases to the payee
//! when the condition resolves to `Ready`. Stages are processed in
//! declaration order; a `Failed` stage halts the contract and routes
//! the *remainder* (unreleased balance) to the payer, unless a
//! dispute is opened first.
//!
//! Each stage can mix time and external (prediction-market) conditions
//! freely; the same `Condition` algebra that the conditions crate
//! exposes is what gates each release.

use lace_conditions::{Condition, OracleResolver, Resolution};
use lace_time::Clock;
use lace_vm::Bytes32;
use serde::{Deserialize, Serialize};

use crate::{Address, Amount, ContractError, Payout, PayoutReason};

/// A single milestone definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    /// Amount released when this milestone fires.
    pub amount: Amount,
    /// Gating condition. May be time-only, oracle-only, or a mix.
    pub condition: Condition,
}

/// Lifecycle state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MilestoneState {
    /// Actively processing stages in order.
    Active,
    /// All stages released.
    Completed,
    /// A stage condition resolved to `Failed`; remainder refunded.
    Failed,
    /// Settlement deferred to dispute oracle.
    Disputed,
}

/// Milestone configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneConfig {
    /// Payer (funds the contract, receives any refund).
    pub payer: Address,
    /// Payee (receives milestone releases).
    pub payee: Address,
    /// Stages in order. Sum of `amount` must equal `deposit`.
    pub stages: Vec<Stage>,
    /// Total funds locked.
    pub deposit: Amount,
}

/// Milestone instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    /// Static config.
    pub config: MilestoneConfig,
    /// Lifecycle state.
    pub state: MilestoneState,
    /// Index of the next stage to evaluate.
    pub next_stage: usize,
    /// Remaining balance.
    pub remaining: Amount,
    /// Dispute oracle, if any.
    pub dispute_oracle: Option<Bytes32>,
    /// Outcome that, if oracle returns it, releases the remaining
    /// balance to the payee. Any other outcome refunds the payer.
    pub dispute_release_outcome: Option<Bytes32>,
}

impl Milestone {
    /// Construct, validating that stage amounts sum to the deposit.
    pub fn new(config: MilestoneConfig) -> Result<Self, ContractError> {
        if config.stages.is_empty() {
            return Err(ContractError::BadConfig("milestone has no stages"));
        }
        let sum: Option<Amount> = config
            .stages
            .iter()
            .try_fold(0u128, |acc, s| acc.checked_add(s.amount));
        match sum {
            Some(s) if s == config.deposit => {}
            Some(_) => return Err(ContractError::BadConfig("stage amounts != deposit")),
            None => return Err(ContractError::AmountOverflow),
        }
        let remaining = config.deposit;
        Ok(Self {
            config,
            state: MilestoneState::Active,
            next_stage: 0,
            remaining,
            dispute_oracle: None,
            dispute_release_outcome: None,
        })
    }

    /// Try to release as many ready stages as possible against the
    /// current clock and oracle state. Stops at the first stage that
    /// is `Pending` (and stays `Active`) or `Failed` (and transitions
    /// to `Failed`).
    pub fn advance(
        &mut self,
        clock: &dyn Clock,
        oracles: &dyn OracleResolver,
    ) -> Vec<Payout> {
        if self.state != MilestoneState::Active {
            return Vec::new();
        }
        let mut payouts = Vec::new();
        while self.next_stage < self.config.stages.len() {
            let stage = &self.config.stages[self.next_stage];
            match stage.condition.resolve(clock, oracles) {
                Resolution::Ready => {
                    self.remaining = self.remaining.saturating_sub(stage.amount);
                    payouts.push(Payout {
                        to: self.config.payee,
                        amount: stage.amount,
                        reason: PayoutReason::MilestoneRelease,
                    });
                    self.next_stage += 1;
                }
                Resolution::Pending => break,
                Resolution::Failed => {
                    // Failure halts the contract. Remaining funds
                    // refund to the payer, *not* the payee.
                    if self.remaining > 0 {
                        payouts.push(Payout {
                            to: self.config.payer,
                            amount: self.remaining,
                            reason: PayoutReason::EscrowRefund,
                        });
                        self.remaining = 0;
                    }
                    self.state = MilestoneState::Failed;
                    return payouts;
                }
            }
        }
        if self.next_stage >= self.config.stages.len() {
            self.state = MilestoneState::Completed;
        }
        payouts
    }

    /// Open a dispute, deferring settlement to an oracle outcome.
    pub fn open_dispute(
        &mut self,
        party: Address,
        oracle: Bytes32,
        release_outcome: Bytes32,
    ) -> Result<(), ContractError> {
        if self.state != MilestoneState::Active {
            return Err(ContractError::InvalidState("milestone not Active"));
        }
        if party != self.config.payer && party != self.config.payee {
            return Err(ContractError::UnauthorisedParty);
        }
        self.state = MilestoneState::Disputed;
        self.dispute_oracle = Some(oracle);
        self.dispute_release_outcome = Some(release_outcome);
        Ok(())
    }

    /// Settle a dispute. The whole remaining balance moves either to
    /// the payee or back to the payer based on the oracle outcome.
    pub fn settle_dispute(&mut self, actual: Bytes32) -> Result<Vec<Payout>, ContractError> {
        if self.state != MilestoneState::Disputed {
            return Err(ContractError::InvalidState("milestone not Disputed"));
        }
        let release = self
            .dispute_release_outcome
            .ok_or(ContractError::InvalidState("missing dispute outcome"))?;
        let to = if actual == release {
            self.config.payee
        } else {
            self.config.payer
        };
        let amount = self.remaining;
        self.remaining = 0;
        self.state = MilestoneState::Completed;
        Ok(vec![Payout {
            to,
            amount,
            reason: PayoutReason::EscrowDisputed,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_conditions::{ExternalCond, PendingResolver, TimeCond};
    use lace_time::{ManualClock, Timestamp};
    use std::collections::HashMap;

    fn addr(b: u8) -> Address {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    struct MockOracles(HashMap<Bytes32, lace_conditions::OracleAnswer>);
    impl OracleResolver for MockOracles {
        fn answer(&self, k: &Bytes32) -> lace_conditions::OracleAnswer {
            self.0
                .get(k)
                .copied()
                .unwrap_or(lace_conditions::OracleAnswer::Pending)
        }
    }

    fn stage_after(ts: u64, amount: Amount) -> Stage {
        Stage {
            amount,
            condition: Condition::Time(TimeCond::After(Timestamp::from_secs(ts))),
        }
    }

    fn fresh() -> Milestone {
        Milestone::new(MilestoneConfig {
            payer: addr(1),
            payee: addr(2),
            stages: vec![stage_after(100, 30), stage_after(200, 70)],
            deposit: 100,
        })
        .unwrap()
    }

    #[test]
    fn rejects_misconfigured_stage_sum() {
        let bad = MilestoneConfig {
            payer: addr(1),
            payee: addr(2),
            stages: vec![stage_after(100, 30), stage_after(200, 80)],
            deposit: 100,
        };
        assert!(matches!(
            Milestone::new(bad).unwrap_err(),
            ContractError::BadConfig(_)
        ));
    }

    #[test]
    fn stages_release_in_order() {
        let mut m = fresh();
        let mut clock = ManualClock::at(Timestamp::from_secs(50));
        assert!(m.advance(&clock, &PendingResolver).is_empty());
        clock.set(Timestamp::from_secs(150));
        let p = m.advance(&clock, &PendingResolver);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].amount, 30);
        clock.set(Timestamp::from_secs(250));
        let p = m.advance(&clock, &PendingResolver);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].amount, 70);
        assert_eq!(m.state, MilestoneState::Completed);
    }

    #[test]
    fn failed_stage_refunds_remainder() {
        let mut m = Milestone::new(MilestoneConfig {
            payer: addr(1),
            payee: addr(2),
            stages: vec![
                stage_after(100, 30),
                Stage {
                    amount: 70,
                    condition: Condition::External(ExternalCond {
                        oracle: addr(50),
                        expected: addr(60),
                    }),
                },
            ],
            deposit: 100,
        })
        .unwrap();
        let clock = ManualClock::at(Timestamp::from_secs(150));
        let mut oracles = MockOracles(HashMap::new());
        // First stage fires, second is pending oracle.
        let p = m.advance(&clock, &oracles);
        assert_eq!(p.len(), 1);
        // Oracle resolves to a non-expected outcome -> Failed.
        oracles
            .0
            .insert(addr(50), lace_conditions::OracleAnswer::Resolved(addr(99)));
        let p = m.advance(&clock, &oracles);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].to, addr(1));
        assert_eq!(p[0].amount, 70);
        assert_eq!(m.state, MilestoneState::Failed);
    }
}
