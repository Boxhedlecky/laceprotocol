//! Dead man's switch.
//!
//! Holds assets that auto-transfer to a list of beneficiaries if the
//! owner's wallet does not produce a [`DeadMan::heartbeat`] within a
//! configurable inactivity threshold. The classic use case is
//! inheritance, but the same mechanism powers "transfer to recovery
//! address if I lose my keys" flows.
//!
//! Beneficiaries can be multi-party with weighted shares. Shares are
//! expressed in arbitrary `u32` units; the contract normalises them
//! so the *sum* of payouts equals the deposit (or as close as integer
//! division allows, with the rounding residue going to the first
//! beneficiary -- a small but deliberate choice to keep the contract
//! deterministic).

use lace_time::{Clock, Duration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{Address, Amount, ContractError, Payout, PayoutReason};

/// A single beneficiary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beneficiary {
    /// Destination address.
    pub address: Address,
    /// Weight in arbitrary units. Final share = `weight / sum_of_weights`.
    pub weight: u32,
}

/// Dead-man configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadManConfig {
    /// Owner address (the only address that can heartbeat).
    pub owner: Address,
    /// Inactivity threshold after which the switch is armed.
    pub threshold: Duration,
    /// Beneficiaries with weights. Must be non-empty and have at
    /// least one nonzero weight.
    pub beneficiaries: Vec<Beneficiary>,
    /// Funds held by the switch.
    pub deposit: Amount,
}

/// Dead-man lifecycle.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadManState {
    /// Owner is alive (heartbeats are landing inside the threshold).
    Armed,
    /// Threshold elapsed; transfer has fired.
    Triggered,
}

/// Dead-man instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadMan {
    /// Static config.
    pub config: DeadManConfig,
    /// Lifecycle state.
    pub state: DeadManState,
    /// Last accepted heartbeat (initialised to contract creation time).
    pub last_heartbeat: Timestamp,
}

impl DeadMan {
    /// Construct, validating beneficiaries and creating a heartbeat
    /// timestamp at "now".
    pub fn new(config: DeadManConfig, clock: &dyn Clock) -> Result<Self, ContractError> {
        if config.beneficiaries.is_empty() {
            return Err(ContractError::BadConfig("no beneficiaries"));
        }
        let total_weight: u64 = config
            .beneficiaries
            .iter()
            .map(|b| b.weight as u64)
            .sum::<u64>();
        if total_weight == 0 {
            return Err(ContractError::BadConfig("beneficiary weights sum to zero"));
        }
        if config.threshold.is_zero() {
            return Err(ContractError::BadConfig("threshold must be > 0"));
        }
        Ok(Self {
            config,
            state: DeadManState::Armed,
            last_heartbeat: clock.now(),
        })
    }

    /// Reset the inactivity clock.
    pub fn heartbeat(&mut self, party: Address, clock: &dyn Clock) -> Result<(), ContractError> {
        if party != self.config.owner {
            return Err(ContractError::UnauthorisedParty);
        }
        if self.state != DeadManState::Armed {
            return Err(ContractError::InvalidState("dead-man already Triggered"));
        }
        self.last_heartbeat = clock.now();
        Ok(())
    }

    /// Check whether the threshold has expired and, if so, emit
    /// payouts to all beneficiaries and transition to `Triggered`.
    /// Callable by anyone (the chain itself drives this at every
    /// block in the integration layer, but we want manual triggering
    /// to be a no-privilege operation so a beneficiary can poke the
    /// contract without depending on chain-driven evaluation).
    pub fn try_trigger(&mut self, clock: &dyn Clock) -> Vec<Payout> {
        if self.state != DeadManState::Armed {
            return Vec::new();
        }
        let deadline = self.last_heartbeat.saturating_add(self.config.threshold);
        if clock.now() <= deadline {
            return Vec::new();
        }
        let total_weight: u64 = self
            .config
            .beneficiaries
            .iter()
            .map(|b| b.weight as u64)
            .sum();
        let mut payouts: Vec<Payout> = Vec::with_capacity(self.config.beneficiaries.len());
        let mut distributed: Amount = 0;
        for (idx, b) in self.config.beneficiaries.iter().enumerate() {
            let share: Amount = if idx + 1 == self.config.beneficiaries.len() {
                // Last beneficiary absorbs any rounding residue so
                // the total distributed equals the deposit exactly.
                self.config.deposit.saturating_sub(distributed)
            } else {
                self.config
                    .deposit
                    .saturating_mul(b.weight as Amount)
                    / total_weight as Amount
            };
            distributed = distributed.saturating_add(share);
            if share > 0 {
                payouts.push(Payout {
                    to: b.address,
                    amount: share,
                    reason: PayoutReason::Inheritance,
                });
            }
        }
        self.state = DeadManState::Triggered;
        payouts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_time::ManualClock;
    use lace_vm::Bytes32;

    fn addr(b: u8) -> Address {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    fn fresh(clock: &dyn Clock) -> DeadMan {
        DeadMan::new(
            DeadManConfig {
                owner: addr(1),
                threshold: Duration::from_secs(3_600),
                beneficiaries: vec![
                    Beneficiary { address: addr(2), weight: 1 },
                    Beneficiary { address: addr(3), weight: 1 },
                ],
                deposit: 100,
            },
            clock,
        )
        .unwrap()
    }

    #[test]
    fn heartbeat_postpones_trigger() {
        let mut clock = ManualClock::at(Timestamp::from_secs(0));
        let mut d = fresh(&clock);
        clock.advance(Duration::from_secs(1_800));
        d.heartbeat(addr(1), &clock).unwrap();
        clock.advance(Duration::from_secs(1_800));
        // Total elapsed since heartbeat = 1800, threshold = 3600 -> no trigger.
        assert!(d.try_trigger(&clock).is_empty());
        assert_eq!(d.state, DeadManState::Armed);
    }

    #[test]
    fn trigger_fires_after_threshold() {
        let mut clock = ManualClock::at(Timestamp::from_secs(0));
        let mut d = fresh(&clock);
        clock.advance(Duration::from_secs(3_601));
        let payouts = d.try_trigger(&clock);
        assert_eq!(payouts.len(), 2);
        assert_eq!(payouts[0].amount + payouts[1].amount, 100);
        assert_eq!(d.state, DeadManState::Triggered);
    }

    #[test]
    fn rounding_residue_goes_to_last_beneficiary() {
        let clock = ManualClock::at(Timestamp::from_secs(0));
        let mut d = DeadMan::new(
            DeadManConfig {
                owner: addr(1),
                threshold: Duration::from_secs(60),
                beneficiaries: vec![
                    Beneficiary { address: addr(2), weight: 1 },
                    Beneficiary { address: addr(3), weight: 1 },
                    Beneficiary { address: addr(4), weight: 1 },
                ],
                deposit: 100,
            },
            &clock,
        )
        .unwrap();
        let mut clock = ManualClock::at(Timestamp::from_secs(0));
        clock.advance(Duration::from_secs(120));
        let payouts = d.try_trigger(&clock);
        // 100 / 3 = 33, 33, residue 34.
        assert_eq!(payouts[0].amount, 33);
        assert_eq!(payouts[1].amount, 33);
        assert_eq!(payouts[2].amount, 34);
    }

    #[test]
    fn unauthorised_heartbeat_rejected() {
        let clock = ManualClock::at(Timestamp::from_secs(0));
        let mut d = fresh(&clock);
        assert_eq!(
            d.heartbeat(addr(99), &clock).unwrap_err(),
            ContractError::UnauthorisedParty
        );
    }
}
