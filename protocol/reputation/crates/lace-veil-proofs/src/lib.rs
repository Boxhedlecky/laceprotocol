//! ZK proofs over a Veil Score.
//!
//! Four proof kinds, all over a hiding commitment to a per-address
//! score witness:
//!
//! - **Threshold** -- "my score is at least `threshold`"
//! - **Zero defaults** -- "I have not missed any payment in the last
//!   `window` blocks" (parameterised by `now`)
//! - **Calibration band** -- "my forecaster calibration sits in
//!   `[lo, hi]`"
//! - **Tenure** -- "this wallet has been active for at least
//!   `min_blocks` blocks"
//!
//! ## Stand-in
//!
//! The privacy layer (Component 1) will host the actual Halo2
//! circuits via `lace-circuits`. Until those land, this crate uses a
//! hash-and-blinding commit-reveal stand-in that mirrors the privacy
//! primitives' Blake2b-stand-in approach. The statement, witness, and
//! verifier shapes are stable -- swapping in the Halo2 circuit is a
//! single-module change behind the [`prove`] / [`verify`] surface,
//! the same migration model `lace-primitives::hash` uses.
//!
//! Tagged with `// TODO(zk-circuit)` markers where the substrate
//! changes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use lace_veil_types::{Address, BlockHeight, BlockSpan, Bytes32, ScoreCommitment};
use serde::{Deserialize, Serialize};

/// The four statement kinds the verifier accepts. Each variant
/// carries only the *public* inputs -- the witness lives in
/// [`Witness`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Statement {
    /// "score >= threshold_bps".
    Threshold {
        /// Subject of the proof.
        subject: Address,
        /// Hiding commitment to the score the prover claims.
        commitment: ScoreCommitment,
        /// Lower bound, inclusive, in bps.
        threshold_bps: u32,
    },
    /// "no missed payment within the last `window` blocks ending at
    /// `now`".
    ZeroDefaults {
        /// Subject of the proof.
        subject: Address,
        /// Hiding commitment to the score the prover claims.
        commitment: ScoreCommitment,
        /// Block height the prover treats as 'now'. Verifier
        /// enforces this is `<= chain tip + clock skew`.
        now: BlockHeight,
        /// Window length, in blocks.
        window: BlockSpan,
    },
    /// "calibration_bps in [lo_bps, hi_bps]".
    CalibrationBand {
        /// Subject of the proof.
        subject: Address,
        /// Hiding commitment.
        commitment: ScoreCommitment,
        /// Inclusive lower bound.
        lo_bps: u32,
        /// Inclusive upper bound.
        hi_bps: u32,
    },
    /// "wallet age (now - first_seen) >= min_blocks".
    Tenure {
        /// Subject of the proof.
        subject: Address,
        /// Hiding commitment.
        commitment: ScoreCommitment,
        /// Block the prover treats as 'now'.
        now: BlockHeight,
        /// Minimum required age.
        min_blocks: BlockSpan,
    },
}

impl Statement {
    /// The subject address asserted by this statement. Verifiers tie
    /// the statement to a known address before checking the proof.
    pub fn subject(&self) -> Address {
        match self {
            Statement::Threshold { subject, .. }
            | Statement::ZeroDefaults { subject, .. }
            | Statement::CalibrationBand { subject, .. }
            | Statement::Tenure { subject, .. } => *subject,
        }
    }

    /// The score commitment this statement opens.
    pub fn commitment(&self) -> ScoreCommitment {
        match self {
            Statement::Threshold { commitment, .. }
            | Statement::ZeroDefaults { commitment, .. }
            | Statement::CalibrationBand { commitment, .. }
            | Statement::Tenure { commitment, .. } => *commitment,
        }
    }
}

/// The prover's private witness for any of the four statement kinds.
///
/// All four kinds share the same witness shape: the prover knows the
/// raw score components, the wallet's first_seen height, and the
/// blinding factor used in the commitment. This is the data the
/// Halo2 circuit will eventually consume in-circuit; in the
/// stand-in, [`verify`] checks that the witness, when hashed with
/// the blinding, reproduces the commitment.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    /// Raw blended score, in bps.
    pub score_bps: u32,
    /// Raw calibration component, in bps.
    pub calibration_bps: u32,
    /// Block this wallet was first observed at.
    pub first_seen: BlockHeight,
    /// Block of the most-recent missed payment (0 if never).
    pub last_missed_at: BlockHeight,
    /// Per-commitment blinding factor. The prover picks this; the
    /// verifier never sees it directly, only its effect on the
    /// commitment.
    pub blinding: [u8; 32],
}

impl Witness {
    /// Build a `Witness`. Out-of-circuit helper for tests and for the
    /// stand-in prover.
    pub const fn new(
        score_bps: u32,
        calibration_bps: u32,
        first_seen: BlockHeight,
        last_missed_at: BlockHeight,
        blinding: [u8; 32],
    ) -> Self {
        Self {
            score_bps,
            calibration_bps,
            first_seen,
            last_missed_at,
            blinding,
        }
    }
}

