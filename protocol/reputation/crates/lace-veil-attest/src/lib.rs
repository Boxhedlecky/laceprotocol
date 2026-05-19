//! Counterparty attestations.
//!
//! Peers attest to one another. Each attestation has:
//!
//! - a [`subject`] (the attested address),
//! - an [`attester`] (the address vouching),
//! - a [`raw_weight_bps`] (the attester's claim, in bps),
//! - a `posted_at` block.
//!
//! The graph applies three filters before emitting a
//! [`AttestationPosted`](lace_veil_types::ScoreEvent::AttestationPosted)
//! event into the score engine:
//!
//! 1. **Sybil weight.** An attester's effective weight is capped by
//!    a function of *their own* score band. Untrusted attesters
//!    contribute almost nothing; Exemplary attesters contribute close
//!    to their full claim. Specifically, the band yields a multiplier
//!    in bps applied to `raw_weight_bps`.
//! 2. **Per-attester budget.** Each attester gets a maximum total
//!    weight they can attribute across all subjects. A single high-
//!    score attester cannot single-handedly push many subjects.
//! 3. **Time decay.** Older attestations decay linearly to zero over
//!    `decay_full` blocks.
//!
//! Revocation is unilateral on the attester's side and instant on
//! the score side (a corresponding
//! [`AttestationRevoked`](lace_veil_types::ScoreEvent::AttestationRevoked)
//! event is emitted, subtracting the same effective weight that was
//! originally added).
//!
//! Disputes are out-of-band: the disputes crate in the temporal VM
//! can flag an attestation as bad-faith, in which case the graph
//! revokes it on the attester's behalf and slashes the attester's
//! own attestation budget by the disputed weight.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_veil_types::{
    Address, AttestationId, BlockHeight, BlockSpan, Bytes32, ScoreBand, ScoreEvent,
};
use serde::{Deserialize, Serialize};

/// Parameters controlling the attestation graph.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestParams {
    /// Per-band weight multiplier in bps. Indexed by
    /// `ScoreBand::index()`. An attester in band `i` has their
    /// `raw_weight_bps` multiplied by `band_multiplier_bps[i]` /
    /// 10_000.
    pub band_multiplier_bps: [u32; 5],
    /// Maximum total attestation weight, in bps, one attester may
    /// attribute across all subjects.
    pub per_attester_budget_bps: u64,
    /// Blocks after which an attestation has fully decayed to zero
    /// effective weight. Linear ramp.
    pub decay_full: BlockSpan,
    /// Slash, in bps of the attester's remaining budget, when an
    /// attestation is upheld as bad-faith via the disputes path.
    pub bad_faith_slash_bps: u32,
}

impl AttestParams {
    /// Launch defaults:
    /// - Sybil curve: 5/25/50/85/100 % multiplier across the five
    ///   bands. An Untrusted wallet attests at 5 % of nominal; an
    ///   Exemplary wallet attests at 100 %.
    /// - Per-attester budget: 50_000 bps total (5x the maximum any
    ///   single subject can absorb in attestation_bps).
    /// - Decay window: 12 months (~2.6M blocks at 12s blocks).
    /// - Bad-faith slash: 50 % of remaining budget.
    // TODO(governance): launch committee finalises these.
    pub const DEFAULT: AttestParams = AttestParams {
        band_multiplier_bps: [500, 2_500, 5_000, 8_500, 10_000],
        per_attester_budget_bps: 50_000,
        decay_full: 2_628_000,
        bad_faith_slash_bps: 5_000,
    };

    /// Effective multiplier for an attester in a given band, in bps.
    pub const fn multiplier_for(self, band: ScoreBand) -> u32 {
        self.band_multiplier_bps[band.index()]
    }
}

/// A persisted attestation record.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    /// Stable id.
    pub id: AttestationId,
    /// The address being attested to.
    pub subject: Address,
    /// The address vouching.
    pub attester: Address,
    /// The attester's raw claim, in bps. The graph applies the band
    /// multiplier and decay before this number ever influences a
    /// score.
    pub raw_weight_bps: u32,
    /// Block at which the attestation was posted.
    pub posted_at: BlockHeight,
    /// Effective weight contribution at post time (post band
    /// multiplier). Used so that revocation subtracts the same
    /// magnitude that was added, regardless of subsequent decay.
    pub effective_at_post_bps: u32,
    /// Whether this attestation has been revoked or expired.
    pub revoked: bool,
}

/// Bookkeeping for an attester's overall budget usage.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttesterLedger {
    /// Sum of `effective_at_post_bps` across currently-active
    /// attestations this attester has posted.
    pub used_bps: u64,
}

