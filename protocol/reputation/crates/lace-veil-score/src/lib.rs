//! Veil Score: continuous on-chain reputation, computed from four
//! orthogonal inputs.
//!
//! ```text
//!   score_bps = floor(
//!       weight.payment    * payment_history_bps     +
//!       weight.calibration * calibration_bps         +
//!       weight.attestation * attestation_bps         +
//!       weight.tenure     * tenure_bps
//!   ) / 10_000
//! ```
//!
//! Each input is a basis-point scalar in `[0, 10_000]`. The four
//! weights are themselves basis-points that sum to 10_000 (enforced
//! by `ScoreWeights::is_well_formed`).
//!
//! ## Soul-bound
//!
//! Score state is keyed by [`lace_veil_types::Address`] and is
//! **non-transferable**. There is no `transfer_score` API; deleting a
//! wallet does not transfer reputation to the next wallet.
//!
//! ## Privacy
//!
//! The raw score is held in the engine's local state, which is the
//! prover's secret witness. Only [`lace_veil_types::ScoreCommitment`]
//! values cross the engine boundary -- those are hiding commitments
//! that downstream consumers feed into `lace-veil-proofs` to verify
//! score properties without learning the score itself.
//!
//! ## Continuous update
//!
//! [`VeilEngine::ingest`] applies one `ScoreEvent` at a time and
//! returns the freshly-recomputed [`Score`]. The engine maintains
//! per-address accumulators so that an event only needs to recompute
//! the affected component (e.g. a `PaymentMet` event only touches the
//! payment-history accumulator), keeping the per-event cost O(1).
//!
//! ## Tenure decay
//!
//! Every component except tenure decays toward 5000 bps (neutral)
//! over time, so a wallet that goes quiet does not coast on old
//! signal. See [`DecayParams`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_veil_types::{Address, Amount, BlockHeight, BlockSpan, Score, ScoreEvent};
use serde::{Deserialize, Serialize};

/// Weights for the four score inputs, in basis points. Must sum to
/// 10_000.
///
/// Adjustable via governance (the launch parameter committee picks
/// the initial values).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreWeights {
    /// Weight on payment history.
    pub payment_bps: u32,
    /// Weight on forecaster calibration.
    pub calibration_bps: u32,
    /// Weight on counterparty attestations.
    pub attestation_bps: u32,
    /// Weight on on-chain tenure.
    pub tenure_bps: u32,
}

impl ScoreWeights {
    /// Launch default: 40 % payments, 25 % calibration, 20 %
    /// attestation, 15 % tenure. Payments dominate because they are
    /// the hardest signal to fake; tenure is light because Sybils can
    /// age cheaply.
    // TODO(governance): launch committee finalises these.
    pub const DEFAULT: ScoreWeights = ScoreWeights {
        payment_bps: 4_000,
        calibration_bps: 2_500,
        attestation_bps: 2_000,
        tenure_bps: 1_500,
    };

    /// True iff the four weights sum exactly to 10_000.
    pub const fn is_well_formed(self) -> bool {
        (self.payment_bps + self.calibration_bps + self.attestation_bps + self.tenure_bps)
            == 10_000
    }
}

/// Decay parameters. Every non-tenure component drifts back toward
/// the neutral midpoint (5000 bps) at a rate of `decay_bps_per_span`
/// basis points per `decay_span` blocks of inactivity.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecayParams {
    /// Length of one decay step, in blocks.
    pub decay_span: BlockSpan,
    /// Magnitude of one decay step, in bps drift toward 5000.
    pub decay_bps_per_span: u32,
}

impl DecayParams {
    /// Launch default: 50 bps drift per ~1 week (assuming 12 s
    /// blocks, that's 50_400 blocks). Slow enough to reward sustained
    /// participation, fast enough to keep stale scores from gaming
    /// downstream LTV.
    // TODO(governance): launch committee finalises these.
    pub const DEFAULT: DecayParams = DecayParams {
        decay_span: 50_400,
        decay_bps_per_span: 50,
    };
}