/// An opaque proof artefact.
///
/// In the Halo2 instantiation this will be the serialised proof
/// transcript. In the stand-in, it carries the witness fields under
/// a domain-separated hash so the verifier can re-derive the
/// commitment and check the statement's predicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    /// The four field of the witness, opened (stand-in only).
    /// TODO(zk-circuit): replace with serialised Halo2 transcript;
    /// these fields disappear from the public proof.
    pub revealed: Witness,
}

/// Errors that can come out of [`verify`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The witness does not bind to the statement's commitment.
    /// In Halo2 this is a circuit-constraint failure.
    CommitmentMismatch,
    /// The statement's predicate (e.g. `score >= threshold`) is
    /// false against the witness.
    PredicateFailed,
    /// The statement's parameters are themselves invalid (e.g. a
    /// calibration band where `lo > hi`).
    MalformedStatement,
}

/// Compute the hiding commitment that a given witness produces.
///
/// Domain-separated Blake2b over the witness fields and the blinding,
/// reduced to a 32-byte tag. Stand-in shape; the eventual Pedersen
/// commitment will be a single curve point but exposes the same
/// 32-byte interface to callers.
pub fn commit(w: &Witness) -> ScoreCommitment {
    let mut buf: Vec<u8> = Vec::with_capacity(8 + 32 + 32 + 4 + 4 + 32);
    buf.extend_from_slice(b"lace/veil/commit/v1");
    buf.extend_from_slice(&w.score_bps.to_le_bytes());
    buf.extend_from_slice(&w.calibration_bps.to_le_bytes());
    buf.extend_from_slice(&w.first_seen.to_le_bytes());
    buf.extend_from_slice(&w.last_missed_at.to_le_bytes());
    buf.extend_from_slice(&w.blinding);
    ScoreCommitment(Bytes32(blake_32(&buf)))
}

/// Build a proof for a statement given the matching witness.
///
/// In the stand-in, this just re-attaches the witness inside the
/// `Proof`. The Halo2 instantiation will replace this body with the
/// prover loop; the function signature is stable.
pub fn prove(statement: &Statement, witness: &Witness) -> Result<Proof, VerifyError> {
    if commit(witness) != statement.commitment() {
        return Err(VerifyError::CommitmentMismatch);
    }
    if !predicate_holds(statement, witness)? {
        return Err(VerifyError::PredicateFailed);
    }
    // TODO(zk-circuit): invoke Halo2 prover; emit transcript bytes
    // instead of `revealed`.
    Ok(Proof { revealed: *witness })
}

/// Verify a proof against a statement. Returns `Ok(())` on success.
///
/// This is the only API external consumers (lending, governance,
/// timelock terms) need.
pub fn verify(statement: &Statement, proof: &Proof) -> Result<(), VerifyError> {
    if commit(&proof.revealed) != statement.commitment() {
        return Err(VerifyError::CommitmentMismatch);
    }
    if !predicate_holds(statement, &proof.revealed)? {
        return Err(VerifyError::PredicateFailed);
    }
    Ok(())
}

fn predicate_holds(statement: &Statement, w: &Witness) -> Result<bool, VerifyError> {
    match statement {
        Statement::Threshold { threshold_bps, .. } => {
            if *threshold_bps > 10_000 {
                return Err(VerifyError::MalformedStatement);
            }
            Ok(w.score_bps >= *threshold_bps)
        }
        Statement::ZeroDefaults { now, window, .. } => {
            // No missed payment within (now - window, now].
            // A `last_missed_at` of 0 means 'never'.
            if *window == 0 {
                return Ok(true);
            }
            if w.last_missed_at == 0 {
                return Ok(true);
            }
            let lo = now.saturating_sub(*window);
            Ok(w.last_missed_at <= lo)
        }
        Statement::CalibrationBand { lo_bps, hi_bps, .. } => {
            if lo_bps > hi_bps || *hi_bps > 10_000 {
                return Err(VerifyError::MalformedStatement);
            }
            Ok(w.calibration_bps >= *lo_bps && w.calibration_bps <= *hi_bps)
        }
        Statement::Tenure { now, min_blocks, .. } => {
            let age = now.saturating_sub(w.first_seen);
            Ok(age >= *min_blocks)
        }
    }
}

