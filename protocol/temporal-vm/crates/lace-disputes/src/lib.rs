//! Abort, dispute, and slashing -- the cross-cutting "things went
//! wrong" layer for the temporal VM.
//!
//! This crate is intentionally thin. The contracts crate owns most of
//! the state-transition logic; what lives here is:
//!
//! 1. The [`DisputeOutcome`] vocabulary: how an oracle resolution
//!    translates into slash / reward decisions.
//! 2. The [`SlashRules`] policy: bounds on how much can be slashed
//!    and against whom, applied consistently across all contracts.
//! 3. The [`ReputationSink`] trait: the integration boundary with
//!    Component 4 (Veil Score). The temporal VM never computes a
//!    reputation score itself; it only emits the events the score
//!    pipeline consumes.
//!
//! Splitting these out means a future change to slashing policy (for
//! example, capping slashes at 5% per dispute) is a single-file change
//! that touches all four contract templates uniformly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

use lace_contracts::{Address, Amount, Payout, PayoutReason};
use lace_vm::Bytes32;
use serde::{Deserialize, Serialize};

/// Outcome of a dispute. Carries enough to drive slashing and
/// reputation updates independently of *which* contract issued the
/// dispute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeOutcome {
    /// Contract instance that opened the dispute.
    pub contract_id: Bytes32,
    /// Party that prevailed.
    pub winner: Address,
    /// Party that lost.
    pub loser: Address,
    /// Amount transferred from the losing-party stake to the winner.
    /// Zero is valid (a "non-malicious loss" outcome).
    pub stake_transferred: Amount,
    /// Whether the loser is judged to have acted in provable bad
    /// faith. Drives the slashing magnitude and the reputation delta.
    pub bad_faith: bool,
}

/// Slashing policy. Single source of truth across contract templates.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashRules {
    /// Maximum fraction of the loser's bonded stake that may be
    /// slashed in a single dispute. Stored as a permille (parts per
    /// thousand) to keep the arithmetic exact.
    pub max_permille: u32,
    /// Additional permille added on top of `max_permille` when
    /// `bad_faith` is true. Together they MUST NOT exceed 1000.
    pub bad_faith_bonus_permille: u32,
}

impl SlashRules {
    /// Conservative default. 10% standard slash, +20% bad-faith
    /// bonus (capped at 30% total).
    pub const DEFAULT: Self = Self {
        max_permille: 100,
        bad_faith_bonus_permille: 200,
    };

    /// Compute the slash amount applied to `stake` under `bad_faith`.
    pub fn slash_amount(self, stake: Amount, bad_faith: bool) -> Amount {
        let mut permille = self.max_permille;
        if bad_faith {
            permille = permille.saturating_add(self.bad_faith_bonus_permille);
        }
        let permille = permille.min(1000) as Amount;
        stake.saturating_mul(permille) / 1000
    }
}

/// Emitted by a both-party abort that turned out to be unilateral
/// (one party requested abort, the other refused, and the abort
/// window closed). Drives a small reputation penalty against the
/// requester, who is presumed to have stalled the deal in bad faith.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleAbort {
    /// Contract that observed the stale abort.
    pub contract_id: Bytes32,
    /// Address that requested the unilateral abort.
    pub requester: Address,
}

/// Events fed into the reputation pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReputationEvent {
    /// A dispute settled with a definite winner / loser.
    DisputeSettled(DisputeOutcome),
    /// A recurring payment missed N consecutive ticks.
    PaymentsMissed {
        /// Contract.
        contract_id: Bytes32,
        /// Defaulting party.
        defaulter: Address,
        /// Consecutive missed-tick count.
        consecutive: u64,
    },
    /// A unilateral abort attempt that wasn't matched by the
    /// counterparty in the abort window.
    StaleAbort(StaleAbort),
    /// A contract released cleanly with mutual confirm. Positive
    /// reputation signal; the score pipeline applies a small bump.
    CleanRelease {
        /// Contract.
        contract_id: Bytes32,
        /// Both parties.
        parties: (Address, Address),
    },
}

/// Component 4 (Veil Score) implements this trait.
pub trait ReputationSink {
    /// Receive a reputation event.
    fn record(&mut self, event: ReputationEvent);
}

/// A no-op reputation sink. Useful in tests and in environments
/// where reputation accounting is disabled (e.g. devnets).
pub struct NullSink;

impl ReputationSink for NullSink {
    fn record(&mut self, _: ReputationEvent) {}
}

/// Translate a dispute outcome into a payout against the loser's
/// stake. Returned alongside the contract's own settlement payouts.
pub fn slash_payout(
    outcome: &DisputeOutcome,
    rules: SlashRules,
    loser_stake: Amount,
) -> Option<Payout> {
    let amount = rules.slash_amount(loser_stake, outcome.bad_faith);
    if amount == 0 {
        return None;
    }
    Some(Payout {
        to: outcome.winner,
        amount,
        reason: PayoutReason::Slash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    #[test]
    fn slash_default_is_10_percent() {
        assert_eq!(SlashRules::DEFAULT.slash_amount(1_000, false), 100);
    }

    #[test]
    fn slash_bad_faith_adds_bonus() {
        // 100 + 200 permille = 300 permille = 30%.
        assert_eq!(SlashRules::DEFAULT.slash_amount(1_000, true), 300);
    }

    #[test]
    fn slash_caps_at_full_stake() {
        let rules = SlashRules {
            max_permille: 900,
            bad_faith_bonus_permille: 500,
        };
        assert_eq!(rules.slash_amount(1_000, true), 1_000);
    }

    #[test]
    fn slash_payout_returns_none_below_dust() {
        let outcome = DisputeOutcome {
            contract_id: addr(1),
            winner: addr(2),
            loser: addr(3),
            stake_transferred: 0,
            bad_faith: false,
        };
        assert!(slash_payout(&outcome, SlashRules::DEFAULT, 1).is_none());
    }

    #[test]
    fn slash_payout_emits_to_winner() {
        let outcome = DisputeOutcome {
            contract_id: addr(1),
            winner: addr(2),
            loser: addr(3),
            stake_transferred: 0,
            bad_faith: true,
        };
        let p = slash_payout(&outcome, SlashRules::DEFAULT, 1_000).unwrap();
        assert_eq!(p.to, addr(2));
        assert_eq!(p.amount, 300);
        assert_eq!(p.reason, PayoutReason::Slash);
    }

    struct CountingSink(Vec<ReputationEvent>);
    impl ReputationSink for CountingSink {
        fn record(&mut self, e: ReputationEvent) {
            self.0.push(e);
        }
    }

    #[test]
    fn reputation_sink_records_events() {
        let mut sink = CountingSink(Vec::new());
        sink.record(ReputationEvent::CleanRelease {
            contract_id: addr(1),
            parties: (addr(2), addr(3)),
        });
        sink.record(ReputationEvent::PaymentsMissed {
            contract_id: addr(1),
            defaulter: addr(2),
            consecutive: 3,
        });
        assert_eq!(sink.0.len(), 2);
    }
}