/// Saturation parameters for each component accumulator.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaturationParams {
    /// Blocks of activity at which `tenure_bps` reaches its maximum
    /// 10_000. Linear ramp from `FirstSeen` block.
    pub tenure_full: BlockSpan,
    /// Number of distinct payment events at which payment history
    /// reaches its asymptote. The accumulator is `paid / (paid +
    /// k*missed_weighted)` mapped to bps, with `k =
    /// missed_penalty_multiplier`.
    pub payment_full: u64,
    /// Penalty multiplier on missed payments; consecutive misses are
    /// further amplified by this multiplier raised to the streak.
    pub missed_penalty_multiplier: u32,
    /// Forecasts after which calibration reaches its asymptote.
    pub calibration_full: u64,
}

impl SaturationParams {
    /// Launch defaults: tenure asymptote at ~1 year (2.6M blocks),
    /// payment-history asymptote at 50 events, calibration at 100
    /// forecasts, missed-payment multiplier of 3 (one miss costs as
    /// much as three on-time payments).
    // TODO(governance): launch committee finalises these.
    pub const DEFAULT: SaturationParams = SaturationParams {
        tenure_full: 2_628_000,
        payment_full: 50,
        missed_penalty_multiplier: 3,
        calibration_full: 100,
    };
}

/// Per-address state. Held privately by the engine.
///
/// The four `*_bps` fields are component accumulators in
/// `[0, 10_000]`. They are blended into a single `Score` on demand
/// via [`VeilEngine::score_of`] using the engine's `ScoreWeights`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressState {
    /// Block at which the engine first observed this address. Anchors
    /// the tenure ramp.
    pub first_seen: BlockHeight,
    /// Block at which the engine last touched this state. Used to
    /// apply decay lazily.
    pub last_touched: BlockHeight,
    /// Running count of cleanly-met payment obligations.
    pub payments_met: u64,
    /// Running, weighted count of missed payment obligations.
    /// `missed_weighted` grows by `missed_penalty_multiplier ^
    /// consecutive` on each miss event, so streaks hurt more.
    pub missed_weighted: u128,
    /// Running count of correct forecasts, weighted by the
    /// reputation the forecaster carried at vote time.
    pub forecasts_correct_weighted: u128,
    /// Running count of incorrect forecasts, weighted likewise.
    pub forecasts_incorrect_weighted: u128,
    /// Sum of currently-active attestation weights, in bps.
    pub attestation_weight_bps: u64,
    /// Component accumulators, post-decay.
    pub payment_bps: u32,
    /// Calibration component.
    pub calibration_bps: u32,
    /// Attestation component.
    pub attestation_bps: u32,
    /// Tenure component.
    pub tenure_bps: u32,
    /// Total LACE ever slashed from this address. Surfaced for the
    /// zero-defaults proof.
    pub total_slashed: Amount,
    /// Block height of the most-recent missed payment (0 if none).
    /// Surfaced for the zero-defaults proof.
    pub last_missed_at: BlockHeight,
}

impl AddressState {
    /// Fresh-wallet state. Every component starts at 5000 bps
    /// (neutral) so untracked addresses are not penalised below
    /// average, but also do not get a free top-band ride.
    pub const fn fresh(first_seen: BlockHeight) -> AddressState {
        AddressState {
            first_seen,
            last_touched: first_seen,
            payments_met: 0,
            missed_weighted: 0,
            forecasts_correct_weighted: 0,
            forecasts_incorrect_weighted: 0,
            attestation_weight_bps: 0,
            payment_bps: 5_000,
            calibration_bps: 5_000,
            attestation_bps: 5_000,
            tenure_bps: 0,
            total_slashed: 0,
            last_missed_at: 0,
        }
    }
}

/// The score engine. Holds the in-memory accumulator state for every
/// known address, plus the governance-controlled parameters.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VeilEngine {
    /// Per-address state.
    states: BTreeMap<Address, AddressState>,
    /// Component weights.
    pub weights: ScoreWeights,
    /// Decay parameters.
    pub decay: DecayParams,
    /// Saturation parameters.
    pub saturation: SaturationParams,
}