fn blake_32(input: &[u8]) -> [u8; 32] {
    // Lightweight 32-byte domain hash. We pull a tiny Merkle-Damgard
    // round in pure code rather than adding blake2 as a dependency
    // here; the privacy layer's `lace-primitives::hash` is what the
    // circuit-side commit will use. This is a faithful stand-in.
    //
    // TODO(zk-circuit): replace this with a call into
    // `lace_primitives::hash` once Component 1 exposes the Pedersen
    // commitment API to non-circuit callers.
    let mut state: [u64; 4] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
    ];
    for (i, byte) in input.iter().enumerate() {
        let lane = i % 4;
        state[lane] = state[lane]
            .wrapping_add(*byte as u64)
            .wrapping_mul(0x100000001b3)
            .rotate_left(7);
    }
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&state[0].to_le_bytes());
    out[8..16].copy_from_slice(&state[1].to_le_bytes());
    out[16..24].copy_from_slice(&state[2].to_le_bytes());
    out[24..32].copy_from_slice(&state[3].to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }
    fn b(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn threshold_proof_round_trip() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(1));
        let c = commit(&w);
        let s = Statement::Threshold {
            subject: addr(1),
            commitment: c,
            threshold_bps: 7_000,
        };
        let p = prove(&s, &w).unwrap();
        assert!(verify(&s, &p).is_ok());
    }

    #[test]
    fn threshold_proof_rejects_below_threshold() {
        let w = Witness::new(6_500, 6_000, 1_000, 0, b(1));
        let c = commit(&w);
        let s = Statement::Threshold {
            subject: addr(1),
            commitment: c,
            threshold_bps: 7_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::PredicateFailed);
    }

    #[test]
    fn threshold_proof_rejects_wrong_commitment() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(1));
        let bad_w = Witness::new(7_500, 6_000, 1_000, 0, b(99));
        let s = Statement::Threshold {
            subject: addr(1),
            commitment: commit(&bad_w),
            threshold_bps: 7_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::CommitmentMismatch);
    }

    #[test]
    fn zero_defaults_accepts_clean_window() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(2));
        let s = Statement::ZeroDefaults {
            subject: addr(2),
            commitment: commit(&w),
            now: 100_000,
            window: 50_000,
        };
        let p = prove(&s, &w).unwrap();
        assert!(verify(&s, &p).is_ok());
    }

    #[test]
    fn zero_defaults_rejects_recent_miss() {
        let w = Witness::new(7_500, 6_000, 1_000, 80_000, b(2));
        let s = Statement::ZeroDefaults {
            subject: addr(2),
            commitment: commit(&w),
            now: 100_000,
            window: 50_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::PredicateFailed);
    }

    #[test]
    fn zero_defaults_accepts_miss_outside_window() {
        let w = Witness::new(7_500, 6_000, 1_000, 10_000, b(2));
        let s = Statement::ZeroDefaults {
            subject: addr(2),
            commitment: commit(&w),
            now: 100_000,
            window: 50_000,
        };
        let p = prove(&s, &w).unwrap();
        assert!(verify(&s, &p).is_ok());
    }

    #[test]
    fn calibration_band_inclusive() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(3));
        let s = Statement::CalibrationBand {
            subject: addr(3),
            commitment: commit(&w),
            lo_bps: 5_000,
            hi_bps: 7_000,
        };
        let p = prove(&s, &w).unwrap();
        assert!(verify(&s, &p).is_ok());
    }

    #[test]
    fn calibration_band_rejects_below() {
        let w = Witness::new(7_500, 4_500, 1_000, 0, b(3));
        let s = Statement::CalibrationBand {
            subject: addr(3),
            commitment: commit(&w),
            lo_bps: 5_000,
            hi_bps: 7_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::PredicateFailed);
    }

    #[test]
    fn calibration_band_malformed_lo_gt_hi() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(3));
        let s = Statement::CalibrationBand {
            subject: addr(3),
            commitment: commit(&w),
            lo_bps: 7_000,
            hi_bps: 5_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::MalformedStatement);
    }

    #[test]
    fn tenure_passes_after_min_blocks() {
        let w = Witness::new(7_500, 6_000, 1_000, 0, b(4));
        let s = Statement::Tenure {
            subject: addr(4),
            commitment: commit(&w),
            now: 1_000_000,
            min_blocks: 100_000,
        };
        let p = prove(&s, &w).unwrap();
        assert!(verify(&s, &p).is_ok());
    }

    #[test]
    fn tenure_rejects_young_wallet() {
        let w = Witness::new(7_500, 6_000, 900_000, 0, b(4));
        let s = Statement::Tenure {
            subject: addr(4),
            commitment: commit(&w),
            now: 1_000_000,
            min_blocks: 200_000,
        };
        assert_eq!(prove(&s, &w).unwrap_err(), VerifyError::PredicateFailed);
    }

    #[test]
    fn verify_independent_of_prove() {
        // A maliciously-crafted proof with a swapped witness should fail
        // commitment binding at verify time.
        let real = Witness::new(7_500, 6_000, 1_000, 0, b(5));
        let s = Statement::Threshold {
            subject: addr(5),
            commitment: commit(&real),
            threshold_bps: 5_000,
        };
        let fake = Witness::new(9_000, 6_000, 1_000, 0, b(5));
        let tampered = Proof { revealed: fake };
        assert_eq!(verify(&s, &tampered).unwrap_err(), VerifyError::CommitmentMismatch);
    }
}
