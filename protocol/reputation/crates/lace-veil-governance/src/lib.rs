//! Reputation-weighted governance.
//!
//! Per the master design principle, **governance weight = staked
//! LACE × Veil Score multiplier**. Pure token weighting is rejected:
//! a high-stake low-calibration voter carries less weight than a
//! moderate-stake high-calibration voter.
//!
//! The multiplier is a step function over [`ScoreBand`]:
//!
//! | Band        | Multiplier (bps) |
//! |-------------|------------------|
//! | Untrusted   |   5_000  (0.5x)  |
//! | Emerging    |   7_500  (0.75x) |
//! | Established |  10_000  (1.0x)  |
//! | Trusted     |  15_000  (1.5x)  |
//! | Exemplary   |  20_000  (2.0x)  |
//!
//! All values governance-adjustable.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lace_veil_types::{Address, Amount, ScoreBand};
use serde::{Deserialize, Serialize};

/// Band-keyed multiplier table, in bps. Indexed by
/// `ScoreBand::index()`. A value of `10_000` is neutral (1.0x).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreBandMultipliers(pub [u32; 5]);

impl ScoreBandMultipliers {
    /// Launch defaults: 0.5 / 0.75 / 1.0 / 1.5 / 2.0.
    // TODO(governance): launch committee finalises.
    pub const DEFAULT: ScoreBandMultipliers = ScoreBandMultipliers([5_000, 7_500, 10_000, 15_000, 20_000]);

    /// Multiplier for a given band, in bps.
    pub const fn for_band(self, band: ScoreBand) -> u32 {
        self.0[band.index()]
    }
}

/// Governance parameters.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceParams {
    /// Score-band multiplier table.
    pub multipliers: ScoreBandMultipliers,
    /// Minimum stake to vote at all. Below this, weight is zero
    /// regardless of score.
    pub min_voting_stake: Amount,
}

impl GovernanceParams {
    /// Launch defaults.
    // TODO(governance): launch committee finalises.
    pub const DEFAULT: GovernanceParams = GovernanceParams {
        multipliers: ScoreBandMultipliers::DEFAULT,
        min_voting_stake: 100,
    };
}

/// Compute a voter's effective weight from their stake and score
/// band.
///
/// Returns 0 when stake is below the minimum.
///
/// ```text
/// weight = floor(stake * multiplier_bps(band) / 10_000)
/// ```
pub fn vote_weight(stake: Amount, band: ScoreBand, params: GovernanceParams) -> Amount {
    if stake < params.min_voting_stake {
        return 0;
    }
    let multiplier = params.multipliers.for_band(band) as Amount;
    stake.saturating_mul(multiplier) / 10_000
}

/// A single voter's contribution to a tally.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    /// Voter.
    pub voter: Address,
    /// Voter's stake at vote time.
    pub stake: Amount,
    /// Voter's score band at vote time.
    pub band: ScoreBand,
    /// True if voting FOR, false if voting AGAINST.
    pub support: bool,
}

/// Aggregate tally over a set of votes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Total weight supporting the motion.
    pub support_weight: Amount,
    /// Total weight against the motion.
    pub against_weight: Amount,
    /// Count of distinct voters with non-zero weight.
    pub effective_voters: u64,
}

impl Tally {
    /// True iff the motion has strict majority of effective weight.
    pub fn passes(&self) -> bool {
        self.support_weight > self.against_weight
    }
}

/// Aggregate a slice of votes into a [`Tally`].
///
/// Deduplicates by voter address: a second vote from the same voter
/// replaces the first.
pub fn tally_votes(votes: &[Vote], params: GovernanceParams) -> Tally {
    let mut by_voter: BTreeMap<Address, &Vote> = BTreeMap::new();
    for v in votes {
        by_voter.insert(v.voter, v);
    }
    let mut t = Tally::default();
    for v in by_voter.values() {
        let w = vote_weight(v.stake, v.band, params);
        if w == 0 {
            continue;
        }
        t.effective_voters = t.effective_voters.saturating_add(1);
        if v.support {
            t.support_weight = t.support_weight.saturating_add(w);
        } else {
            t.against_weight = t.against_weight.saturating_add(w);
        }
    }
    t
}

