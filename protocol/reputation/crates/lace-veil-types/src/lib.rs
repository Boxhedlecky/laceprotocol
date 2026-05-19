//! Common types for the Lace reputation / Veil Score system.
//!
//! Mirrors the contract surface of the prediction-market and temporal-VM
//! components: opaque 32-byte ids, `Amount` as `u128`, basis-point scalars
//! for any value in `[0, 1]`. Anything richer than that lives in the crate
//! that owns the semantics (score state in `lace-veil-score`, attestation
//! graph in `lace-veil-attest`, etc.).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use core::fmt;
use serde::{Deserialize, Serialize};

/// A 32-byte opaque identifier. Same shape as the temporal-VM and
/// prediction-market `Bytes32`; used here for addresses, score
/// commitments, attestation ids, and loan ids.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Bytes32(pub [u8; 32]);

impl Bytes32 {
    /// All zero bytes. Useful as a sentinel and in tests.
    pub const ZERO: Bytes32 = Bytes32([0u8; 32]);

    /// Construct from a raw byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Bytes32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl From<[u8; 32]> for Bytes32 {
    fn from(b: [u8; 32]) -> Self {
        Self(b)
    }
}

/// A protocol participant. A `Bytes32` newtype to keep address
/// arguments from being silently swapped with other id kinds.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Address(pub Bytes32);

impl Address {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A non-negative quantity in the smallest indivisible LACE unit.
///
/// `u128` for consistency with the prediction-market and temporal-VM
/// components: intermediate computations in fee splits, slashing
/// distribution, and interest accrual can briefly exceed `u64`.
pub type Amount = u128;

/// A block height. `u64` matches the temporal-VM `BlockHeight`.
pub type BlockHeight = u64;

/// A duration measured in blocks. Distinct alias for readability at
/// the integration boundary; functionally a `u64`.
pub type BlockSpan = u64;

/// A basis-point scalar (0..=10_000). The Veil Score itself, every
/// score component, every weight, and every LTV is quoted in bps so
/// downstream consumers do not have to negotiate units.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Bps(u32);

impl Bps {
    /// Zero.
    pub const ZERO: Bps = Bps(0);
    /// One hundred per cent.
    pub const ONE: Bps = Bps(10_000);
    /// Maximum raw basis-point value.
    pub const MAX: u32 = 10_000;

    /// Construct from a raw basis-point value. Saturates above 10_000.
    pub const fn from_bps(bps: u32) -> Self {
        if bps > Self::MAX {
            Bps(Self::MAX)
        } else {
            Bps(bps)
        }
    }

    /// Unsaturating, but capped at 10_000.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Apply this fraction to an amount: `floor(amount * self / 10_000)`.
    pub const fn apply(self, amount: Amount) -> Amount {
        amount.saturating_mul(self.0 as Amount) / 10_000
    }

    /// Convenience: 50 %.
    pub const fn half() -> Self {
        Bps(5_000)
    }
}

/// The Veil Score itself, in basis points (0..=10_000).
///
/// This is the *clear-text* score value; only the score state crate
/// and the ZK proof crate ever see this type directly. Anything that
/// crosses an external boundary uses a [`ScoreCommitment`] plus a
/// proof from `lace-veil-proofs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Score(Bps);

impl Score {
    /// The lowest possible score (a fresh wallet with no signal).
    pub const ZERO: Score = Score(Bps::ZERO);
    /// The highest possible score.
    pub const MAX: Score = Score(Bps::ONE);

    /// Construct from a raw basis-point value.
    pub const fn from_bps(bps: u32) -> Self {
        Score(Bps::from_bps(bps))
    }

    /// Raw basis-point value (0..=10_000).
    pub const fn bps(self) -> u32 {
        self.0.raw()
    }

    /// Return the band this score falls into. Bands drive LTV ratios,
    /// governance multipliers, attestation weights, and timelock
    /// terms.
    pub const fn band(self) -> ScoreBand {
        ScoreBand::for_score(self)
    }
}

/// A binned view over a [`Score`]. Bands are the only score shape that
/// crosses external boundaries -- exposing the raw bps would leak more
/// than the protocol intends.
///
/// Five bands, evenly spaced in 2000-bps increments. Adjustable via
/// governance (see `lace-veil-governance::ScoreBandParams`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ScoreBand {
    /// 0..2000. New or distressed wallets. No undercollateralised
    /// credit. Minimum governance weight.
    Untrusted,
    /// 2000..4000. Established wallets with some clean history.
    Emerging,
    /// 4000..6000. Solid baseline. The middle of the curve.
    Established,
    /// 6000..8000. High-calibration wallets with sustained
    /// performance.
    Trusted,
    /// 8000..=10000. The top tier. Undercollateralised lending and
    /// max governance weight.
    Exemplary,
}

