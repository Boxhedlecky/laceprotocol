# Lace Reputation & Veil Score — Technical Specification

> Component 4 of 5. Pre-1.0. All `// TODO(governance)` parameter values are placeholder pending the launch parameter committee. The `// TODO(zk-circuit)` substrate swap is scheduled before the first internal testnet.

---

## 1. Overview

The Veil Score is a per-address reputation tag computed from four orthogonal signals. The raw score never crosses an external boundary — only [`ScoreCommitment`](crates/lace-veil-types/src/lib.rs) values do — and external consumers verify properties of the score using one of four ZK proof shapes.

The system is **soul-bound** (non-transferable), **private by default** (the privacy layer holds the witness), and **continuously updated** (new on-chain events take effect immediately, not on epoch boundaries).

Four downstream protocol functions depend on the score:

1. **Undercollateralised lending** (`lace-veil-lending`)
2. **Reputation-weighted governance** (`lace-veil-governance`)
3. **Tiered timelock terms** (consumed by Component 2)
4. **Forecaster vote weighting in oracle resolution** (consumed by Component 3)

---

## 2. Score components

The score is a weighted blend of four inputs, each in basis points (0..=10_000):

```
score_bps = floor(
    w_payment      * payment_history_bps +
    w_calibration  * calibration_bps     +
    w_attestation  * attestation_bps     +
    w_tenure       * tenure_bps
) / 10_000
```

Default weights (governance-adjustable): **40 / 25 / 20 / 15**.

### 2.1 Payment history (`payment_history_bps`)

Inputs: `PaymentMet` and `PaymentMissed` events emitted by the temporal VM. A `Slashed` event from the lending or stake crates also drives this component.

Accumulator:

- `payments_met` — simple count.
- `missed_weighted` — incremented by `missed_penalty_multiplier ^ consecutive` on each `PaymentMissed`. Consecutive misses hurt progressively (default base = 3, cap streak at 8).

Component value:

```
observed_bps = paid * 10_000 / (paid + missed_weighted)
blended_bps  = (observed_bps * min(total, payment_full) + 5_000 * (payment_full - min(total, payment_full))) / payment_full
```

The blend prevents low-volume wallets from swinging wildly toward 0 or 10_000 on one event.

### 2.2 Forecast calibration (`calibration_bps`)

Inputs: `ForecastCorrect` and `ForecastIncorrect` events emitted by the prediction-market oracle. Each event carries the `weight_bps` the forecaster's vote had at vote time; the accumulator is weighted by that.

Same blend-with-neutral-prior shape as payment history. Saturation at `calibration_full = 100` forecasts.

### 2.3 Counterparty attestations (`attestation_bps`)

Inputs: `AttestationPosted` and `AttestationRevoked` events emitted by the attestation graph (`lace-veil-attest`). The graph itself applies three filters before the event reaches the score engine:

1. **Sybil weight**: per-band multiplier on the attester's claim.
2. **Per-attester budget**: hard cap on aggregate attestation weight per attester.
3. **Time decay**: linear ramp to zero over `decay_full` blocks.

Score-side accumulator: sum of effective attestation weight, saturating at 20_000 bps (full credit at 2x the maximum single-attestation weight).

### 2.4 Tenure (`tenure_bps`)

Linear ramp from `first_seen` block: `min(age / tenure_full, 1) * 10_000`. Default `tenure_full ≈ 1 year`.

Tenure does **not** decay. Idle wallets see their three behavioural components drift to neutral, but tenure is monotone non-decreasing.

### 2.5 Decay

Every non-tenure component drifts back toward 5_000 bps (neutral) by `decay_bps_per_span` bps every `decay_span` blocks of inactivity. Default: 50 bps / week. Applied lazily on the next event ingest.

---

## 3. Score bands

A score in `[0, 10_000]` partitions into five bands. Bands are the **only** score shape that crosses external boundaries unmediated by a ZK proof.

| Band         | Range         | LTV (lending) | Governance multiplier | Attester sybil multiplier |
|--------------|---------------|---------------|------------------------|----------------------------|
| Untrusted    | `0..2_000`    | — (no credit) | 0.5x                  | 0.05x                      |
| Emerging     | `2_000..4_000`| 60 %          | 0.75x                 | 0.25x                      |
| Established  | `4_000..6_000`| 80 %          | 1.0x                  | 0.5x                       |
| Trusted      | `6_000..8_000`| 100 %         | 1.5x                  | 0.85x                      |
| Exemplary    | `8_000..=10_000`| 125 %       | 2.0x                  | 1.0x                       |