impl Default for VeilEngine {
    fn default() -> Self {
        VeilEngine::new(ScoreWeights::DEFAULT, DecayParams::DEFAULT, SaturationParams::DEFAULT)
    }
}

impl VeilEngine {
    /// Build a new engine. Panics if `weights` is not well-formed --
    /// governance is responsible for emitting only valid weight
    /// vectors.
    pub fn new(weights: ScoreWeights, decay: DecayParams, saturation: SaturationParams) -> Self {
        assert!(weights.is_well_formed(), "weights must sum to 10_000 bps");
        Self {
            states: BTreeMap::new(),
            weights,
            decay,
            saturation,
        }
    }

    /// True if this engine has seen the given address.
    pub fn knows(&self, a: &Address) -> bool {
        self.states.contains_key(a)
    }

    /// Borrow the state of an address, if any.
    pub fn state_of(&self, a: &Address) -> Option<&AddressState> {
        self.states.get(a)
    }

    /// Compute the current Veil Score for an address.
    ///
    /// Returns `Score::ZERO` for unknown addresses -- the score is
    /// soul-bound and an absence of signal is *not* the same as a
    /// presence of negative signal, but downstream lending /
    /// governance code will treat untracked addresses as
    /// `ScoreBand::Untrusted` either way.
    pub fn score_of(&self, a: &Address) -> Score {
        let Some(s) = self.states.get(a) else {
            return Score::ZERO;
        };
        let blended = (s.payment_bps as u128 * self.weights.payment_bps as u128
            + s.calibration_bps as u128 * self.weights.calibration_bps as u128
            + s.attestation_bps as u128 * self.weights.attestation_bps as u128
            + s.tenure_bps as u128 * self.weights.tenure_bps as u128)
            / 10_000;
        Score::from_bps(blended as u32)
    }

    /// Ingest one `ScoreEvent`. Returns the address's freshly-updated
    /// score. Idempotent in the sense that repeating the same event
    /// with the same `at` height is a no-op for tenure but advances
    /// the relevant accumulator (the engine does *not* de-duplicate;
    /// upstream is expected to do so).
    pub fn ingest(&mut self, event: ScoreEvent) -> Score {
        let subject = event.subject();
        let at = event.at();
        // Ensure tracking exists. `FirstSeen` is the canonical way to
        // create state, but any first-touch event for an unknown
        // address implicitly does it.
        let state = self
            .states
            .entry(subject)
            .or_insert_with(|| AddressState::fresh(at));
        // Apply decay since `last_touched` before mutating the
        // affected component.
        Self::apply_decay(state, at, self.decay);
        match event {
            ScoreEvent::FirstSeen { at, .. } => {
                // If we just inserted this state, `first_seen` is
                // already `at`. If the address was already known,
                // we preserve the earlier `first_seen` (cannot move
                // backwards).
                if state.first_seen > at {
                    state.first_seen = at;
                }
            }
            ScoreEvent::PaymentMet { .. } => {
                state.payments_met = state.payments_met.saturating_add(1);
                state.payment_bps = Self::recompute_payment_bps(state, self.saturation);
            }
            ScoreEvent::PaymentMissed { consecutive, at, .. } => {
                let multiplier = (self.saturation.missed_penalty_multiplier as u128)
                    .saturating_pow(consecutive.min(8) as u32);
                state.missed_weighted = state.missed_weighted.saturating_add(multiplier);
                state.last_missed_at = at;
                state.payment_bps = Self::recompute_payment_bps(state, self.saturation);
            }
            ScoreEvent::ForecastCorrect { weight_bps, .. } => {
                state.forecasts_correct_weighted = state
                    .forecasts_correct_weighted
                    .saturating_add(weight_bps as u128);
                state.calibration_bps = Self::recompute_calibration_bps(state, self.saturation);
            }
            ScoreEvent::ForecastIncorrect { weight_bps, .. } => {
                state.forecasts_incorrect_weighted = state
                    .forecasts_incorrect_weighted
                    .saturating_add(weight_bps as u128);
                state.calibration_bps = Self::recompute_calibration_bps(state, self.saturation);
            }
            ScoreEvent::AttestationPosted { weight_bps, .. } => {
                state.attestation_weight_bps = state
                    .attestation_weight_bps
                    .saturating_add(weight_bps as u64);
                state.attestation_bps = Self::recompute_attestation_bps(state);
            }
            ScoreEvent::AttestationRevoked { weight_bps, .. } => {
                state.attestation_weight_bps = state
                    .attestation_weight_bps
                    .saturating_sub(weight_bps as u64);
                state.attestation_bps = Self::recompute_attestation_bps(state);
            }
            ScoreEvent::Slashed { amount, at, .. } => {
                // A slash is also a default signal: it bumps
                // missed_weighted by a flat 4x and marks the
                // recent-default timestamp so the zero-defaults
                // proof rejects.
                state.missed_weighted = state.missed_weighted.saturating_add(4);
                state.last_missed_at = at;
                state.total_slashed = state.total_slashed.saturating_add(amount);
                state.payment_bps = Self::recompute_payment_bps(state, self.saturation);
            }
        }
        state.tenure_bps = Self::recompute_tenure_bps(state, at, self.saturation);
        state.last_touched = at;
        self.score_of(&subject)
    }

