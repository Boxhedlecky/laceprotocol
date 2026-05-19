# Prediction Market Engine — Technical Specification

> Component 3 of 5. Owns market shapes, the LMSR AMM, oracle resolution,
> composability interfaces, and prediction-gated governance.

## 1. Goals and non-goals

### Goals

1. Quote a live probability for every supported market shape, continuously, with bounded protocol exposure.
2. Resolve subjective outcomes through a validator + reputation-weighted vote, with a real dispute path.
3. Expose a stable composability surface so loans (Component 5), timelocks (Component 2), and governance can consume market state without coupling to AMM internals.
4. Keep the trader's identity off this layer entirely — positions are referenced by opaque commitment hashes provided by the privacy layer (Component 1).

### Non-goals

- Settling payouts. Payout descriptors are produced; the privacy layer settles them.
- Maintaining the off-chain question text. Only a `question_hash` is stored.
- Oracle for *objective* price feeds. Lace markets resolve subjective outcomes (events, attestations). Objective price feeds belong in a separate oracle component or are wrapped as binary "did price > X at time T" markets.

## 2. Market shapes

Four shapes, defined in [`lace-pm-markets`](crates/lace-pm-markets/src/lib.rs):

| Shape | `n_outcomes()` | Resolution |
| --- | --- | --- |
| **Binary** | 2 | One of `OutcomeId::YES` / `OutcomeId::NO` |
| **Scalar** | 2 (long/short) | A value within `[lo, hi]`; projected to a probability via `scalar_to_probability` |
| **MultiOutcome** | N (≥2) | Exactly one of the enumerated `OutcomeId`s |
| **Conditional** | inner | Voids if parent resolves any way other than `parent_outcome`; otherwise resolves as `inner` |

Conditionals cannot nest (rejected at construction with `MarketError::NestedConditional`). Multi-level dependency is achieved by chaining `createConditionalTrigger` callbacks rather than nesting market definitions.

### State machine

```
        Open ─── enter_resolution_window ──▶ ResolutionWindow
          │                                       │
          │                                       │ report_resolution
          │                                       ▼
          │                                  Disputed ─── finalize ──▶ Resolved
          │                                       │
          │                                       │ dispute upheld
          │                                       ▼
          └────────── void ───────────────────▶ Voided
```

`Voided` and `Resolved` are terminal. Both `Voided` and `Resolved` markets expose a final outcome to downstream readers via the composability surface (`Voided` returns `None`).

## 3. AMM choice: LMSR over CPMM

Both LMSR and CPMM were considered. The trade-off matrix:

| Property | LMSR | CPMM |
| --- | --- | --- |
| Liquidity at market creation | ✅ Subsidised by `b` | ❌ Requires LP capital |
| Bounded protocol loss | ✅ `b · ln(n)` | ❌ Divergence loss |
| Closed-form probability | ✅ `p_i = exp(q_i/b) / Σ exp(q_j/b)` | ⚠️ Derived from reserves |
| Performance on long-tail markets | ✅ Excellent | ❌ Tiny LP pools mean huge slippage |
| LP yield narrative | ⚠️ Weak | ✅ Strong |

Markets on Lace are protocol infrastructure pricing uncertainty for loans, timelocks, governance, and oracles. The engine *must* keep quoting on rarely-traded markets (e.g. "did validator X equivocate in epoch 942?") and *must* keep its own exposure bounded. The first four properties are load-bearing for that mission; the fifth is not.

Decision: **LMSR**, with a fee-routed liquidity reserve (`liquidity_bps` of every collected trade fee) that tops up subsidy budgets so the protocol does not have to allocate fresh capital each market.

### LMSR mechanics

Cost function: `C(q) = b · ln(Σ_i exp(q_i / b))`.

Marginal price of outcome `i`: `p_i(q) = exp(q_i / b) / Σ_j exp(q_j / b)`.

Trader pays `C(q + Δ·e_i) − C(q)` to receive `Δ` shares of outcome `i`.

Worst-case loss to the protocol over a market's lifetime: `b · ln(n)` where `n` is the number of outcomes.

Liquidity provision: increasing `b → b + Δb` while rescaling `q ← q · (b + Δb) / b` preserves current prices and requires a top-up of `C(q_new, b_new) − C(q_old, b_old)` from the provider.

### Numerics

Reference implementation uses `f64`. Annotated with `// TODO(consensus-fp)` at each non-trivial floating-point operation. Production consensus settlement replaces these with fixed-point Q64.64; the algorithm is unchanged.

## 4. Fees

`FeeSchedule` (in `lace-pm-types`):

- `trade_bps` — total trade fee in basis points (default 30).
- `burn_bps / validator_bps / resolution_bps / liquidity_bps` — sub-bps that sum to 10 000 and route the collected trade fee into four sinks.

Defaults:

| Sink | Share | Purpose |
| --- | --- | --- |
| Burn | 40 % | Deflationary pressure on LACE supply |
| Validator | 25 % | Validator reward augmentation |
| Resolution | 15 % | Backs slashing of bad-faith resolvers |
| Liquidity | 20 % | Replenishes the LMSR subsidy reserve |

The resolution sink absorbs any rounding remainder so the four-way split conserves total fee exactly.

## 5. Oracle / resolution architecture

Each resolution proceeds in three stages:

1. **Resolution window** (length: `resolution_window_blocks`)
   - Validators submit votes weighted by their staked LACE.
   - Forecasters submit votes weighted by their Veil Score reputation.
   - Combined weight `W(outcome) = α · stake_weight + (1 − α) · reputation_weight`. `α` is on-chain-governance-controlled (default `0.6`).
   - At window close, the outcome with the largest combined weight is *provisionally* reported.

2. **Dispute window** (length: `dispute_window_blocks`)
   - Any participant may post a dispute bond and challenge the provisional resolution.
   - The dispute pays the resolution sink if upheld (bond burns), or refunds the challenger if successful (and slashes the resolvers who voted the losing way).
   - During this window the market transitions to `Disputed`.

3. **Finalisation**
   - If the dispute window closes uncontested, `finalize()` transitions to `Resolved` with the provisional outcome.
   - If a dispute opened a second resolution round, the round runs to its end and finalises whichever way it resolves.

Veil Score integration: every resolver's vote is later compared to the finalised outcome. Votes that matched feed `ReputationEvent::ResolverCorrect`; votes that didn't feed `ReputationEvent::ResolverIncorrect`. Component 4 consumes these to update scores. The same calibration history feeds back into future weight calculations.

## 6. Composability surface

Defined in [`lace-pm-compose`](crates/lace-pm-compose). The contract is intentionally narrow:

```rust
trait ProbabilityFeed {
    fn get_live_probability(&self, market: MarketId, outcome: OutcomeId) -> Option<Probability>;
    fn get_resolved_outcome(&self, market: MarketId) -> Option<OutcomeId>;
    fn create_conditional_trigger(
        &mut self,
        market: MarketId,
        expected: OutcomeId,
        callback: TriggerCallback,
    ) -> TriggerId;
}
```

In addition, `lace-pm-compose` implements `OracleResolver` (defined in the temporal-VM crate `lace-conditions`) so timelock conditions can be gated on market outcomes:

```rust
impl OracleResolver for ComposeFacade {
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer {
        // The oracle hash is interpreted as a MarketId.
        // Returns `Resolved(outcome_hash)`, `Pending`, or `Voided`.
    }
}
```

Triggers fire idempotently: once a market resolves the expected way, all registered triggers fire; once any trigger has fired, repeated `tick()`s are no-ops for that trigger.

## 7. Prediction-gated governance

Defined in [`lace-pm-governance`](crates/lace-pm-governance).

A governance proposal binds an upgrade payload to a binary market: "will the network adopt this upgrade safely within window W?" The proposal executes iff the market clears an *adoption threshold* (default 65 % YES at window close) **and** trading volume exceeds a *liquidity threshold* (so a thin market can't push through an upgrade).

Parameter governance covers:

- `FeeSchedule` — trade and split bps.
- `alpha` — stake vs reputation weight in the resolution combiner.
- `resolution_window_blocks` / `dispute_window_blocks` defaults.
- Liquidity-subsidy `b` defaults for newly-created markets.

All parameters are stored as `GovernanceParams` and updates require a clean prediction-gated proposal.

## 8. Privacy

Trades on this layer never expose the trader's address. Each trade carries an opaque `position_commitment: Bytes32` produced by the privacy layer. The AMM stores it but never inspects it; the rest of the engine treats it as a primary key only.

Consequences:

- **Position privacy.** A balance sheet for a single trader cannot be reconstructed from on-chain state alone — it requires decryption keys held by the trader.
- **Resolver privacy.** Resolver votes carry stake/reputation *weights*, not addresses. The privacy layer's threshold disclosure key is what lets a resolver prove "I had at least W stake" without revealing the exact stake.
- **Privacy of resolutions.** Resolutions themselves are public, because downstream timelocks and loans need to read them. Privacy is at the *position* level, not the *outcome* level.

## 9. Tests

- **`lace-pm-types`** — fee routing well-formedness, probability clamping, outcome identifier distinctness.
- **`lace-pm-markets`** — every state transition, every shape's input validation, scalar projection, nested-conditional rejection.
- **`lace-pm-amm`** — initial 50/50 prices, price-sum invariant after arbitrary trades, monotonicity (buying raises price), LMSR worst-case bound, slippage scaling with `b`, lossless pre-fee round trip, liquidity provision price preservation, position-commitment opacity.
- **`lace-pm-oracle`** — validator+reputation weighting, dispute window timing, escalation, voided market path.
- **`lace-pm-compose`** — `OracleResolver` impl mirrors final outcome, conditional triggers fire exactly once.
- **`tests/` (workspace integration)** — full lifecycle across all crates with a mocked privacy layer, a mocked temporal VM, and a mocked Veil Score sink.

## 10. Threat model

Defended against:

- **Oracle manipulation by a minority validator coalition.** The reputation half of the combiner prevents capture; the dispute window catches obvious collusion.
- **Wash-trading to manipulate probability before resolution.** LMSR cost scales convexly with `q`; large pre-resolution probability swings are expensive. The fee schedule's burn share also taxes wash volume.
- **Fee siphoning.** All collected fees route deterministically into four sinks; no operator-controlled wallet sits between the trade and the sinks.
- **Identity leakage via the AMM.** The AMM never sees an address; only an opaque commitment.

**Not** defended against:

- A majority validator coalition acting in concert with a majority reputation coalition.
- Off-chain endpoint compromise of a resolver.
- Insider knowledge of a market's outcome before public information (this is a market design problem, not a protocol problem).
