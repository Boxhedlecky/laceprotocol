# Lace Protocol — Reputation & Veil Score

> Component 4 of 5 in the Lace Protocol core build.

Every address on Lace accumulates a **Veil Score** — a private, ZK-provable reputation built from on-chain behaviour. The score is never revealed directly; users prove properties of it (above-threshold, zero defaults, calibration band, tenure) with ZK proofs that ride the privacy layer (Component 1).

The Veil Score is what makes the rest of the protocol load-bearing for strangers transacting with strangers:

- **Undercollateralised lending** — LTV bands keyed off the score band. The top band can borrow up to 125 % of collateral.
- **Tiered timelock terms** — escrow disputes weigh attestations and history through the score.
- **Governance weighting** — vote weight is `staked LACE × score multiplier`, not pure token-weighted (per the master design principles).
- **Prediction market resolution weighting** — high-calibration forecasters carry more weight (consumed by Component 3 via `ReputationSink`).

## What this component delivers

1. **Soul-bound score** built from four inputs: payment history (from temporal VM), forecast calibration (from prediction markets), counterparty attestations, and on-chain tenure. Continuously updated.
2. **ZK score proofs** — threshold, zero-defaults, calibration-band, tenure. Privacy layer holds the actual Halo2 circuits; this crate holds the statement shapes and a stand-in verifier.
3. **Undercollateralised lending** with score-banded LTVs, liquidation, and a defined recovery path.
4. **Reputation staking** — slashing routes 60 % to the harmed counterparty / 25 % burn / 15 % ecosystem reserve. Calibration rewards for sustained performance. Cooldown on unstake.
5. **Governance weighting** with adjustable multiplier bands.
6. **Counterparty attestation graph** with sybil resistance (new / low-score attesters carry minimal weight), decay over time, and revocation.

## Workspace layout

| Crate | Role |
| --- | --- |
| [`lace-veil-types`](crates/lace-veil-types) | `Address`, `Score`, `ScoreBand`, `ScoreCommitment`, `ScoreEvent`, `LoanId`, `AttestationId` — the contract surface. |
| [`lace-veil-score`](crates/lace-veil-score) | Four-input score calculation, accumulator state, continuous update from `ScoreEvent`s, decay. |
| [`lace-veil-proofs`](crates/lace-veil-proofs) | Statement shapes and prover/verifier for the four ZK proof kinds. |
| [`lace-veil-attest`](crates/lace-veil-attest) | Attestation graph with sybil weight, time decay, revocation, and dispute. |
| [`lace-veil-stake`](crates/lace-veil-stake) | Stake / slash / reward / unstake state machine. |
| [`lace-veil-lending`](crates/lace-veil-lending) | Loan origination, repayment, liquidation, recovery. |
| [`lace-veil-governance`](crates/lace-veil-governance) | Vote-weight calculation, band-keyed multiplier params. |

See [`SPEC.md`](SPEC.md) for the full technical specification, [`INTEGRATION.md`](INTEGRATION.md) for cross-component interfaces, and [`API.md`](API.md) for the consolidated API reference.

## Building

```bash
cd protocol/reputation
cargo build
cargo test
```

Toolchain pinned to Rust 1.95 via [`rust-toolchain.toml`](rust-toolchain.toml). No external services required.

## Integration boundaries

This component **does not** own:

- **Funds movement** — slashing and lending payouts emit settlement descriptors that the privacy layer (Component 1) consumes.
- **The ZK circuits themselves** — `lace-veil-proofs` defines the statement shapes; Component 1's `lace-circuits` will host the Halo2 instances.
- **Reputation event sources** — payment misses come from the temporal VM (Component 2), forecaster calibration from the prediction-market oracle (Component 3). This crate consumes their `ReputationEvent`s through a thin adapter.
- **Time semantics** — block heights and tenure spans are interpreted; advancing the clock is the temporal VM's job.

## Status

Pre-1.0. Reference implementation. The ZK proof crate uses a hash-and-blinding stand-in (see [`lace-veil-proofs/src/lib.rs`](crates/lace-veil-proofs/src/lib.rs)) labelled with `// TODO(zk-circuit)` markers, pending the Halo2 circuits in the privacy layer. The statement shapes and verifier API are stable.

All parameter values (LTV bands, governance multipliers, decay rates, slashing splits past the documented 60/25/15) are **placeholder** and clearly labelled `// TODO(governance)` for the launch parameter committee to adjust.