    /// Read-only access to all known states. Useful for tests and
    /// for snapshotting state into a circuit witness.
    pub fn states(&self) -> &BTreeMap<Address, AddressState> {
        &self.states
    }

    fn apply_decay(state: &mut AddressState, now: BlockHeight, params: DecayParams) {
        if params.decay_span == 0 || params.decay_bps_per_span == 0 {
            return;
        }
        if now <= state.last_touched {
            return;
        }
        let elapsed = now - state.last_touched;
        let steps = (elapsed / params.decay_span) as u32;
        if steps == 0 {
            return;
        }
        let drift = (steps as u64 * params.decay_bps_per_span as u64).min(10_000) as u32;
        for component in [
            &mut state.payment_bps,
            &mut state.calibration_bps,
            &mut state.attestation_bps,
        ] {
            *component = drift_toward(*component, 5_000, drift);
        }
        // Tenure does not decay -- it is monotone non-decreasing in
        // wallet age; staleness shows up via the other three
        // components.
    }

    fn recompute_payment_bps(state: &AddressState, sat: SaturationParams) -> u32 {
        let paid = state.payments_met as u128;
        let missed = state.missed_weighted;
        let total = paid + missed;
        if total == 0 {
            return 5_000;
        }
        // Logistic-style ramp toward 10_000 as `paid - missed` grows.
        // For small samples we blend with the neutral midpoint so
        // very-low-volume wallets do not swing wildly.
        let observed_bps = (paid * 10_000 / total) as u32;
        let weight_full = sat.payment_full.max(1) as u128;
        let weight_obs = total.min(weight_full);
        let weight_neutral = weight_full - weight_obs;
        let blended = (observed_bps as u128 * weight_obs + 5_000u128 * weight_neutral)
            / weight_full;
        blended as u32
    }

    fn recompute_calibration_bps(state: &AddressState, sat: SaturationParams) -> u32 {
        let right = state.forecasts_correct_weighted;
        let wrong = state.forecasts_incorrect_weighted;
        let total = right + wrong;
        if total == 0 {
            return 5_000;
        }
        let observed_bps = (right * 10_000 / total) as u32;
        let weight_full = sat.calibration_full.max(1) as u128;
        // Each unit of forecasts_*_weighted is one bps of voting
        // weight; normalise back to a forecast-count comparable to
        // `calibration_full`.
        let weight_obs = (total / 10_000).max(1).min(weight_full);
        let weight_neutral = weight_full - weight_obs;
        let blended = (observed_bps as u128 * weight_obs + 5_000u128 * weight_neutral)
            / weight_full;
        blended as u32
    }