impl ScoreBand {
    /// Map a raw score to its band. Boundaries are inclusive on the
    /// low side, exclusive on the high side, with the top band
    /// inclusive at 10_000.
    pub const fn for_score(s: Score) -> ScoreBand {
        match s.bps() {
            0..=1_999 => ScoreBand::Untrusted,
            2_000..=3_999 => ScoreBand::Emerging,
            4_000..=5_999 => ScoreBand::Established,
            6_000..=7_999 => ScoreBand::Trusted,
            _ => ScoreBand::Exemplary,
        }
    }

    /// Numeric index 0..=4, useful for table-driven parameter lookup.
    pub const fn index(self) -> usize {
        match self {
            ScoreBand::Untrusted => 0,
            ScoreBand::Emerging => 1,
            ScoreBand::Established => 2,
            ScoreBand::Trusted => 3,
            ScoreBand::Exemplary => 4,
        }
    }

    /// All five bands in ascending order. The order matches
    /// [`ScoreBand::index`].
    pub const ALL: [ScoreBand; 5] = [
        ScoreBand::Untrusted,
        ScoreBand::Emerging,
        ScoreBand::Established,
        ScoreBand::Trusted,
        ScoreBand::Exemplary,
    ];
}

/// A Pedersen-style hiding commitment to a [`Score`].
///
/// The commitment is a 32-byte opaque value. Internally it's a hash
/// over `(score_bps, blinding_factor)` -- a stand-in for the
/// Pedersen-on-BN254 commitment the privacy layer will provide. See
/// [`lace-veil-proofs`] for the proof system that opens these.
///
/// External consumers never see the score itself, only the commitment
/// and a proof of some property over it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScoreCommitment(pub Bytes32);

impl ScoreCommitment {
    /// All-zero sentinel. Not a valid commitment to any score; used
    /// as the "no commitment yet" marker for fresh addresses.
    pub const NONE: ScoreCommitment = ScoreCommitment(Bytes32::ZERO);
}

/// A loan identifier. Distinct nominal type so the lending crate's
/// API cannot accidentally take an address where a loan is expected.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LoanId(pub Bytes32);