/// The attestation graph.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AttestGraph {
    /// All attestations, keyed by id.
    attestations: BTreeMap<AttestationId, Attestation>,
    /// Per-attester bookkeeping.
    ledgers: BTreeMap<Address, AttesterLedger>,
}

/// Errors from posting / revoking attestations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AttestError {
    /// An attestation with the given id already exists.
    DuplicateId,
    /// The attester is attesting to themselves; not allowed.
    SelfAttestation,
    /// `raw_weight_bps` is above 10_000.
    InvalidWeight,
    /// Posting this attestation would exceed the attester's overall
    /// per-attester budget.
    BudgetExceeded,
    /// Tried to revoke or look up an attestation that does not exist
    /// or is already revoked.
    NotFound,
}

/// Outcome of a post / revoke / dispute. Carries the ScoreEvent the
/// engine should ingest (zero, one, or two events depending on the
/// action) -- callers wire these into `VeilEngine::ingest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestOutcome {
    /// Events to feed into the score engine. Always one for post or
    /// revoke; two for a dispute that slashes the attester (one
    /// revoke for the disputed attestation, one
    /// `AttestationRevoked` for the slash effect on the attester
    /// itself).
    pub events: alloc::vec::Vec<ScoreEvent>,
}

impl AttestGraph {
    /// Build an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Post a new attestation.
    ///
    /// `attester_band` is the band the score engine reports for the
    /// attester *at this block*. The graph applies the per-band
    /// multiplier to derive the effective contribution.
    pub fn post(
        &mut self,
        id: AttestationId,
        subject: Address,
        attester: Address,
        raw_weight_bps: u32,
        attester_band: ScoreBand,
        params: AttestParams,
        at: BlockHeight,
    ) -> Result<AttestOutcome, AttestError> {
        if self.attestations.contains_key(&id) {
            return Err(AttestError::DuplicateId);
        }
        if subject == attester {
            return Err(AttestError::SelfAttestation);
        }
        if raw_weight_bps > 10_000 {
            return Err(AttestError::InvalidWeight);
        }
        let multiplier = params.multiplier_for(attester_band) as u64;
        let effective = (raw_weight_bps as u64 * multiplier) / 10_000;
        let ledger = self.ledgers.entry(attester).or_default();
        if ledger.used_bps.saturating_add(effective) > params.per_attester_budget_bps {
            return Err(AttestError::BudgetExceeded);
        }
        ledger.used_bps = ledger.used_bps.saturating_add(effective);
        let effective_bps = effective.min(u32::MAX as u64) as u32;
        self.attestations.insert(
            id,
            Attestation {
                id,
                subject,
                attester,
                raw_weight_bps,
                posted_at: at,
                effective_at_post_bps: effective_bps,
                revoked: false,
            },
        );
        let mut events = alloc::vec::Vec::with_capacity(1);
        if effective_bps > 0 {
            events.push(ScoreEvent::AttestationPosted {
                subject,
                attester,
                weight_bps: effective_bps,
                at,
            });
        }
        Ok(AttestOutcome { events })
    }

    /// Revoke an existing attestation. Frees the attester's budget
    /// and emits a matching `AttestationRevoked` event so the score
    /// engine can subtract the original contribution.
    pub fn revoke(
        &mut self,
        id: AttestationId,
        at: BlockHeight,
    ) -> Result<AttestOutcome, AttestError> {
        let a = self.attestations.get_mut(&id).ok_or(AttestError::NotFound)?;
        if a.revoked {
            return Err(AttestError::NotFound);
        }
        a.revoked = true;
        let copy = *a;
        if let Some(ledger) = self.ledgers.get_mut(&copy.attester) {
            ledger.used_bps = ledger.used_bps.saturating_sub(copy.effective_at_post_bps as u64);
        }
        let mut events = alloc::vec::Vec::with_capacity(1);
        if copy.effective_at_post_bps > 0 {
            events.push(ScoreEvent::AttestationRevoked {
                subject: copy.subject,
                attester: copy.attester,
                weight_bps: copy.effective_at_post_bps,
                at,
            });
        }
        Ok(AttestOutcome { events })
    }