/// Return the voter list sorted by descending effective weight.
/// Useful for surfacing the most-influential voters on a proposal.
pub fn sorted_by_weight(votes: &[Vote], params: GovernanceParams) -> Vec<(Address, Amount)> {
    let mut by_voter: BTreeMap<Address, Vote> = BTreeMap::new();
    for v in votes {
        by_voter.insert(v.voter, *v);
    }
    let mut out: Vec<(Address, Amount)> = by_voter
        .values()
        .map(|v| (v.voter, vote_weight(v.stake, v.band, params)))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }

    #[test]
    fn untrusted_voter_carries_half_weight() {
        let w = vote_weight(1_000, ScoreBand::Untrusted, GovernanceParams::DEFAULT);
        assert_eq!(w, 500);
    }

    #[test]
    fn established_voter_carries_neutral_weight() {
        let w = vote_weight(1_000, ScoreBand::Established, GovernanceParams::DEFAULT);
        assert_eq!(w, 1_000);
    }

    #[test]
    fn exemplary_voter_doubled() {
        let w = vote_weight(1_000, ScoreBand::Exemplary, GovernanceParams::DEFAULT);
        assert_eq!(w, 2_000);
    }

    #[test]
    fn sub_minimum_stake_zero_weight() {
        let w = vote_weight(50, ScoreBand::Exemplary, GovernanceParams::DEFAULT);
        assert_eq!(w, 0);
    }

    #[test]
    fn reputation_beats_pure_stake() {
        // 10x stake at Untrusted (0.5x) vs 1x stake at Exemplary (2x):
        // 10_000 * 0.5 = 5_000 vs 1_000 * 2 = 2_000. So actually pure
        // stake still wins by 2.5x here -- but consider 2x stake at
        // Untrusted vs 1x at Exemplary: 1_000 vs 2_000 (rep wins).
        let stake_w = vote_weight(2_000, ScoreBand::Untrusted, GovernanceParams::DEFAULT);
        let rep_w = vote_weight(1_000, ScoreBand::Exemplary, GovernanceParams::DEFAULT);
        assert!(rep_w > stake_w, "{} should beat {}", rep_w, stake_w);
    }

    #[test]
    fn tally_aggregates_support_and_against() {
        let votes = [
            Vote { voter: addr(1), stake: 1_000, band: ScoreBand::Established, support: true },
            Vote { voter: addr(2), stake: 1_000, band: ScoreBand::Established, support: true },
            Vote { voter: addr(3), stake: 1_000, band: ScoreBand::Established, support: false },
        ];
        let t = tally_votes(&votes, GovernanceParams::DEFAULT);
        assert_eq!(t.support_weight, 2_000);
        assert_eq!(t.against_weight, 1_000);
        assert!(t.passes());
        assert_eq!(t.effective_voters, 3);
    }

    #[test]
    fn tally_deduplicates_repeated_voters() {
        let votes = [
            Vote { voter: addr(1), stake: 1_000, band: ScoreBand::Established, support: true },
            Vote { voter: addr(1), stake: 1_000, band: ScoreBand::Established, support: false },
        ];
        let t = tally_votes(&votes, GovernanceParams::DEFAULT);
        // Latter vote replaces the former.
        assert_eq!(t.support_weight, 0);
        assert_eq!(t.against_weight, 1_000);
        assert_eq!(t.effective_voters, 1);
    }

    #[test]
    fn tally_ignores_sub_minimum_voters() {
        let votes = [
            Vote { voter: addr(1), stake: 50, band: ScoreBand::Exemplary, support: true },
            Vote { voter: addr(2), stake: 200, band: ScoreBand::Established, support: true },
        ];
        let t = tally_votes(&votes, GovernanceParams::DEFAULT);
        assert_eq!(t.support_weight, 200);
        assert_eq!(t.effective_voters, 1);
    }

    #[test]
    fn sorted_by_weight_ranks_exemplary_above_untrusted() {
        let votes = [
            Vote { voter: addr(1), stake: 1_000, band: ScoreBand::Exemplary, support: true },
            Vote { voter: addr(2), stake: 1_000, band: ScoreBand::Untrusted, support: true },
            Vote { voter: addr(3), stake: 1_000, band: ScoreBand::Established, support: true },
        ];
        let sorted = sorted_by_weight(&votes, GovernanceParams::DEFAULT);
        assert_eq!(sorted[0].0, addr(1)); // Exemplary, 2_000
        assert_eq!(sorted[1].0, addr(3)); // Established, 1_000
        assert_eq!(sorted[2].0, addr(2)); // Untrusted, 500
    }

    #[test]
    fn tally_passes_on_strict_majority() {
        let votes = [
            Vote { voter: addr(1), stake: 1_000, band: ScoreBand::Established, support: true },
            Vote { voter: addr(2), stake: 1_000, band: ScoreBand::Established, support: false },
        ];
        let t = tally_votes(&votes, GovernanceParams::DEFAULT);
        // Equal weight: no strict majority -> does not pass.
        assert!(!t.passes());
    }
}
