//! LMSR (Logarithmic Market Scoring Rule) automated market maker.
//!
//! # Why LMSR over CPMM
//!
//! The Lace prediction-market engine deliberately chose LMSR over a
//! constant-product market maker. The trade-off matrix:
//!
//! | Property | LMSR | CPMM |
//! | --- | --- | --- |
//! | Liquidity from genesis | yes, via subsidy `b` | no, needs LP capital |
//! | Bounded worst-case loss for the protocol | yes, `b * ln(n)` | no, divergence loss |
//! | Analytical price extraction | clean closed form | requires reserve ratios |
//! | Long-tail / governance markets | excellent | poor (tiny LP pools) |
//! | LP yield narrative | weak | strong |
//!
//! Markets on Lace are *infrastructure* -- they price uncertainty for
//! the loan, timelock, governance, and oracle layers. That use case
//! values the first four properties strongly and the fifth weakly:
//! the protocol must keep quoting on rarely-traded markets (e.g. "did
//! validator X equivocate in epoch 942?") and must keep its own
//! exposure bounded. LMSR is the right answer.
//!
//! The fee-routed liquidity reserve (`liquidity_bps` in the fee
//! schedule) tops up the subsidy budget so the protocol does not have
//! to allocate fresh capital on every market.
//!
//! # Numerics
//!
//! This implementation uses `f64` for the cost function. That is good
//! enough for an off-chain reference quoter and for tests, but for
//! consensus settlement a fixed-point Q64.64 rewrite is required so
//! every node computes the exact same cost across architectures. The
//! algorithm is unchanged; only the arithmetic substrate changes.
//! This boundary is annotated with `// TODO(consensus-fp)` comments
//! at each non-trivial floating-point operation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use lace_pm_markets::MarketKind;
use lace_pm_types::{Amount, Bytes32, FeeRouting, FeeSchedule, Probability};
use serde::{Deserialize, Serialize};

/// LMSR state for a single market.
///
/// Holds the per-outcome share count vector `q`, the liquidity
/// parameter `b`, and the cumulative collected-fee buckets. The
/// market state machine in `lace-pm-markets` owns lifecycle; this
/// struct owns *pricing*.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LmsrState {
    /// Per-outcome share count `q_i`. Stored as `f64` for the
    /// reference implementation; see the module-level note on the
    /// consensus rewrite.
    pub q: Vec<f64>,
    /// LMSR liquidity parameter. Larger `b` -> flatter book ->
    /// smaller price impact per trade and higher worst-case subsidy.
    pub b: f64,
    /// Cumulative LMSR cost paid into the pool over the market's
    /// lifetime. Always non-decreasing.
    pub cumulative_cost: f64,
    /// Cumulative fee, in LACE units, collected from trades. This is
    /// what the fee router consumes.
    pub cumulative_fee_collected: Amount,
}

impl LmsrState {
    /// Build a fresh AMM state for a market shape with the given
    /// liquidity parameter.
    pub fn new(kind: &MarketKind, b: f64) -> Self {
        let n = kind.n_outcomes();
        let q = alloc::vec![0.0; n];
        let cumulative_cost = lmsr_cost(&q, b);
        Self {
            q,
            b,
            cumulative_cost,
            cumulative_fee_collected: 0,
        }
    }

    /// Number of distinct outcomes this state is pricing.
    pub fn n(&self) -> usize {
        self.q.len()
    }

    /// Per-outcome marginal price (probability) vector. Sums to 1.0
    /// up to floating-point error.
    pub fn prices(&self) -> Vec<f64> {
        lmsr_prices(&self.q, self.b)
    }

    /// Probability of outcome `i` as a basis-point integer.
    pub fn probability(&self, i: usize) -> Probability {
        let prices = self.prices();
        Probability::from_f64(*prices.get(i).unwrap_or(&0.0))
    }

    /// Quote the cost of buying `delta` shares of outcome `i`. Does
    /// not mutate state.
    ///
    /// `delta` may be negative (selling). The result is the cost the
    /// trader pays the pool: positive on buys, negative (i.e. a
    /// payout) on sells.
    pub fn quote(&self, i: usize, delta: f64) -> Quote {
        let mut q_after = self.q.clone();
        q_after[i] += delta;
        let after = lmsr_cost(&q_after, self.b);
        let before = self.cumulative_cost;
        let cost = after - before;
        let prices_before = lmsr_prices(&self.q, self.b);
        let prices_after = lmsr_prices(&q_after, self.b);
        Quote {
            outcome: i,
            delta,
            cost,
            price_before: prices_before[i],
            price_after: prices_after[i],
        }
    }

