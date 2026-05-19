//! Reputation staking, slashing, calibration rewards, and unstake
//! cooldown.
//!
//! Users lock LACE against their own Veil Score. The stake serves
//! two purposes:
//!
//! 1. **Skin in the game.** Defaults (missed payment obligations,
//!    upheld disputes, bad-faith attestations) slash from the
//!    stake. Slash routing is fixed by protocol spec:
//!    **60 %** to the harmed counterparty, **25 %** burned,
//!    **15 %** to the ecosystem reserve.
//! 2. **Calibration rewards.** Stakes that sit through a full
//!    calibration epoch with no slashes accrue a reward proportional
//!    to the stake's calibration-component contribution.
//!
//! Unstake is gated by a cooldown window: a user requests unstake at
//! block `t`, and can withdraw at block `t + cooldown`. New slashing
//! events during the cooldown still apply.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_veil_types::{Address, Amount, BlockHeight, BlockSpan, ScoreEvent};
use serde::{Deserialize, Serialize};

/// Slashing routing split. The three sub-bps must sum to 10_000.
///
/// **Protocol-fixed**: 60 / 25 / 15. Documented in the master spec
/// and not governance-adjustable -- this is a load-bearing
/// commitment to the harmed counterparty.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashRouting {
    /// To the harmed counterparty.
    pub counterparty_bps: u32,
    /// Burned.
    pub burn_bps: u32,
    /// To the ecosystem reserve.
    pub ecosystem_bps: u32,
}

impl SlashRouting {
    /// The protocol-fixed routing.
    pub const PROTOCOL: SlashRouting = SlashRouting {
        counterparty_bps: 6_000,
        burn_bps: 2_500,
        ecosystem_bps: 1_500,
    };

    /// True iff the three sub-bps sum exactly to 10_000.
    pub const fn is_well_formed(self) -> bool {
        self.counterparty_bps + self.burn_bps + self.ecosystem_bps == 10_000
    }

    /// Split a slashed amount into its three sinks.
    pub fn split(self, amount: Amount) -> SlashDistribution {
        let to_counterparty =
            amount.saturating_mul(self.counterparty_bps as Amount) / 10_000;
        let to_burn = amount.saturating_mul(self.burn_bps as Amount) / 10_000;
        let to_ecosystem = amount.saturating_mul(self.ecosystem_bps as Amount) / 10_000;
        let summed = to_counterparty + to_burn + to_ecosystem;
        // Any rounding remainder goes to the ecosystem sink so the
        // total slashed equals the configured amount.
        let remainder = amount.saturating_sub(summed);
        SlashDistribution {
            to_counterparty,
            to_burn,
            to_ecosystem: to_ecosystem + remainder,
        }
    }
}

/// The realised three-way split of a slash.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashDistribution {
    /// LACE delivered to the harmed counterparty.
    pub to_counterparty: Amount,
    /// LACE permanently destroyed.
    pub to_burn: Amount,
    /// LACE forwarded to the ecosystem reserve.
    pub to_ecosystem: Amount,
}

/// Stake parameters.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StakeParams {
    /// Minimum stake to be eligible for calibration rewards and
    /// undercollateralised lending.
    pub min_stake: Amount,
    /// Length of the unstake cooldown, in blocks.
    pub unstake_cooldown: BlockSpan,
    /// Length of one calibration reward epoch.
    pub reward_epoch: BlockSpan,
    /// Reward, in bps of stake per epoch, for a stake that ran the
    /// full epoch with no slashes.
    pub reward_bps_per_epoch: u32,
    /// Protocol-fixed slash routing.
    pub routing: SlashRouting,
}