    /// Settle a bad-faith dispute against an attestation: revoke it
    /// and slash the attester's remaining budget. Returns the
    /// score-engine event stream (the per-subject revoke, plus a
    /// budget-slash record in this graph that does not surface to
    /// the score engine -- only future posts will hit the lowered
    /// budget).
    pub fn settle_dispute_bad_faith(
        &mut self,
        id: AttestationId,
        params: AttestParams,
        at: BlockHeight,
    ) -> Result<AttestOutcome, AttestError> {
        let outcome = self.revoke(id, at)?;
        // Slash remaining budget. The slash does not retroactively
        // unwind prior attestations from this attester; it only
        // shrinks future capacity.
        let attester = {
            let a = self.attestations.get(&id).ok_or(AttestError::NotFound)?;
            a.attester
        };
        if let Some(ledger) = self.ledgers.get_mut(&attester) {
            let slash = (ledger.used_bps * params.bad_faith_slash_bps as u64) / 10_000;
            ledger.used_bps = ledger.used_bps.saturating_sub(slash);
        }
        Ok(outcome)
    }

    /// Apply time decay to all live attestations. For each one,
    /// compute the current effective weight; if it is below the
    /// at-post weight, emit a partial revoke equal to the
    /// difference. Returns the aggregated event stream.
    ///
    /// This is the "tick" the engine calls periodically (e.g. once
    /// per epoch) to keep stale attestations from carrying weight
    /// forever. Idempotent: calling it twice at the same block is a
    /// no-op the second time.
    pub fn tick_decay(
        &mut self,
        now: BlockHeight,
        params: AttestParams,
    ) -> AttestOutcome {
        let mut events = alloc::vec::Vec::new();
        if params.decay_full == 0 {
            return AttestOutcome { events };
        }
        for a in self.attestations.values_mut() {
            if a.revoked {
                continue;
            }
            let age = now.saturating_sub(a.posted_at);
            let bps_remaining = if age >= params.decay_full {
                0u64
            } else {
                let surviving =
                    (params.decay_full - age) as u128 * a.effective_at_post_bps as u128;
                (surviving / params.decay_full as u128) as u64
            };
            let already_attributed = a.effective_at_post_bps as u64;
            if bps_remaining < already_attributed {
                let delta = (already_attributed - bps_remaining).min(u32::MAX as u64) as u32;
                events.push(ScoreEvent::AttestationRevoked {
                    subject: a.subject,
                    attester: a.attester,
                    weight_bps: delta,
                    at: now,
                });
                a.effective_at_post_bps = bps_remaining as u32;
                if let Some(ledger) = self.ledgers.get_mut(&a.attester) {
                    ledger.used_bps = ledger.used_bps.saturating_sub(delta as u64);
                }
                if bps_remaining == 0 {
                    a.revoked = true;
                }
            }
        }
        AttestOutcome { events }
    }

    /// Borrow a single attestation.
    pub fn get(&self, id: &AttestationId) -> Option<&Attestation> {
        self.attestations.get(id)
    }

    /// Borrow an attester's ledger.
    pub fn ledger(&self, a: &Address) -> Option<&AttesterLedger> {
        self.ledgers.get(a)
    }
}

