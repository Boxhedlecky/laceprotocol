//! Common types for the Lace prediction market engine.
//!
//! These types are intentionally minimal -- they are the contract surface
//! that all downstream crates in `protocol/prediction-markets/` and the
//! sibling components (privacy, temporal VM, Veil Score) agree on.
//!
//! Anything richer than an opaque identifier or a scalar lives in the
//! crate that owns the semantics (e.g. AMM state in `lace-pm-amm`,
//! resolution rounds in `lace-pm-oracle`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use core::fmt;
use serde::{Deserialize, Serialize};

/// A 32-byte opaque identifier. Same shape as the temporal-VM and
/// privacy-layer `Bytes32`; used here for market ids, outcome ids,
/// validator ids, and oracle references handed to other components.
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

/// A market identifier. Distinct nominal type from `Bytes32` so callers
/// cannot accidentally pass an outcome where a market is expected.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MarketId(pub Bytes32);

impl MarketId {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// An outcome identifier within a market. For binary markets, two
/// canonical outcome ids exist (`YES` and `NO`); for multi-outcome
/// markets the engine assigns one per branch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OutcomeId(pub Bytes32);

impl OutcomeId {
    /// The canonical `YES` outcome for binary markets.
    pub const YES: OutcomeId = OutcomeId(Bytes32([1u8; 32]));
    /// The canonical `NO` outcome for binary markets.
    pub const NO: OutcomeId = OutcomeId(Bytes32([2u8; 32]));

    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A protocol participant. Could be a wallet, a validator, or a
/// resolver; the prediction market engine does not care.
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
/// Using `u128` rather than `u64` because the AMM cost function for
/// large-liquidity markets can briefly accumulate to values that
/// overflow `u64` in intermediate computations.
pub type Amount = u128;

/// A probability expressed in basis points (0..=10_000).
///
/// The engine never stores probabilities as floats on its wire
/// boundary: all external consumers see a `Probability` and can
/// reason about it as an integer. Internally the AMM uses higher
/// precision (see `lace-pm-amm`) and rounds to basis points when
/// crossing the boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Probability(u32);

impl Probability {
    /// Zero probability.
    pub const ZERO: Probability = Probability(0);
    /// One (100 %) probability.
    pub const ONE: Probability = Probability(10_000);

    /// Maximum representable raw basis-point value.
    pub const MAX_BPS: u32 = 10_000;

    /// Build from basis points. Saturates above 10_000.
    pub const fn from_bps(bps: u32) -> Self {
        if bps > Self::MAX_BPS {
            Probability(Self::MAX_BPS)
        } else {
            Probability(bps)
        }
    }

    /// Build from an `f64` in `[0.0, 1.0]`. Saturating, banker-safe
    /// rounding (round half away from zero). Used only at the boundary
    /// between AMM internals and the external interface.
    pub fn from_f64(p: f64) -> Self {
        if !p.is_finite() || p <= 0.0 {
            return Probability::ZERO;
        }
        if p >= 1.0 {
            return Probability::ONE;
        }
        let bps = (p * 10_000.0).round() as i64;
        let bps = if bps < 0 {
            0
        } else if bps > Self::MAX_BPS as i64 {
            Self::MAX_BPS
        } else {
            bps as u32
        };
        Probability(bps)
    }

    /// Return the raw basis-point value.
    pub const fn bps(self) -> u32 {
        self.0
    }

    /// Return as an `f64` in `[0.0, 1.0]`.
    pub fn as_f64(self) -> f64 {
        (self.0 as f64) / 10_000.0
    }
}

/// Fee parameters for an AMM.
///
/// Fees are quoted in basis points of the trade *cost*, not of the
/// share count. See `lace-pm-amm` for the exact application.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeSchedule {
    /// Total trade fee in basis points (e.g. 30 = 0.30 %).
    pub trade_bps: u16,
    /// Of the total trade fee, the share routed to LACE token burn.
    /// Implements the deflationary pressure described in the master
    /// context block.
    pub burn_bps: u16,
    /// Of the total trade fee, the share routed to validator rewards.
    pub validator_bps: u16,
    /// Of the total trade fee, the share routed to the resolution
    /// pool that backs disputed-resolution slashing.
    pub resolution_bps: u16,
    /// Of the total trade fee, the share routed to the liquidity
    /// pool (LMSR subsidy reserve / LP returns).
    pub liquidity_bps: u16,
}

impl FeeSchedule {
    /// Sensible default for mainnet: 30 bps total, split 40 / 25 / 15 / 20
    /// between burn / validator / resolution / liquidity.
    pub const DEFAULT: FeeSchedule = FeeSchedule {
        trade_bps: 30,
        burn_bps: 4_000,
        validator_bps: 2_500,
        resolution_bps: 1_500,
        liquidity_bps: 2_000,
    };