impl StakeParams {
    /// Launch defaults: min stake 100 LACE, cooldown ~14 days (~100k
    /// blocks at 12s), epoch ~30 days (~216k blocks), reward 50 bps
    /// per epoch (~6 % annualised at four epochs/quarter).
    // TODO(governance): launch committee finalises everything except
    // `routing`, which is protocol-fixed.
    pub const DEFAULT: StakeParams = StakeParams {
        min_stake: 100,
        unstake_cooldown: 100_800,
        reward_epoch: 216_000,
        reward_bps_per_epoch: 50,
        routing: SlashRouting::PROTOCOL,
    };
}

/// Per-address stake position.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StakePosition {
    /// Currently-locked LACE.
    pub locked: Amount,
    /// LACE currently in the unstake cooldown.
    pub cooling: Amount,
    /// Block at which the cooling tranche becomes withdrawable.
    pub cooling_ready_at: BlockHeight,
    /// Cumulative LACE slashed from this position.
    pub slashed_total: Amount,
    /// Cumulative reward earned by this position.
    pub rewards_earned: Amount,
    /// Last block at which a reward was paid (epoch anchor).
    pub last_reward_at: BlockHeight,
    /// Last block at which a slash hit this position. The next
    /// reward epoch is gated on `now - last_slash_at >=
    /// reward_epoch`.
    pub last_slash_at: BlockHeight,
}

/// The staking engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StakeEngine {
    positions: BTreeMap<Address, StakePosition>,
    /// Stake parameters.
    pub params: StakeParams,
}

impl Default for StakeEngine {
    fn default() -> Self {
        Self {
            positions: BTreeMap::new(),
            params: StakeParams::DEFAULT,
        }
    }
}

/// Errors from staking operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StakeError {
    /// Stake below `min_stake`.
    BelowMinimum,
    /// Insufficient locked balance to satisfy the operation.
    InsufficientStake,
    /// Tried to withdraw before the cooldown elapsed.
    CooldownActive,
    /// No cooling tranche exists to withdraw.
    NothingCooling,
}

impl StakeEngine {
    /// Build an engine with the default parameters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an engine with custom parameters.
    pub fn with_params(params: StakeParams) -> Self {
        Self {
            positions: BTreeMap::new(),
            params,
        }
    }

    /// Borrow a position.
    pub fn position_of(&self, a: &Address) -> Option<&StakePosition> {
        self.positions.get(a)
    }

    /// Lock more LACE into `subject`'s stake.
    pub fn stake(&mut self, subject: Address, amount: Amount, at: BlockHeight) -> Result<(), StakeError> {
        if amount < self.params.min_stake {
            return Err(StakeError::BelowMinimum);
        }
        let pos = self.positions.entry(subject).or_default();
        pos.locked = pos.locked.saturating_add(amount);
        if pos.last_reward_at == 0 {
            pos.last_reward_at = at;
        }
        Ok(())
    }

    /// Begin the unstake cooldown for `amount` LACE. The amount is
    /// moved from `locked` into `cooling`. A second `request_unstake`
    /// during an active cooldown extends the cooling balance and
    /// resets the ready timestamp.
    pub fn request_unstake(
        &mut self,
        subject: Address,
        amount: Amount,
        at: BlockHeight,
    ) -> Result<(), StakeError> {
        let pos = self.positions.get_mut(&subject).ok_or(StakeError::InsufficientStake)?;
        if amount > pos.locked {
            return Err(StakeError::InsufficientStake);
        }
        pos.locked = pos.locked.saturating_sub(amount);
        pos.cooling = pos.cooling.saturating_add(amount);
        pos.cooling_ready_at = at.saturating_add(self.params.unstake_cooldown);
        Ok(())
    }

    /// Withdraw a matured cooling tranche. Returns the amount
    /// released.
    pub fn withdraw(&mut self, subject: Address, at: BlockHeight) -> Result<Amount, StakeError> {
        let pos = self.positions.get_mut(&subject).ok_or(StakeError::NothingCooling)?;
        if pos.cooling == 0 {
            return Err(StakeError::NothingCooling);
        }
        if at < pos.cooling_ready_at {
            return Err(StakeError::CooldownActive);
        }
        let released = pos.cooling;
        pos.cooling = 0;
        pos.cooling_ready_at = 0;
        Ok(released)
    }