impl LoanId {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// An attestation identifier.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AttestationId(pub Bytes32);

impl AttestationId {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A reputation event ingested by the score engine.
///
/// Mirrors the `ReputationEvent` enums in the prediction-market oracle
/// crate and the temporal-VM disputes crate; this is the common
/// envelope they get normalised into when they reach Veil Score.
///
/// Keeping the shape stable here means the two upstream components
/// can evolve their own enums without forcing a Veil Score schema
/// migration: their `ReputationSink` impls translate into this type
/// at the boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoreEvent {
    /// A timelock obligation (recurring payment, escrow release,
    /// milestone payout) was met cleanly.
    PaymentMet {
        /// Address that met the obligation.
        subject: Address,
        /// Block at which the obligation cleared.
        at: BlockHeight,
    },
    /// A timelock obligation was missed. `consecutive` is the running
    /// count of consecutive misses; the engine penalises each miss
    /// progressively.
    PaymentMissed {
        /// Address that defaulted.
        subject: Address,
        /// Number of consecutive missed ticks at the moment of this
        /// event.
        consecutive: u64,
        /// Block at which the miss was recorded.
        at: BlockHeight,
    },
    /// A forecaster voted for the outcome that was finally resolved
    /// in a prediction market.
    ForecastCorrect {
        /// Forecaster address.
        subject: Address,
        /// Reputation weight the forecaster carried at vote time,
        /// in bps. Used to scale the calibration delta.
        weight_bps: u32,
        /// Block at which the market finalised.
        at: BlockHeight,
    },
    /// A forecaster voted for an outcome that was *not* the final
    /// resolution.
    ForecastIncorrect {
        /// Forecaster address.
        subject: Address,
        /// Reputation weight at vote time.
        weight_bps: u32,
        /// Block at which the market finalised.
        at: BlockHeight,
    },
    /// A peer attestation was successfully posted and accepted into
    /// the graph (already vetted for sybil resistance and weighted by
    /// [`lace-veil-attest`]).
    AttestationPosted {
        /// Attested subject.
        subject: Address,
        /// Attester.
        attester: Address,
        /// Effective weight assigned to the attestation, in bps.
        weight_bps: u32,
        /// Block at which the attestation was accepted.
        at: BlockHeight,
    },
    /// A previously-accepted attestation was revoked or expired.
    /// The score engine subtracts the same `weight_bps` it added.
    AttestationRevoked {
        /// Attested subject.
        subject: Address,
        /// Attester.
        attester: Address,
        /// Weight to subtract.
        weight_bps: u32,
        /// Block at which the revocation took effect.
        at: BlockHeight,
    },
    /// The first time the engine observes a given address. Anchors
    /// the tenure clock. The engine is responsible for emitting this
    /// itself on the first event for a previously-unseen address.
    FirstSeen {
        /// Newly-tracked address.
        subject: Address,
        /// Block at which it was first observed.
        at: BlockHeight,
    },
    /// A reputation-staked default. Slashes are applied by the stake
    /// crate; this event lets the score engine reflect the default
    /// in payment history immediately.
    Slashed {
        /// Slashed address.
        subject: Address,
        /// Amount slashed, in LACE.
        amount: Amount,
        /// Block at which the slash was applied.
        at: BlockHeight,
    },
}

impl ScoreEvent {
    /// The address this event affects.
    pub fn subject(&self) -> Address {
        match self {
            ScoreEvent::PaymentMet { subject, .. }
            | ScoreEvent::PaymentMissed { subject, .. }
            | ScoreEvent::ForecastCorrect { subject, .. }
            | ScoreEvent::ForecastIncorrect { subject, .. }
            | ScoreEvent::AttestationPosted { subject, .. }
            | ScoreEvent::AttestationRevoked { subject, .. }
            | ScoreEvent::FirstSeen { subject, .. }
            | ScoreEvent::Slashed { subject, .. } => *subject,
        }
    }

    /// Block height at which the event occurred.
    pub fn at(&self) -> BlockHeight {
        match self {
            ScoreEvent::PaymentMet { at, .. }
            | ScoreEvent::PaymentMissed { at, .. }
            | ScoreEvent::ForecastCorrect { at, .. }
            | ScoreEvent::ForecastIncorrect { at, .. }
            | ScoreEvent::AttestationPosted { at, .. }
            | ScoreEvent::AttestationRevoked { at, .. }
            | ScoreEvent::FirstSeen { at, .. }
            | ScoreEvent::Slashed { at, .. } => *at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bps_clamps_above_one() {
        assert_eq!(Bps::from_bps(99_999).raw(), 10_000);
    }

    #[test]
    fn bps_apply_floors() {
        assert_eq!(Bps::from_bps(2_500).apply(1_000), 250);
        assert_eq!(Bps::from_bps(1).apply(1_000), 0); // floors
    }

    #[test]
    fn score_bands_partition_full_range() {
        for raw in 0..=10_000 {
            let s = Score::from_bps(raw);
            // Every raw value maps to exactly one band; index must be 0..=4.
            assert!(s.band().index() < 5);
        }
    }

    #[test]
    fn score_band_boundaries() {
        assert_eq!(Score::from_bps(0).band(), ScoreBand::Untrusted);
        assert_eq!(Score::from_bps(1_999).band(), ScoreBand::Untrusted);
        assert_eq!(Score::from_bps(2_000).band(), ScoreBand::Emerging);
        assert_eq!(Score::from_bps(3_999).band(), ScoreBand::Emerging);
        assert_eq!(Score::from_bps(4_000).band(), ScoreBand::Established);
        assert_eq!(Score::from_bps(6_000).band(), ScoreBand::Trusted);
        assert_eq!(Score::from_bps(8_000).band(), ScoreBand::Exemplary);
        assert_eq!(Score::from_bps(10_000).band(), ScoreBand::Exemplary);
    }

    #[test]
    fn score_band_index_round_trips() {
        for (i, band) in ScoreBand::ALL.iter().enumerate() {
            assert_eq!(band.index(), i);
        }
    }

    #[test]
    fn score_event_subject_and_at() {
        let a = Address::new([1; 32]);
        let e = ScoreEvent::PaymentMissed {
            subject: a,
            consecutive: 3,
            at: 100,
        };
        assert_eq!(e.subject(), a);
        assert_eq!(e.at(), 100);
    }
}