    /// Apply the trade fee to a cost. Returns `(net_paid_by_user,
    /// fee_taken)`.
    pub fn split(self, gross_cost: Amount) -> (Amount, Amount) {
        let fee = gross_cost.saturating_mul(self.trade_bps as Amount) / 10_000;
        (gross_cost.saturating_add(fee), fee)
    }

    /// Route a collected fee into its four sinks. Returns
    /// `FeeRouting { burn, validator, resolution, liquidity }`.
    ///
    /// The four sub-bps must sum to 10_000; otherwise this method
    /// returns the partial split it can compute and the remainder is
    /// silently routed to the resolution pool (a safe default).
    pub fn route(self, fee: Amount) -> FeeRouting {
        let burn = fee.saturating_mul(self.burn_bps as Amount) / 10_000;
        let validator = fee.saturating_mul(self.validator_bps as Amount) / 10_000;
        let resolution = fee.saturating_mul(self.resolution_bps as Amount) / 10_000;
        let liquidity = fee.saturating_mul(self.liquidity_bps as Amount) / 10_000;
        let summed = burn + validator + resolution + liquidity;
        let remainder = fee.saturating_sub(summed);
        FeeRouting {
            burn,
            validator,
            resolution: resolution + remainder,
            liquidity,
        }
    }

    /// Returns true iff this schedule's four sub-bps sum exactly to
    /// 10_000.
    pub fn is_well_formed(self) -> bool {
        (self.burn_bps as u32 + self.validator_bps as u32
            + self.resolution_bps as u32 + self.liquidity_bps as u32) == 10_000
    }
}

/// The four sinks fed by collected market-making fees.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeRouting {
    /// LACE permanently destroyed.
    pub burn: Amount,
    /// Forwarded to the validator reward pool.
    pub validator: Amount,
    /// Forwarded to the dispute / resolution slashing pool.
    pub resolution: Amount,
    /// Forwarded to the AMM's liquidity / subsidy reserve.
    pub liquidity: Amount,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probability_clamps_outside_unit_interval() {
        assert_eq!(Probability::from_f64(-0.5).bps(), 0);
        assert_eq!(Probability::from_f64(1.5).bps(), 10_000);
        assert_eq!(Probability::from_f64(f64::NAN).bps(), 0);
    }

    #[test]
    fn probability_round_trip_bps() {
        for raw in [0, 1, 5_000, 9_999, 10_000, 99_999] {
            let p = Probability::from_bps(raw);
            assert!(p.bps() <= 10_000);
        }
    }

    #[test]
    fn fee_schedule_default_is_well_formed() {
        assert!(FeeSchedule::DEFAULT.is_well_formed());
    }

    #[test]
    fn fee_route_preserves_total_with_well_formed_schedule() {
        let fee = 10_000u128;
        let routing = FeeSchedule::DEFAULT.route(fee);
        let summed = routing.burn + routing.validator + routing.resolution + routing.liquidity;
        assert_eq!(summed, fee);
    }

    #[test]
    fn fee_route_remainder_goes_to_resolution_pool() {
        // Construct a deliberately-ill-formed schedule that drops 100 bps.
        let s = FeeSchedule {
            trade_bps: 30,
            burn_bps: 4_000,
            validator_bps: 2_500,
            resolution_bps: 1_400, // would be 1_500 in default
            liquidity_bps: 2_000,
        };
        let routing = s.route(10_000);
        let summed = routing.burn + routing.validator + routing.resolution + routing.liquidity;
        assert_eq!(summed, 10_000);
        assert!(routing.resolution >= 1_400 + 100);
    }

    #[test]
    fn outcome_yes_no_distinct() {
        assert_ne!(OutcomeId::YES, OutcomeId::NO);
    }
}