    /// Slash a stake. The slash applies first to `locked`, then to
    /// `cooling` -- a defaulter cannot front-run slashing by moving
    /// the entire stake into cooldown.
    ///
    /// Returns the [`SlashDistribution`] plus a
    /// [`ScoreEvent::Slashed`] for the score engine.
    pub fn slash(
        &mut self,
        subject: Address,
        counterparty: Address,
        amount: Amount,
        at: BlockHeight,
    ) -> SlashOutcome {
        let pos = self.positions.entry(subject).or_default();
        let mut remaining = amount;
        let from_locked = remaining.min(pos.locked);
        pos.locked = pos.locked.saturating_sub(from_locked);
        remaining = remaining.saturating_sub(from_locked);
        let from_cooling = remaining.min(pos.cooling);
        pos.cooling = pos.cooling.saturating_sub(from_cooling);
        remaining = remaining.saturating_sub(from_cooling);
        let realised = from_locked + from_cooling;
        pos.slashed_total = pos.slashed_total.saturating_add(realised);
        pos.last_slash_at = at;
        let distribution = self.params.routing.split(realised);
        SlashOutcome {
            requested: amount,
            realised,
            unrecovered: remaining,
            distribution,
            counterparty,
            score_event: ScoreEvent::Slashed {
                subject,
                amount: realised,
                at,
            },
        }
    }

    /// Pay one calibration epoch's worth of reward to `subject` if
    /// they have not been slashed within the most recent epoch.
    /// Returns the reward paid (zero if not yet eligible).
    pub fn settle_reward(&mut self, subject: Address, at: BlockHeight) -> Amount {
        let p = self.params;
        let pos = match self.positions.get_mut(&subject) {
            Some(p) => p,
            None => return 0,
        };
        if pos.locked < p.min_stake {
            return 0;
        }
        if at.saturating_sub(pos.last_reward_at) < p.reward_epoch {
            return 0;
        }
        if pos.last_slash_at != 0 && at.saturating_sub(pos.last_slash_at) < p.reward_epoch {
            // Slash within the most recent epoch -- no reward.
            pos.last_reward_at = at;
            return 0;
        }
        let reward =
            pos.locked.saturating_mul(p.reward_bps_per_epoch as Amount) / 10_000;
        pos.rewards_earned = pos.rewards_earned.saturating_add(reward);
        pos.last_reward_at = at;
        reward
    }
}