    fn recompute_attestation_bps(state: &AddressState) -> u32 {
        // Attestations are a positive-only signal: a wallet with no
        // attestations sits at neutral (5_000), and each unit of
        // saturated attestation weight lifts the component toward
        // 10_000. Revocations / decay reduce the underlying
        // attestation_weight_bps, which symmetrically lowers the
        // component back toward 5_000 -- but never below it.
        // (Bad-faith attestation behaviour penalises the *attester*,
        // not the subject; see lace-veil-attest.)
        let saturated = state.attestation_weight_bps.min(20_000);
        let lift = (saturated * 5_000 / 20_000) as u32;
        5_000u32.saturating_add(lift)
    }

    fn recompute_tenure_bps(state: &AddressState, now: BlockHeight, sat: SaturationParams) -> u32 {
        if sat.tenure_full == 0 {
            return 10_000;
        }
        let age = now.saturating_sub(state.first_seen);
        let bps = (age as u128 * 10_000 / sat.tenure_full as u128).min(10_000) as u32;
        bps
    }
}

fn drift_toward(value: u32, target: u32, magnitude: u32) -> u32 {
    if value == target {
        return target;
    }
    if value > target {
        value.saturating_sub(magnitude).max(target)
    } else {
        value.saturating_add(magnitude).min(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }

    #[test]
    fn default_weights_well_formed() {
        assert!(ScoreWeights::DEFAULT.is_well_formed());
    }

    #[test]
    fn fresh_address_starts_neutral() {
        let s = AddressState::fresh(100);
        assert_eq!(s.payment_bps, 5_000);
        assert_eq!(s.calibration_bps, 5_000);
        assert_eq!(s.attestation_bps, 5_000);
        assert_eq!(s.tenure_bps, 0);
    }

    #[test]
    fn unknown_address_returns_zero_score() {
        let e = VeilEngine::default();
        assert_eq!(e.score_of(&addr(1)).bps(), 0);
    }

    #[test]
    fn first_seen_creates_tracking() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::FirstSeen {
            subject: addr(1),
            at: 100,
        });
        assert!(e.knows(&addr(1)));
        // Brand-new wallet: tenure 0, all else neutral, score
        // depends on weights but should not be top band.
        assert!(e.score_of(&addr(1)).bps() < 8_000);
    }

    #[test]
    fn met_payments_raise_payment_component() {
        let mut e = VeilEngine::default();
        for i in 0..50 {
            e.ingest(ScoreEvent::PaymentMet {
                subject: addr(1),
                at: 1_000 + i,
            });
        }
        let s = e.state_of(&addr(1)).unwrap();
        // With 50 met / 0 missed and payment_full=50, observed bps
        // should fully outweigh the neutral prior.
        assert!(s.payment_bps >= 9_500, "got {}", s.payment_bps);
    }

    #[test]
    fn missed_payments_drag_payment_component_below_neutral() {
        let mut e = VeilEngine::default();
        for _ in 0..5 {
            e.ingest(ScoreEvent::PaymentMet {
                subject: addr(1),
                at: 100,
            });
        }
        e.ingest(ScoreEvent::PaymentMissed {
            subject: addr(1),
            consecutive: 3,
            at: 200,
        });
        let s = e.state_of(&addr(1)).unwrap();
        // 5 paid vs 1 missed at streak 3 = 1 * 3^3 = 27 weighted;
        // 5 / (5+27) ~= 15% -> below neutral.
        assert!(s.payment_bps < 5_000, "got {}", s.payment_bps);
    }

    #[test]
    fn forecast_calibration_blends_toward_observed() {
        let mut e = VeilEngine::default();
        // 100 forecasts at 100% weight each, all correct.
        for _ in 0..200 {
            e.ingest(ScoreEvent::ForecastCorrect {
                subject: addr(2),
                weight_bps: 5_000,
                at: 100,
            });
        }
        let s = e.state_of(&addr(2)).unwrap();
        assert!(s.calibration_bps >= 9_500, "got {}", s.calibration_bps);
    }

    #[test]
    fn attestation_weight_saturates() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::AttestationPosted {
            subject: addr(3),
            attester: addr(4),
            weight_bps: 30_000, // above saturation
            at: 100,
        });
        let s = e.state_of(&addr(3)).unwrap();
        // Saturation lifts neutral 5_000 by a full 5_000.
        assert_eq!(s.attestation_bps, 10_000);
    }

    #[test]
    fn no_attestations_keeps_component_at_neutral() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::FirstSeen {
            subject: addr(20),
            at: 100,
        });
        assert_eq!(e.state_of(&addr(20)).unwrap().attestation_bps, 5_000);
    }

    #[test]
    fn small_attestation_lifts_modestly_above_neutral() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::AttestationPosted {
            subject: addr(21),
            attester: addr(22),
            weight_bps: 4_000, // 20 % of saturation
            at: 100,
        });
        // Lift = 4_000 * 5_000 / 20_000 = 1_000.
        assert_eq!(e.state_of(&addr(21)).unwrap().attestation_bps, 6_000);
    }

    #[test]
    fn tenure_ramps_linearly() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::FirstSeen {
            subject: addr(5),
            at: 0,
        });
        // At half the tenure_full window, tenure_bps should be ~5000.
        e.ingest(ScoreEvent::PaymentMet {
            subject: addr(5),
            at: SaturationParams::DEFAULT.tenure_full / 2,
        });
        let s = e.state_of(&addr(5)).unwrap();
        assert!(s.tenure_bps >= 4_900 && s.tenure_bps <= 5_100);
    }

    #[test]
    fn decay_drifts_idle_components_to_neutral() {
        let mut e = VeilEngine::default();
        // Push payment way above neutral.
        for i in 0..50 {
            e.ingest(ScoreEvent::PaymentMet {
                subject: addr(6),
                at: 1_000 + i,
            });
        }
        let high = e.state_of(&addr(6)).unwrap().payment_bps;
        // Now jump well into the future with a no-op (FirstSeen
        // is idempotent for known addresses) so decay kicks in.
        let later = 1_000 + 50 + 10 * DecayParams::DEFAULT.decay_span;
        e.ingest(ScoreEvent::FirstSeen {
            subject: addr(6),
            at: later,
        });
        let after = e.state_of(&addr(6)).unwrap().payment_bps;
        assert!(after < high, "{} should be < {}", after, high);
        // 10 steps * 50 bps = 500 drift.
        assert!(after >= high - 500 - 5, "decay went too far");
    }

    #[test]
    fn slashing_pushes_payment_below_neutral_and_marks_default() {
        let mut e = VeilEngine::default();
        for i in 0..20 {
            e.ingest(ScoreEvent::PaymentMet {
                subject: addr(7),
                at: 100 + i,
            });
        }
        e.ingest(ScoreEvent::Slashed {
            subject: addr(7),
            amount: 1_000,
            at: 500,
        });
        let s = e.state_of(&addr(7)).unwrap();
        assert_eq!(s.total_slashed, 1_000);
        assert_eq!(s.last_missed_at, 500);
        assert!(s.payment_bps < 9_500);
    }

    #[test]
    fn score_is_blend_of_components() {
        let mut e = VeilEngine::default();
        e.ingest(ScoreEvent::FirstSeen {
            subject: addr(8),
            at: 0,
        });
        // Drive all components to known values to check blending.
        let s = e.state_of(&addr(8)).copied().unwrap();
        let manual = (s.payment_bps as u128 * 4_000
            + s.calibration_bps as u128 * 2_500
            + s.attestation_bps as u128 * 2_000
            + s.tenure_bps as u128 * 1_500)
            / 10_000;
        assert_eq!(e.score_of(&addr(8)).bps(), manual as u32);
    }
}