/// Convenience: derive a deterministic attestation id from its
/// components. Useful for tests and for deterministic transaction
/// authoring.
pub fn derive_id(subject: Address, attester: Address, posted_at: BlockHeight) -> AttestationId {
    let mut bytes = [0u8; 32];
    bytes[0..32].copy_from_slice(&subject.0 .0);
    for (i, b) in attester.0 .0.iter().enumerate() {
        bytes[i] ^= *b;
    }
    let p = posted_at.to_le_bytes();
    for (i, b) in p.iter().enumerate() {
        bytes[i] ^= *b;
    }
    AttestationId(Bytes32(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }
    fn id(b: u8) -> AttestationId {
        AttestationId::new([b; 32])
    }

    #[test]
    fn untrusted_attester_contributes_almost_nothing() {
        let mut g = AttestGraph::new();
        let out = g
            .post(
                id(1),
                addr(2),
                addr(3),
                10_000,
                ScoreBand::Untrusted,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap();
        // 10000 raw * 500/10000 multiplier = 500 effective.
        assert_eq!(out.events.len(), 1);
        match &out.events[0] {
            ScoreEvent::AttestationPosted { weight_bps, .. } => assert_eq!(*weight_bps, 500),
            _ => panic!(),
        }
    }

    #[test]
    fn exemplary_attester_contributes_full_weight() {
        let mut g = AttestGraph::new();
        let out = g
            .post(
                id(1),
                addr(2),
                addr(3),
                10_000,
                ScoreBand::Exemplary,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap();
        match &out.events[0] {
            ScoreEvent::AttestationPosted { weight_bps, .. } => assert_eq!(*weight_bps, 10_000),
            _ => panic!(),
        }
    }

    #[test]
    fn self_attestation_rejected() {
        let mut g = AttestGraph::new();
        let err = g
            .post(
                id(1),
                addr(2),
                addr(2),
                5_000,
                ScoreBand::Trusted,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap_err();
        assert_eq!(err, AttestError::SelfAttestation);
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut g = AttestGraph::new();
        g.post(
            id(1),
            addr(2),
            addr(3),
            5_000,
            ScoreBand::Trusted,
            AttestParams::DEFAULT,
            100,
        )
        .unwrap();
        let err = g
            .post(
                id(1),
                addr(4),
                addr(5),
                5_000,
                ScoreBand::Trusted,
                AttestParams::DEFAULT,
                101,
            )
            .unwrap_err();
        assert_eq!(err, AttestError::DuplicateId);
    }

    #[test]
    fn budget_caps_per_attester_total_weight() {
        let mut g = AttestGraph::new();
        // Default budget is 50_000 bps. Exemplary multiplier is 1x.
        // So 5 attestations of 10_000 bps fit; the 6th should fail.
        for i in 0..5 {
            g.post(
                id(i),
                addr(100 + i),
                addr(7),
                10_000,
                ScoreBand::Exemplary,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap();
        }
        let err = g
            .post(
                id(99),
                addr(200),
                addr(7),
                10_000,
                ScoreBand::Exemplary,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap_err();
        assert_eq!(err, AttestError::BudgetExceeded);
    }

    #[test]
    fn revoke_emits_matching_negative_event() {
        let mut g = AttestGraph::new();
        g.post(
            id(1),
            addr(2),
            addr(3),
            10_000,
            ScoreBand::Exemplary,
            AttestParams::DEFAULT,
            100,
        )
        .unwrap();
        let out = g.revoke(id(1), 200).unwrap();
        match &out.events[0] {
            ScoreEvent::AttestationRevoked { weight_bps, .. } => assert_eq!(*weight_bps, 10_000),
            _ => panic!(),
        }
        assert_eq!(g.ledger(&addr(3)).unwrap().used_bps, 0);
    }

    #[test]
    fn time_decay_zeroes_old_attestations() {
        let mut g = AttestGraph::new();
        g.post(
            id(1),
            addr(2),
            addr(3),
            10_000,
            ScoreBand::Exemplary,
            AttestParams::DEFAULT,
            100,
        )
        .unwrap();
        let later = 100 + AttestParams::DEFAULT.decay_full;
        let out = g.tick_decay(later, AttestParams::DEFAULT);
        assert!(!out.events.is_empty());
        // After tick, attestation should be revoked.
        assert!(g.get(&id(1)).unwrap().revoked);
    }

    #[test]
    fn time_decay_partial_emits_partial_revoke() {
        let mut g = AttestGraph::new();
        g.post(
            id(1),
            addr(2),
            addr(3),
            10_000,
            ScoreBand::Exemplary,
            AttestParams::DEFAULT,
            0,
        )
        .unwrap();
        let half = AttestParams::DEFAULT.decay_full / 2;
        let out = g.tick_decay(half, AttestParams::DEFAULT);
        match &out.events[0] {
            ScoreEvent::AttestationRevoked { weight_bps, .. } => {
                assert!(*weight_bps >= 4_900 && *weight_bps <= 5_100);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn bad_faith_dispute_revokes_and_slashes_budget() {
        let mut g = AttestGraph::new();
        // Burn the attester's full budget on five attestations.
        for i in 0..5 {
            g.post(
                id(i),
                addr(100 + i),
                addr(7),
                10_000,
                ScoreBand::Exemplary,
                AttestParams::DEFAULT,
                100,
            )
            .unwrap();
        }
        let used_before = g.ledger(&addr(7)).unwrap().used_bps;
        g.settle_dispute_bad_faith(id(0), AttestParams::DEFAULT, 200)
            .unwrap();
        let used_after = g.ledger(&addr(7)).unwrap().used_bps;
        // Revoke freed 10_000; then 50 % slash on the remaining 40_000.
        assert_eq!(used_before, 50_000);
        assert_eq!(used_after, (50_000 - 10_000) - ((50_000 - 10_000) * 5_000 / 10_000));
    }

    #[test]
    fn derive_id_distinct_for_distinct_inputs() {
        let a = derive_id(addr(1), addr(2), 100);
        let b = derive_id(addr(1), addr(2), 101);
        let c = derive_id(addr(1), addr(3), 100);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