All values `// TODO(governance)` placeholder.

---

## 4. ZK score proofs

External consumers see only a [`ScoreCommitment`] (Pedersen-style hiding commitment to the score witness) plus one of four proof shapes:

| Proof              | Public inputs                                | Predicate                                    |
|--------------------|----------------------------------------------|----------------------------------------------|
| `Threshold`        | `commitment`, `threshold_bps`                | `score >= threshold_bps`                     |
| `ZeroDefaults`     | `commitment`, `now`, `window`                | no `PaymentMissed` in `(now - window, now]`  |
| `CalibrationBand`  | `commitment`, `lo_bps`, `hi_bps`             | `calibration_bps ∈ [lo, hi]`                 |
| `Tenure`           | `commitment`, `now`, `min_blocks`            | `now - first_seen >= min_blocks`             |

### 4.1 Substrate

The privacy layer (Component 1) holds the actual Halo2 circuit implementations via `lace-circuits`. Until those land, `lace-veil-proofs` ships a **commit-and-open stand-in**: the proof reveals the witness inside the proof artefact, and `verify()` re-derives the commitment from the revealed witness and checks the predicate.

Migration to Halo2 is a single-module swap behind the stable `prove()` / `verify()` API — the same migration model `lace-primitives::hash` uses for its Blake2b → Poseidon2 stand-in. Migration changes every commitment value the protocol produces.

### 4.2 Binding

The commitment binds together the score, the calibration component, the `first_seen` block, the last-missed-payment block, and a per-commitment blinding factor. All four proof kinds use the same witness shape; the predicate is what differentiates them.

---

## 5. Undercollateralised lending

`lace-veil-lending::LendingEngine` runs the loan lifecycle:

```
open --> active --> repaid
              \--> defaulted --> recovery window --> liquidated
                            \--> partial repay   --> active
```

Open requires the borrower to be in an eligible band and `principal / collateral <= max_ltv(band)`. Interest is flat per tenor (default 250 bps / 30 days).

A loan enters `Defaulted` automatically on `tick(now)` when `now > due_at + grace_period`. The tick emits a `PaymentMissed` score event so the score drops *as* the loan deteriorates.

`liquidate()` is only callable after the recovery window has elapsed past the default. It seizes all collateral and emits a `Slashed` event carrying `seized + shortfall` (the realised loss). The stake crate then slashes the borrower's reputation stake by the shortfall.

---

## 6. Reputation staking

`lace-veil-stake::StakeEngine`. Users lock LACE against their own score.

### 6.1 Slash routing — protocol-fixed

**60 % to harmed counterparty / 25 % burn / 15 % ecosystem reserve.** Documented in the master spec; not governance-adjustable.

Any rounding remainder routes to the ecosystem sink so the realised total equals the requested slash amount.

### 6.2 Slash priority

Slash drains `locked` first, then `cooling`. A defaulter cannot front-run slashing by requesting unstake.

### 6.3 Calibration rewards

A stake that survives a full `reward_epoch` without being slashed earns `reward_bps_per_epoch` bps in LACE (default 50 bps / 30 days ≈ 6 % annualised). Settled lazily by `settle_reward(subject, now)`. Sub-minimum positions are ineligible.

### 6.4 Unstake

`request_unstake(amount)` moves LACE from `locked` to `cooling` and sets `cooling_ready_at = now + unstake_cooldown` (default 14 days). `withdraw()` rejects before the cooldown matures.

---

## 7. Counterparty attestations

`lace-veil-attest::AttestGraph`. Each attestation: `(subject, attester, raw_weight_bps, posted_at)`.

### 7.1 Sybil resistance

Effective weight is `raw_weight_bps * multiplier(attester_band) / 10_000`. An Untrusted attester contributes 5 % of their nominal claim; Exemplary attesters contribute 100 %. This makes sybil rings ineffective: every fresh sybil starts in Untrusted and stays there until it accrues independent signal.

### 7.2 Budget

Each attester has a per-attester total budget (default 50_000 bps). Exceeding it on a `post` returns `BudgetExceeded`.

### 7.3 Decay & revocation

`tick_decay(now)` is the idempotent epoch tick. Each live attestation's effective weight ramps linearly to zero over `decay_full` blocks; `tick_decay` emits `AttestationRevoked` events for the difference since the last tick.

