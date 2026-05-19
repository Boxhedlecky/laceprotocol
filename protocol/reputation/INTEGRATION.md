# Integration Guide

How the reputation / Veil Score system plugs into the other four protocol components.

---

## With Component 1 — Privacy

**What the privacy layer provides:**

- `lace-primitives::hash` — the in-circuit hash function. The proof crate's `commit()` will call this once Component 1 exposes it to non-circuit callers.
- `lace-circuits` (forthcoming) — Halo2 circuits for the four `Statement` kinds in `lace-veil-proofs`. The `prove()` / `verify()` API surface in this crate is stable; the Halo2 substitution is a single-module change.
- Selective disclosure keys — a Veil Score holder can use the privacy layer's `ThresholdDisclosureKey` to share their score with a specific counterparty (e.g. a lender on another chain) without revealing it generally.

**What this component provides:**

- `ScoreCommitment` — opaque 32-byte hiding commitment to a score witness. The privacy layer's note ciphertexts may carry this as an opaque field.
- `Witness` shape — the witness fields a Halo2 circuit needs as private inputs.

**Boundary contract:**

- This crate **does not** allocate Halo2 columns or call Halo2 directly. The substrate swap lives behind `prove()` / `verify()`.
- Privacy layer **does not** read raw scores. It only sees commitments and statement shapes.

---

## With Component 2 — Temporal VM

**What the temporal VM provides:**

- `temporal_vm::ReputationEvent` (defined in `lace-disputes` and emitted by `lace-contracts`). The variants of interest:
  - `CleanRelease` → maps to `ScoreEvent::PaymentMet`.
  - `PaymentsMissed { consecutive, .. }` → maps to `ScoreEvent::PaymentMissed { consecutive, .. }`.
  - `DisputeSettled { loser, bad_faith, .. }` → maps to a `ScoreEvent::PaymentMissed` on the loser, with `consecutive` chosen by the bad-faith flag (`bad_faith ⇒ 2` else `1`).
  - `StaleAbort` → maps to a single `ScoreEvent::PaymentMissed { consecutive: 1, .. }` on the requester.
- Block-height clock. Every `ScoreEvent` carries a block height; this crate trusts it.
- Tiered timelock terms: a contract template can demand `Threshold` proofs from one or both counterparties at open time.

**What this component provides:**

- `ScoreBand` lookup for the borrower / counterparty so timelock terms can branch on band.
- The four ZK proof shapes; the temporal VM's contract templates encode them as opening predicates.

**Boundary contract:**

- The reputation crate **does not** advance the clock. It is a pure consumer of `block_height` parameters.
- The temporal VM **does not** compute scores. It only emits events.

---

## With Component 3 — Prediction Markets

**What the prediction-market engine provides:**

- `pm::ReputationEvent` (defined in `lace-pm-oracle`). The mapping:
  - `ResolverCorrect`, `ForecasterCorrect` → `ScoreEvent::ForecastCorrect { weight_bps, .. }`.
  - `ResolverIncorrect`, `ForecasterIncorrect` → `ScoreEvent::ForecastIncorrect { weight_bps, .. }`.
  - `DisputeUpheld { challenger, .. }` → `ScoreEvent::ForecastCorrect` on the challenger.
  - `DisputeRejected { challenger, bond_burned, .. }` → `ScoreEvent::ForecastIncorrect` on the challenger.
- Resolution-time read of the forecaster's score: `pm` calls `VeilEngine::score_of(voter).band()` to derive the `reputation_bps` weighting parameter in `ForecasterVote`.

**What this component provides:**

- A `ReputationSink` adapter that wraps `VeilEngine::ingest`. Drop-in for the `pm::ReputationSink` trait.
- Forecaster score lookup for vote weighting.

**Boundary contract:**

- The reputation crate **does not** resolve markets. It only consumes resolution-quality events.
- The PM engine **does not** read raw scores. It calls `band()` only.

---

## With Component 5 — Consensus

**What the consensus layer provides (forthcoming):**

- Block height ticks. Used to expire defaulted loans, decay attestations, mature unstake cooldowns.
- Validator reputation hook: `VeilEngine::ingest` consumes validator-side `ResolverCorrect` / `ResolverIncorrect` events from the same `pm::ReputationSink` adapter, so a validator's calibration score grows from their resolution accuracy.

**What this component provides:**

- Governance vote weight via `lace-veil-governance::vote_weight`. The consensus layer consumes this for any on-chain governance vote it shepherds.
- Validator slashing through `lace-veil-stake::StakeEngine::slash` for misbehaviour the consensus layer detects (equivocation, downtime).

**Boundary contract:**

- The reputation crate **does not** produce blocks or finalise. It is a state machine driven by external events.
- The consensus layer **does not** compute multipliers or LTVs. It consults this crate.

---

## With the Presale Contracts (off-chain components)

The presale contracts on Ethereum mainnet **do not** read Veil Score. The score system is post-TGE infrastructure; the presale referral logic uses on-Ethereum referral counts only.

However, the **referral multiplier tiers** in the presale (1-9 / 10-49 / 50+) are intentionally shaped to lay groundwork for post-TGE score bootstrapping: high-volume referrers naturally accrue a positive `payments_met` signal once the chain is live and they redeem their vested LACE on-chain, which seeds them into `ScoreBand::Emerging` or higher on day one.

---

## Adapter sketch

```rust
// In Component 3's lace-pm-oracle crate, the existing
// `ReputationSink` trait is implemented by this crate as:

use lace_veil_score::VeilEngine;
use lace_pm_oracle::{ReputationEvent as PmEvent, ReputationSink};

pub struct VeilSink<'a> {
    pub engine: &'a mut VeilEngine,
    pub now: u64,
}

impl<'a> ReputationSink for VeilSink<'a> {
    fn record(&mut self, event: PmEvent) {
        let event = match event {
            PmEvent::ForecasterCorrect { voter, reputation_bps, .. } => {
                lace_veil_types::ScoreEvent::ForecastCorrect {
                    subject: lace_veil_types::Address::new(voter.0.0),
                    weight_bps: reputation_bps,
                    at: self.now,
                }
            }
            // ... other variants ...
            _ => return,
        };
        self.engine.ingest(event);
    }
}
```

The temporal-VM adapter is symmetrical.

---

## Operational notes

- **Snapshotting.** `VeilEngine` is `Serialize + Deserialize`. The consensus layer is expected to snapshot it at epoch boundaries for fast-sync. The serialised representation is the score's full state; rebuilding the engine from raw events is also supported (event log replay).
- **Decay cost.** Decay is applied lazily on next-event for each address, so an idle wallet imposes O(1) work per next-touch (not per-block). `tick_decay` for the attestation graph is the one exception: it walks the live attestation set and should be invoked once per epoch.
- **Cross-crate event chaining.** A liquidation emits one `Slashed` event (from `lace-veil-lending`) plus an explicit `StakeEngine::slash` call. Consumers should treat both as part of the same logical transition rather than expecting the score engine to know about the stake side.