/// Result of a [`StakeEngine::slash`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashOutcome {
    /// Amount the slasher requested.
    pub requested: Amount,
    /// Amount actually slashed from the position (capped at the
    /// position's total balance).
    pub realised: Amount,
    /// Shortfall the slasher could not extract; surfaced so the
    /// lending crate can route it through its recovery path.
    pub unrecovered: Amount,
    /// Three-way split of `realised`.
    pub distribution: SlashDistribution,
    /// The harmed counterparty receiving `distribution.to_counterparty`.
    pub counterparty: Address,
    /// Event the score engine should ingest.
    pub score_event: ScoreEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }

    #[test]
    fn slash_routing_is_well_formed() {
        assert!(SlashRouting::PROTOCOL.is_well_formed());
    }

    #[test]
    fn slash_split_matches_spec() {
        let d = SlashRouting::PROTOCOL.split(10_000);
        assert_eq!(d.to_counterparty, 6_000);
        assert_eq!(d.to_burn, 2_500);
        assert_eq!(d.to_ecosystem, 1_500);
    }

    #[test]
    fn slash_split_remainder_lands_in_ecosystem() {
        let d = SlashRouting::PROTOCOL.split(7);
        // 7 * 6000 / 10000 = 4, 7 * 2500 / 10000 = 1, 7 * 1500 / 10000 = 1, remainder 1
        let sum = d.to_counterparty + d.to_burn + d.to_ecosystem;
        assert_eq!(sum, 7);
    }

    #[test]
    fn stake_below_minimum_rejected() {
        let mut e = StakeEngine::new();
        assert_eq!(
            e.stake(addr(1), 50, 100).unwrap_err(),
            StakeError::BelowMinimum
        );
    }

    #[test]
    fn stake_locks_balance() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        assert_eq!(e.position_of(&addr(1)).unwrap().locked, 1_000);
    }

    #[test]
    fn request_unstake_moves_to_cooling_and_sets_ready() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        e.request_unstake(addr(1), 600, 200).unwrap();
        let p = e.position_of(&addr(1)).unwrap();
        assert_eq!(p.locked, 400);
        assert_eq!(p.cooling, 600);
        assert_eq!(p.cooling_ready_at, 200 + StakeParams::DEFAULT.unstake_cooldown);
    }

    #[test]
    fn withdraw_before_cooldown_rejected() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        e.request_unstake(addr(1), 600, 200).unwrap();
        let err = e.withdraw(addr(1), 200 + 1_000).unwrap_err();
        assert_eq!(err, StakeError::CooldownActive);
    }

    #[test]
    fn withdraw_after_cooldown_releases() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        e.request_unstake(addr(1), 600, 200).unwrap();
        let at = 200 + StakeParams::DEFAULT.unstake_cooldown;
        let released = e.withdraw(addr(1), at).unwrap();
        assert_eq!(released, 600);
        assert_eq!(e.position_of(&addr(1)).unwrap().cooling, 0);
    }

    #[test]
    fn slash_takes_from_locked_then_cooling() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        e.request_unstake(addr(1), 600, 200).unwrap();
        let out = e.slash(addr(1), addr(2), 500, 300);
        // 400 in locked plus 100 of cooling.
        assert_eq!(out.realised, 500);
        let p = e.position_of(&addr(1)).unwrap();
        assert_eq!(p.locked, 0);
        assert_eq!(p.cooling, 500);
    }

    #[test]
    fn slash_unrecovered_shortfall_surfaces() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 100, 100).unwrap();
        let out = e.slash(addr(1), addr(2), 500, 300);
        assert_eq!(out.realised, 100);
        assert_eq!(out.unrecovered, 400);
    }

    #[test]
    fn slash_emits_score_event() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 1_000, 100).unwrap();
        let out = e.slash(addr(1), addr(2), 300, 500);
        match out.score_event {
            ScoreEvent::Slashed { subject, amount, at } => {
                assert_eq!(subject, addr(1));
                assert_eq!(amount, 300);
                assert_eq!(at, 500);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn reward_paid_after_full_epoch_without_slash() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 10_000, 100).unwrap();
        let at = 100 + StakeParams::DEFAULT.reward_epoch + 1;
        let r = e.settle_reward(addr(1), at);
        // 10_000 * 50 / 10_000 = 50.
        assert_eq!(r, 50);
    }

    #[test]
    fn reward_skipped_if_slashed_within_epoch() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 10_000, 100).unwrap();
        // Slash partway through the epoch.
        e.slash(addr(1), addr(2), 1_000, 100 + StakeParams::DEFAULT.reward_epoch / 2);
        let at = 100 + StakeParams::DEFAULT.reward_epoch + 1;
        let r = e.settle_reward(addr(1), at);
        assert_eq!(r, 0);
    }

    #[test]
    fn reward_skipped_below_min_stake() {
        let mut e = StakeEngine::new();
        e.stake(addr(1), 100, 100).unwrap();
        e.request_unstake(addr(1), 100, 200).unwrap();
        let at = 200 + StakeParams::DEFAULT.reward_epoch;
        assert_eq!(e.settle_reward(addr(1), at), 0);
    }
}