`revoke()` is a unilateral attester operation. Bad-faith disputes go through the temporal-VM disputes path; on upheld dispute, the graph revokes the attestation **and** slashes 50 % of the attester's remaining budget.

---

## 8. Governance weighting

`lace-veil-governance::vote_weight(stake, band, params)` returns:

```
vote_weight = floor(stake * multiplier(band) / 10_000)   if stake >= min_voting_stake
            = 0                                          otherwise
```

`tally_votes(votes, params)` aggregates with per-voter deduplication (latest vote wins) and a strict-majority pass rule.

---

## 9. Event normalisation

Upstream components (temporal VM, prediction-market oracle) emit their own `ReputationEvent` enums. A thin adapter in their `ReputationSink` impls translates into [`lace_veil_types::ScoreEvent`] before reaching the engine. This lets the upstreams evolve their event shapes without forcing a Veil Score schema migration.

Mapping table:

| Upstream event                                  | `ScoreEvent` variant       |
|-------------------------------------------------|----------------------------|
| `temporal_vm::CleanRelease`                     | `PaymentMet`               |
| `temporal_vm::PaymentsMissed`                   | `PaymentMissed`            |
| `temporal_vm::DisputeSettled` (loser)           | `PaymentMissed`            |
| `pm::ResolverCorrect` / `ForecasterCorrect`     | `ForecastCorrect`          |
| `pm::ResolverIncorrect` / `ForecasterIncorrect` | `ForecastIncorrect`        |
| `pm::DisputeUpheld` (challenger)                | `ForecastCorrect`          |
| `pm::DisputeRejected` (challenger)              | `ForecastIncorrect`        |
| `attest::AttestationOk`                         | `AttestationPosted`        |
| `attest::AttestationRevoked`                    | `AttestationRevoked`       |

---

## 10. Risks & known gaps

- **Score-band oracle gaming.** A counterparty needs to know the borrower's score band to enforce per-band terms. The protocol exposes this via a *band attestation*: the borrower commits to a band and proves `Threshold` for the band's lower bound. A borrower lying about their band by committing to an under-threshold score breaks the binding check; lying by committing to an over-threshold score lets the borrower get *worse* terms, not better.
- **Stand-in proof substrate.** Until the Halo2 circuits land, the proof crate opens the witness inside the proof; an attacker reading the proof bytes learns the score. This is acceptable for devnets and pre-mainnet testing only.
- **Per-attester budget bypass.** Coordinated sybil rings can pool budgets across many attesters. Mitigation is the per-band multiplier curve plus the upstream cost of getting each sybil into a non-Untrusted band; not strictly impossible, but expensive.
- **First-seen anchoring.** `FirstSeen` is emitted by the engine on the first event for an address. A malicious first-event sender cannot lie about *which* block the event occurred at (block height is provided by the consensus layer), but they can choose to emit `FirstSeen` artificially late. Mitigation: the engine treats the first-touch event itself as implicit `FirstSeen` and takes the earlier of `state.first_seen` and the event block.

---

## 11. Roadmap

1. **Halo2 circuit migration** (blocks first internal testnet). Closes the proof substrate gap.
2. **Per-component decay curves** — currently linear toward 5_000; experiments with exponential and asymmetric curves are tracked.
3. **Cross-chain reputation portability** — a `Threshold` proof verifiable on Ethereum mainnet for the presale window, so referral-tier qualification can read the Lace-side score without bridging the wallet.

---

## 12. Test coverage

- 6 unit tests in `lace-veil-types` (basis-point arithmetic, band partitioning).
- 12 unit tests in `lace-veil-score` (component recompute, decay, neutral blending).
- 12 unit tests in `lace-veil-proofs` (each of the four predicate kinds, plus tampered-proof binding check).
- 10 unit tests in `lace-veil-attest` (sybil weighting, budget, decay, dispute path).
- 14 unit tests in `lace-veil-stake` (slash priority, distribution remainder, reward gating).
- 11 unit tests in `lace-veil-lending` (band eligibility, default → recovery → liquidation, shortfall surfacing).
- 10 unit tests in `lace-veil-governance` (per-band multipliers, dedup, strict-majority rule).
- Workspace integration tests in `tests/` cover the full flow: ingest events → recompute score → mint commitment → prove threshold → take a loan → miss a payment → liquidate → slash.