    /// Execute a trade. Mutates `q`, advances `cumulative_cost`,
    /// applies fees from `fees`, and returns the trade receipt.
    ///
    /// `position_commitment` is the *opaque* commitment hash from the
    /// privacy layer identifying this trade's shielded position note.
    /// The AMM never sees the trader's address, only this hash.
    pub fn execute(
        &mut self,
        outcome: usize,
        delta: f64,
        fees: FeeSchedule,
        position_commitment: Bytes32,
    ) -> Result<TradeReceipt, AmmError> {
        if outcome >= self.n() {
            return Err(AmmError::UnknownOutcome);
        }
        if !delta.is_finite() {
            return Err(AmmError::NonFiniteDelta);
        }
        // Reject trades that would push share count negative (selling
        // more than exists). LMSR can technically extend below zero,
        // but for a *protocol-grade* market we constrain this so
        // private positions can't double-spend their shares.
        if self.q[outcome] + delta < 0.0 {
            return Err(AmmError::Underflow);
        }
        let q_before = self.q.clone();
        self.q[outcome] += delta;
        let cost_after = lmsr_cost(&self.q, self.b);
        let raw_cost = cost_after - self.cumulative_cost;
        // Fees apply only to the *cost the trader pays* (raw_cost > 0).
        // On a sell (raw_cost < 0) the trader receives a payout net of
        // a symmetric fee taken from the payout side.
        let (gross, fee_bps_amt) = apply_fee(raw_cost, fees);
        // Saturating conversion to integer LACE units. The AMM is
        // priced in LACE; out-of-circuit f64 -> Amount rounds toward
        // zero.
        let fee_amount: Amount = clamp_to_amount(fee_bps_amt.abs());
        self.cumulative_cost = cost_after;
        self.cumulative_fee_collected =
            self.cumulative_fee_collected.saturating_add(fee_amount);
        let routing = fees.route(fee_amount);
        Ok(TradeReceipt {
            outcome,
            delta,
            position_commitment,
            raw_cost,
            gross_cost: gross,
            fee_amount,
            fee_routing: routing,
            q_before,
            q_after: self.q.clone(),
            price_after: lmsr_prices(&self.q, self.b)[outcome],
        })
    }

    /// Apply a *liquidity provision* event: increase `b` by `delta_b`
    /// while preserving prices.
    ///
    /// LMSR is unusual in that liquidity is governed by `b` not by
    /// reserves. To preserve current prices after raising `b`, the
    /// q-vector must be rescaled proportionally. Returns the change
    /// in subsidised cost (the deposit the liquidity provider must
    /// fund to take `b` from `b` to `b + delta_b`).
    pub fn provide_liquidity(&mut self, delta_b: f64) -> Result<f64, AmmError> {
        if delta_b <= 0.0 || !delta_b.is_finite() {
            return Err(AmmError::NonFiniteDelta);
        }
        let old_b = self.b;
        let new_b = old_b + delta_b;
        let scale = new_b / old_b;
        for q in self.q.iter_mut() {
            *q *= scale;
        }
        self.b = new_b;
        let new_cost = lmsr_cost(&self.q, self.b);
        let deposit = new_cost - self.cumulative_cost;
        self.cumulative_cost = new_cost;
        Ok(deposit)
    }
}

/// A quote -- the result of pricing a trade without executing it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Quote {
    /// Outcome index priced.
    pub outcome: usize,
    /// Share delta the quote is for.
    pub delta: f64,
    /// Pool cost of the trade (positive = trader pays, negative =
    /// trader receives).
    pub cost: f64,
    /// Marginal price before the trade.
    pub price_before: f64,
    /// Marginal price after the trade.
    pub price_after: f64,
}

impl Quote {
    /// Slippage of this quote, measured as the absolute change in
    /// marginal probability that the trade causes.
    pub fn slippage(&self) -> f64 {
        (self.price_after - self.price_before).abs()
    }

    /// Effective price the trader paid per share. Defined only when
    /// `delta != 0`.
    pub fn effective_price(&self) -> Option<f64> {
        if self.delta == 0.0 {
            None
        } else {
            Some(self.cost / self.delta)
        }
    }
}

