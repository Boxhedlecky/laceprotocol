# Lace Temporal VM

> Component 2 of 5 in the Lace Protocol core build.

Time is a first-class type in the Lace VM. Rather than relying on smart-contract bytecode to handle deadlines, recurring schedules, and inactivity timers, the temporal VM understands time *natively* — as opcodes with strict typing, as a three-valued condition algebra, and as a small set of P2P contract templates that compose both.

The goal is trustless P2P agreements (escrow, recurring payments, milestone contracts, inheritance) without the smart-contract overhead — and without the bug surface that comes with reimplementing the same time logic in every contract.

## Workspace layout

| Crate | Role |
| --- | --- |
| [`lace-time`](crates/lace-time) | `Timestamp`, `Duration`, `Interval`, `TimeDelta`, `Clock` trait |
| [`lace-vm`](crates/lace-vm) | Opcodes (`AFTER`, `BEFORE`, `DEADLINE`, `RECURRING`, `TIMEDELTA`), executor, scheduler |
| [`lace-conditions`](crates/lace-conditions) | Three-valued (`Ready` / `Pending` / `Failed`) condition algebra over time + oracle leaves |
| [`lace-contracts`](crates/lace-contracts) | Escrow, recurring payment, milestone, dead-man switch templates |
| [`lace-disputes`](crates/lace-disputes) | Slashing policy, dispute outcomes, reputation event sink |

See [`SPEC.md`](SPEC.md) for the full technical specification.

## What this component delivers

1. **Five native time opcodes**: `AFTER(ts)`, `BEFORE(ts)`, `DEADLINE(ts)`, `RECURRING(interval, start, end)`, `TIMEDELTA(t1, t2)`. All composable with each other and with the ZK privacy layer (Component 1).
2. **Conditional release logic**: time AND oracle, time OR oracle, freely nested. The condition resolver is three-valued so `Pending` (wait) is distinguishable from `Failed` (give up). Resolution against an [`OracleResolver`] is the integration point with the prediction-market engine (Component 3).
3. **Four P2P primitive contracts** built on the opcodes and conditions:
   - **Escrow** — mutual confirm, both-party abort window, dispute escalation, slashing for bad-faith abort.
   - **Recurring payment** — configurable interval / window, pause / resume, deterministic missed-tick handling.
   - **Milestone** — staged release with mixed time + oracle gating, dispute path, refund of remainder on failure.
   - **Dead man's switch** — weighted multi-beneficiary inheritance, heartbeat reset, residue-balancing rounding.
4. **Abort and dispute system** — bounded slashing (default 10 %, +20 % on bad faith), reputation-event sink consumed by Component 4 (Veil Score).
5. **Tests** — 55 unit + integration tests covering every opcode combination, every contract path, and explicit attack-vector cases (timestamp replay, grief, simultaneous transitions).

## Building

```bash
cd protocol/temporal-vm
cargo build
cargo test
```

Toolchain pinned to Rust 1.95 via [`rust-toolchain.toml`](rust-toolchain.toml). No external services or chain state needed for build or test.

## Status

Pre-1.0. All public types may change before mainnet. The opcode discriminant table in `lace-vm::opcode::Op::tag` is stable across this crate's `0.1.x` line; the contract state-machine shapes are not yet stable and may evolve as Components 3 and 4 land.

## Integration boundaries

The temporal VM does not own:

- **Funds movement**. Contracts emit [`Payout`] descriptors; the privacy layer settles them against private notes.
- **Oracle resolution**. The conditions crate calls into an [`OracleResolver`] implemented by the prediction-market engine.
- **Reputation scoring**. The disputes crate emits [`ReputationEvent`]s to a [`ReputationSink`] implemented by the Veil Score engine.

Those three traits are the entire surface area between this component and the rest of the protocol. The interfaces are documented in [`SPEC.md`](SPEC.md) §6.
