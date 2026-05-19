# Lace Protocol — Consensus Mechanism

> Component 5 of 5 in the Lace Protocol core build.

The consensus layer is the engine that runs Lace. It produces blocks, finalises
them, accounts rewards, punishes misbehaviour, proves block validity to
Ethereum, and pins down where block data lives. All four prior components plug
in here:

- **Privacy** ([../privacy](../privacy)) supplies the proof system that ZK
  execution wraps and the disclosure-key types the bridge re-verifies.
- **Temporal VM** ([../temporal-vm](../temporal-vm)) supplies `BlockHeight`,
  slot semantics, and the recurring-payment / timelock obligations the
  reputation system observes.
- **Prediction markets** ([../prediction-markets](../prediction-markets))
  resolves the markets that calibrate validator Veil Scores.
- **Reputation** ([../reputation](../reputation)) supplies the Veil Score that
  scales proposer selection weight and block reward share.

Lace runs **Proof of Stake with reputation weighting**. A validator's slot
share is `stake × veil_multiplier`, and their reward on each block they
propose is `base_reward × stake_weight × reputation_multiplier`. Slashing
follows the protocol-wide 60 % counterparty / 25 % burn / 15 % ecosystem
reserve split fixed in the reputation spec.

## What this component delivers

1. **Proof of Stake core** — validator registration, staking, proposer
   selection weighted by `stake × reputation`, slot/epoch architecture,
   fork-choice rule, and a finality gadget.
2. **Reputation-weighted validation** — block reward share scaled by Veil
   Score; slashing for downtime, double signing, and provably bad market
   resolution.
3. **ZK-native execution** — block production *is* proof production.
   Recursive aggregation rolls a window of blocks into one proof. A stand-in
   Ethereum verifier interface anchors finality on L1.
4. **Data availability** — in-house DA with validity proofs and a sampling
   API. A modular `DaBackend` trait lets the operator swap to Celestia or
   EigenDA without touching the rest of the consensus stack.
5. **Interoperability** — IBC-shaped channel/connection/packet
   interfaces, a bridge architecture spec, and cross-chain verification of
   selective disclosure keys and Veil Score statements.
6. **Devnet** — a 4-validator in-process simulator, a genesis file, a
   faucet stub, and an explorer config. One command brings the whole stack
   up.
7. **Tests** — safety + liveness, validator slashing, fork resolution, DA
   availability, and bridge proof checks.

## Workspace layout

| Crate | Role |
| --- | --- |
| [`lace-cons-types`](crates/lace-cons-types) | `ValidatorId`, `Slot`, `Epoch`, `BlockHeader`, `Block`, `Stake`, `Vote`, `Justification`. The contract surface. |
| [`lace-cons-pos`](crates/lace-cons-pos) | Validator set, registration, staking / unstaking, weighted proposer selection, epoch transitions. |
| [`lace-cons-fork-choice`](crates/lace-cons-fork-choice) | GHOST-style fork choice over the block tree + a two-round BFT finality gadget. |
| [`lace-cons-slashing`](crates/lace-cons-slashing) | Slashing predicates (downtime, double-sign, bad-resolution) and slash math (60/25/15 routing). |
| [`lace-cons-rewards`](crates/lace-cons-rewards) | Block reward calculation: `base × stake_weight × reputation_multiplier`, emission curve, fee distribution. |
| [`lace-cons-zk-exec`](crates/lace-cons-zk-exec) | `Prover` / `Verifier` traits, recursive aggregation interface, Ethereum verifier client stand-in. |
| [`lace-cons-da`](crates/lace-cons-da) | In-house DA store, sampling API, modular `DaBackend` for Celestia / EigenDA. |
| [`lace-cons-bridge`](crates/lace-cons-bridge) | IBC-shaped channel/connection/packet types, cross-chain disclosure-key and Veil Score verification. |
| [`lace-cons-node`](crates/lace-cons-node) | Single-node runtime binary tying every crate together. |
| [`lace-cons-devnet`](crates/lace-cons-devnet) | 4+ validator in-process simulator binary, genesis loader, faucet stub. |

See [`SPEC.md`](SPEC.md) for the full technical specification.

## Building

```bash
cd protocol/consensus
cargo build
cargo test
```

Toolchain pinned to Rust 1.95 via [`rust-toolchain.toml`](rust-toolchain.toml).
No external services required for the devnet — everything runs in-process.

## Running the devnet

```bash
cargo run --bin lace-cons-devnet -- --validators 4 --genesis devnet/genesis.json
```

See [`../../DEVNET.md`](../../DEVNET.md) for the full quickstart, including the
faucet and explorer.

## Integration boundaries

This component **does not** own:

- **The Halo2 circuits themselves** — `lace-cons-zk-exec` defines the
  `Prover` / `Verifier` traits and a stand-in implementation. The real
  circuits live in `privacy/crates/lace-circuits`.
- **The Veil Score itself** — the consensus layer treats Veil Score as an
  oracle (`ReputationOracle` trait); the score is computed and committed by
  `reputation/crates/lace-veil-score`.
- **The settlement of slashed funds** — slashing produces a
  `SlashSettlement` descriptor; the privacy layer's note system moves the
  funds.
- **Market resolution** — bad-resolution slashing fires on evidence emitted
  by `prediction-markets/crates/lace-pm-oracle`. This crate only verifies the
  evidence and applies the slash.
- **Cross-chain transport** — the bridge crate defines IBC-shaped packet
  shapes and verification rules. The wire transport (relayer, light client)
  is out of scope for the devnet.

## Status

Pre-1.0. Reference implementation. ZK proving, the Ethereum L1 verifier, the
IBC relayer transport, and the live multi-host devnet topology are
**stand-ins** clearly labelled `// TODO(zk-prover)`, `// TODO(eth-verifier)`,
`// TODO(ibc-relayer)`, and `// TODO(devnet-topology)`. The shapes, the trait
surfaces, and all pure-logic algorithms (proposer selection, fork choice,
slashing math, reward routing, DA sampling) are stable and tested.

All parameter values (minimum stake, slot duration, epoch length, finality
threshold, slashing severity, emission curve coefficients) are **placeholder**
and labelled `// TODO(governance)` for the launch parameter committee.