/// The result of an executed trade.
#[derive(Clone, Debug, PartialEq)]
pub struct TradeReceipt {
    /// Outcome index traded.
    pub outcome: usize,
    /// Share delta executed.
    pub delta: f64,
    /// Opaque hash of the shielded position note. The AMM never
    /// learns the trader's address.
    pub position_commitment: Bytes32,
    /// LMSR cost (pre-fee) of the trade.
    pub raw_cost: f64,
    /// Total cost paid (or payout received), post-fee. Buys are
    /// positive, sells are negative.
    pub gross_cost: f64,
    /// Fee taken from the trade, in LACE units.
    pub fee_amount: Amount,
    /// Routing of the fee into burn / validator / resolution /
    /// liquidity sinks.
    pub fee_routing: FeeRouting,
    /// q-vector pre-trade (preserved for audit).
    pub q_before: Vec<f64>,
    /// q-vector post-trade.
    pub q_after: Vec<f64>,
    /// Outcome's marginal price immediately after the trade.
    pub price_after: f64,
}

/// AMM errors.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AmmError {
    /// Outcome index out of range for this market shape.
    UnknownOutcome,
    /// Trade delta was not a finite float.
    NonFiniteDelta,
    /// Trade would have pushed share count below zero.
    Underflow,
}

/// LMSR cost function:  C(q) = b * ln(sum(exp(q_i / b))).
///
/// Numerically stabilised by subtracting `max(q_i / b)` before
/// `exp`, then adding it back outside the `ln`.
pub fn lmsr_cost(q: &[f64], b: f64) -> f64 {
    if q.is_empty() {
        return 0.0;
    }
    // TODO(consensus-fp): replace with fixed-point exp/ln.
    let scaled: Vec<f64> = q.iter().map(|&qi| qi / b).collect();
    let m = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let sum_exp: f64 = scaled.iter().map(|&s| (s - m).exp()).sum();
    b * (m + sum_exp.ln())
}

/// Per-outcome LMSR marginal prices.
pub fn lmsr_prices(q: &[f64], b: f64) -> Vec<f64> {
    if q.is_empty() {
        return Vec::new();
    }
    // TODO(consensus-fp): replace with fixed-point exp.
    let scaled: Vec<f64> = q.iter().map(|&qi| qi / b).collect();
    let m = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = scaled.iter().map(|&s| (s - m).exp()).collect();
    let sum: f64 = exps.iter().sum();
    exps.iter().map(|&e| e / sum).collect()
}

/// Apply a fee schedule to a raw cost. Returns `(gross_paid,
/// fee_taken)`. Mirrors the sign of `raw_cost`.
pub fn apply_fee(raw_cost: f64, fees: FeeSchedule) -> (f64, f64) {
    let bps = fees.trade_bps as f64 / 10_000.0;
    let fee = raw_cost.abs() * bps;
    let signed_fee = if raw_cost >= 0.0 { fee } else { -fee };
    (raw_cost + signed_fee, signed_fee)
}

