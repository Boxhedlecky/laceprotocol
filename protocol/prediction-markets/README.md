# Lace Protocol — Prediction Market Engine

> Component 3 of 5 in the Lace Protocol core build.

Prediction markets are **not an app** on Lace — they are base-layer infrastructure. Market-resolved probabilities feed into loans, timelocks, governance, and oracle functions. This component is the uncertainty pricing engine for the entire protocol.

## What this component delivers

1. **Four market shapes** — binary, scalar, multi-outcome, and conditional. All four expose the same live-probability surface so downstream consumers do not branch on shape.
2. **LMSR market maker** — bounded-loss, always-quoting AMM. Chosen over CPMM after a documented trade-off analysis (see [`SPEC.md §2`](SPEC.md)).
3. **Validator + reputation-weighted resolution** — markets resolve via a validator pool combined with reputation-weighted forecaster votes. A configurable dispute window allows challenge; an escalation path resolves contested rounds.
4. **Composability interfaces** for the rest of the protocol:
   - `get_live_probability(market_id)`
   - `get_resolved_outcome(market_id)`
   - `create_conditional_trigger(market_id, expected_outcome, callback)`
   - implements the temporal VM's `OracleResolver` trait so timelocks can be gated on market outcomes.
5. **Prediction-gated governance** — protocol upgrades only execute if a forecast market clears an adoption threshold within a defined window; on-chain parameter governance for fees, weights, and windows.

## Workspace layout

| Crate | Role |
| --- | --- |
| [`lace-pm-types`](crates/lace-pm-types) | `MarketId`, `OutcomeId`, `Probability`, `FeeSchedule`, `Amount` — the contract surface other crates agree on. |
| [`lace-pm-markets`](crates/lace-pm-markets) | The four market shapes, the market state machine, scalar→probability projection. |
| [`lace-pm-amm`](crates/lace-pm-amm) | LMSR pricing, fee application, slippage measurement, liquidity provision. |
| [`lace-pm-oracle`](crates/lace-pm-oracle) | Resolution rounds, validator + reputation-weighted voting, dispute windows, escalation, the `OracleResolver` impl consumed by the temporal VM. |
| [`lace-pm-compose`](crates/lace-pm-compose) | The cross-component facade: probability feeds, resolved-outcome lookups, conditional triggers, the temporal-VM `OracleResolver` adapter. |
| [`lace-pm-governance`](crates/lace-pm-governance) | Prediction-gated upgrade controller and on-chain parameter governance. |

See [`SPEC.md`](SPEC.md) for the full technical specification, [`INTEGRATION.md`](INTEGRATION.md) for the cross-component interfaces.

## Building

```bash
cd protocol/prediction-markets
cargo build
cargo test
```

Toolchain pinned to Rust 1.95 via [`rust-toolchain.toml`](rust-toolchain.toml). No external services required.

## Integration boundaries

The prediction market engine **does not** own:

- **Funds movement** — settled payouts emit descriptors that the privacy layer (Component 1) consumes.
- **Wallet identity** — trades carry an opaque position-commitment hash from the privacy layer; the AMM never sees a trader's address.
- **Time semantics** — close heights and window lengths are interpreted by the temporal VM (Component 2). The oracle crate calls into the VM for "is this window still open?" queries.
- **Reputation computation** — the engine emits resolution-quality and forecaster-calibration signals as reputation events; Veil Score (Component 4) reads them and updates scores.

Those four boundaries are documented in [`INTEGRATION.md`](INTEGRATION.md).

## Status

Pre-1.0. Reference implementation. The AMM uses `f64` for cost-function math; production consensus settlement requires a fixed-point Q64.64 rewrite (annotated with `// TODO(consensus-fp)` markers). Algorithm is unchanged; only the arithmetic substrate changes.
