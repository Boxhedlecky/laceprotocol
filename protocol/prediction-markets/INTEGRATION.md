# Integration Guide

This document is the contract surface between the prediction-market engine (Component 3) and the rest of the Lace Protocol. Read it before changing any public API in this workspace.

## Cross-component matrix

| Consumer | Crate it imports from here | Trait / function consumed | Direction |
| --- | --- | --- | --- |
| Privacy (C1) | `lace-pm-amm` | `LmsrState::execute` takes a `Bytes32` position commitment from the privacy layer | C1 → C3 |
| Temporal VM (C2) | `lace-pm-compose` | `OracleResolver::answer` — matches `lace-conditions::OracleResolver` exactly | C3 → C2 |
| Temporal VM (C2) | `lace-pm-compose` | `Engine::create_conditional_trigger` for time-and-market gated contracts | C3 → C2 |
| Veil Score (C4) | `lace-pm-oracle` | `ReputationSink::record(ReputationEvent::*)` | C3 → C4 |
| Veil Score (C4) | `lace-pm-oracle` | `ForecasterVote::reputation_bps` (read from C4) | C4 → C3 |
| Loans / governance (C5) | `lace-pm-compose` | `ProbabilityFeed::get_live_probability` for collateral-ratio adjustment | C3 → C5 |
| Governance (built-in) | `lace-pm-governance` | `try_execute` / `GovernanceParams` | self-contained |

## 1. Privacy layer (Component 1) → AMM

The AMM never sees an address. Every call to `LmsrState::execute` carries a `position_commitment: Bytes32` produced by the privacy layer. The commitment is the hash of a shielded note that the privacy layer maintains; the AMM stores it on the [`TradeReceipt`] verbatim and emits no other identifier.

```rust
let receipt = amm.execute(outcome_idx, delta_shares, fees, position_commitment)?;
// `receipt.position_commitment` is what the privacy layer uses to find
// the shielded note for this position; the AMM did not need to inspect
// it at any point.
```

When the privacy layer adds a `PrivateInvoke` circuit, it can prove "this commitment maps to a note whose owner I am" and reveal the note value without revealing the owner. The AMM is unchanged.

## 2. Temporal VM (Component 2) ↔ Oracle resolver

The temporal VM (`lace-conditions`) defines:

```rust
pub trait OracleResolver {
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer;
}

pub enum OracleAnswer {
    Resolved(Bytes32),
    Pending,
    Voided,
}
```

`lace-pm-compose` declares a parallel `OracleResolver` trait with **identical semantics**. At the protocol-binary assembly stage one trivial adapter bridges the two:

```rust
// In the binary that owns both crates:
impl lace_conditions::OracleResolver for lace_pm_compose::Engine {
    fn answer(&self, oracle: &lace_conditions::Bytes32) -> lace_conditions::OracleAnswer {
        match lace_pm_compose::OracleResolver::answer(self, &Bytes32(oracle.0)) {
            lace_pm_compose::OracleAnswer::Resolved(h) => {
                lace_conditions::OracleAnswer::Resolved(lace_conditions::Bytes32(h.0))
            }
            lace_pm_compose::OracleAnswer::Pending => lace_conditions::OracleAnswer::Pending,
            lace_pm_compose::OracleAnswer::Voided => lace_conditions::OracleAnswer::Voided,
        }
    }
}
```

This adapter is the *only* code that needs to know about both workspaces. Both crates can be iterated independently as long as the two answer enums stay in sync.

### Why duplicate the trait

We deliberately did not add `lace-conditions` as a path dependency of this workspace because:

1. Component 3 must build standalone for audit.
2. The trait shape is small and stable — three variants, one method.
3. Avoiding the cross-workspace `path = "../temporal-vm/..."` import means both components can vendor independently into a release binary.

## 3. Temporal VM (Component 2) → Conditional triggers

A timelock contract that wants to fire when a market resolves the expected way registers a trigger:

```rust
engine.create_conditional_trigger(market_id, OutcomeId::YES, Box::new(move |_market, _outcome| {
    // The temporal VM's contract scheduler advances the contract.
}));
```

`tick()` is called by the runtime once per block; it fires every trigger whose market terminally resolved its expected outcome since the previous tick, and drops triggers whose markets resolved any other way (including void).

Triggers are **fire-once-and-forget**: re-firing requires registering a new trigger. This is intentional — markets are single-shot resolution events and idempotency is the safer default for the timelock side.

## 4. Veil Score (Component 4) ↔ Reputation feedback

The `ReputationSink` trait in `lace-pm-oracle` mirrors the one in `lace-disputes` from the temporal VM. Both components emit events into the same downstream accumulator:

```rust
pub enum ReputationEvent {
    ResolverCorrect { voter, market, outcome, stake },
    ResolverIncorrect { voter, market, outcome, stake },
    ForecasterCorrect { voter, market, outcome, reputation_bps },
    ForecasterIncorrect { voter, market, outcome, reputation_bps },
    DisputeUpheld { challenger, market },
    DisputeRejected { challenger, market, bond_burned },
}
```

The Veil Score crate implements `ReputationSink` and consumes these to update calibration histories. Component 4 also feeds its current scores back into the oracle through `ForecasterVote::reputation_bps` — this is read-only from Component 3's perspective.

### Calibration window

Resolver and forecaster correctness events fire on `finalize_and_emit`. If the resolution was overturned by a dispute, the *post-dispute* outcome is used as the correctness benchmark. This is the cycle that drives the protocol's reputation flywheel: forecasters who consistently match the upheld outcome rise; those who don't fall.

## 5. Loans (Component 5) → Probability feed

A loan that wants to underwrite using a market-resolved probability:

```rust
use lace_pm_compose::ProbabilityFeed;

let market = MarketId(/* ... */);
let outcome = OutcomeId::YES;
let p_default = engine.get_live_probability(market, outcome)
    .unwrap_or(Probability::ZERO);
let ltv_adjustment = compute_ltv_from_default_probability(p_default);
```

The probability feed is *live* — it tracks the AMM continuously. This is the same surface used for collateral-ratio adjustments, dynamic interest rates, and undercollateralised lending tied to Veil Score.

## 6. Stability guarantees

Within the `0.1.x` line:

- ✅ `Bytes32`, `MarketId`, `OutcomeId`, `Address`, `Amount`, `Probability` — wire-stable.
- ✅ `OracleAnswer` (3 variants) — wire-stable.
- ✅ `ReputationEvent` — wire-stable. New variants may be added; consumers must be exhaustive-matching tolerant.
- ✅ `ProbabilityFeed` trait — method signatures stable; default outcomes (`YES` / `NO`) interpretation stable.
- ⚠️ `Market`, `LmsrState`, `ResolutionRound` — internal field layout may change. Callers should construct via the public constructors and read via the documented accessors, not field access.
- ⚠️ `GovernanceParams` — fields may be added. Treat as additive-only on the wire.

Anything not in this list is internal and may change at any time.