fn clamp_to_amount(x: f64) -> Amount {
    if !x.is_finite() || x <= 0.0 {
        return 0;
    }
    if x >= u128::MAX as f64 {
        return u128::MAX;
    }
    x as Amount
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary_state(b: f64) -> LmsrState {
        LmsrState::new(&MarketKind::Binary, b)
    }

    #[test]
    fn fresh_binary_market_quotes_50_50() {
        let s = binary_state(100.0);
        let prices = s.prices();
        assert!((prices[0] - 0.5).abs() < 1e-9);
        assert!((prices[1] - 0.5).abs() < 1e-9);
        assert_eq!(s.probability(0), Probability::from_bps(5_000));
    }

    #[test]
    fn prices_sum_to_one_after_arbitrary_trade() {
        let mut s = binary_state(100.0);
        s.execute(0, 50.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let prices = s.prices();
        let sum: f64 = prices.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn buying_one_outcome_raises_its_price() {
        let mut s = binary_state(100.0);
        let before = s.prices()[0];
        s.execute(0, 50.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let after = s.prices()[0];
        assert!(after > before);
    }

    #[test]
    fn larger_b_means_smaller_slippage_for_same_trade() {
        let mut tight = LmsrState::new(&MarketKind::Binary, 10.0);
        let mut wide = LmsrState::new(&MarketKind::Binary, 1000.0);
        let q1 = tight.quote(0, 100.0);
        let q2 = wide.quote(0, 100.0);
        assert!(q2.slippage() < q1.slippage());
        // Suppress unused-mut warnings.
        let _ = (&mut tight, &mut wide);
    }

    #[test]
    fn round_trip_buy_then_sell_is_lossless_pre_fee() {
        let mut s = binary_state(100.0);
        let cost_in = s.quote(0, 25.0).cost;
        s.execute(0, 25.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let cost_out = s.quote(0, -25.0).cost;
        // Pre-fee, round-trip net cost is approximately zero.
        assert!((cost_in + cost_out).abs() < 1e-9);
    }

    #[test]
    fn cannot_sell_more_than_exists() {
        let mut s = binary_state(100.0);
        let err = s
            .execute(0, -10.0, FeeSchedule::DEFAULT, Bytes32::ZERO)
            .unwrap_err();
        assert_eq!(err, AmmError::Underflow);
    }

    #[test]
    fn rejects_unknown_outcome() {
        let mut s = binary_state(100.0);
        let err = s
            .execute(5, 10.0, FeeSchedule::DEFAULT, Bytes32::ZERO)
            .unwrap_err();
        assert_eq!(err, AmmError::UnknownOutcome);
    }

    #[test]
    fn rejects_non_finite_delta() {
        let mut s = binary_state(100.0);
        let err = s
            .execute(0, f64::NAN, FeeSchedule::DEFAULT, Bytes32::ZERO)
            .unwrap_err();
        assert_eq!(err, AmmError::NonFiniteDelta);
    }

    #[test]
    fn worst_case_subsidy_bounded_by_b_ln_n() {
        // Sanity check the LMSR subsidy bound. With b=100 and a
        // binary market the worst the market maker ever pays out
        // beyond fees is b * ln(2) ~= 69.31.
        let s = binary_state(100.0);
        let initial_cost = s.cumulative_cost;
        let bound = 100.0 * (2f64.ln());
        // Push q[0] to +infinity (cap at 10_000) to approach the
        // bound: marginal price -> 1.0, total cost converges to b * ln(n).
        let mut sweep = s.clone();
        sweep.execute(0, 10_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let cost_paid_out = sweep.cumulative_cost - initial_cost;
        // The trader paid `cost_paid_out` to buy 10_000 shares; the
        // protocol's worst-case net is what it would owe out once
        // outcome 0 happens (10_000 shares * 1 LACE = 10_000) minus
        // what it took in (cost_paid_out).
        let payout = 10_000.0;
        let net = payout - cost_paid_out;
        assert!(
            net <= bound + 1e-3,
            "LMSR bound violated: net={} bound={}",
            net,
            bound
        );
    }

    #[test]
    fn liquidity_provision_preserves_prices() {
        let mut s = binary_state(100.0);
        s.execute(0, 50.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let prices_before = s.prices();
        let _deposit = s.provide_liquidity(500.0).unwrap();
        let prices_after = s.prices();
        for (a, b) in prices_before.iter().zip(prices_after.iter()) {
            assert!((a - b).abs() < 1e-9, "price drifted: {} -> {}", a, b);
        }
    }

    #[test]
    fn liquidity_provision_increases_b() {
        let mut s = binary_state(100.0);
        s.provide_liquidity(50.0).unwrap();
        assert!((s.b - 150.0).abs() < 1e-9);
    }

    #[test]
    fn fees_are_collected_on_buy() {
        // Realistic mainnet-scale market: b in the millions of
        // indivisible LACE units so that 30bps trade fees floor to a
        // meaningful integer amount.
        let mut s = LmsrState::new(&MarketKind::Binary, 1_000_000.0);
        let receipt = s
            .execute(0, 100_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO)
            .unwrap();
        assert!(receipt.fee_amount > 0);
        let routed = receipt.fee_routing.burn
            + receipt.fee_routing.validator
            + receipt.fee_routing.resolution
            + receipt.fee_routing.liquidity;
        assert_eq!(routed, receipt.fee_amount);
    }

    #[test]
    fn multi_outcome_initial_prices_uniform() {
        let kind = MarketKind::MultiOutcome {
            outcomes: vec![
                lace_pm_types::OutcomeId(Bytes32([1u8; 32])),
                lace_pm_types::OutcomeId(Bytes32([2u8; 32])),
                lace_pm_types::OutcomeId(Bytes32([3u8; 32])),
                lace_pm_types::OutcomeId(Bytes32([4u8; 32])),
            ],
        };
        let s = LmsrState::new(&kind, 100.0);
        let prices = s.prices();
        for p in &prices {
            assert!((p - 0.25).abs() < 1e-9);
        }
    }

    #[test]
    fn position_commitment_is_opaque_to_amm() {
        // The AMM stores the commitment but never inspects it. This
        // test enforces that property by feeding garbage and
        // confirming nothing breaks.
        let mut s = binary_state(100.0);
        let receipt = s
            .execute(0, 10.0, FeeSchedule::DEFAULT, Bytes32([0xAB; 32]))
            .unwrap();
        assert_eq!(receipt.position_commitment.0, [0xAB; 32]);
    }
}
