# Lace Temporal VM — Technical Specification

**Version:** 0.1 (pre-1.0; subject to change before mainnet)
**Component:** 2 of 5 in the Lace Protocol core build
**Scope:** Native time opcodes, conditional release, P2P contract templates, dispute / slashing primitives.

---

## 1. Design goals

The temporal VM exists because every prior chain has implemented "do X after time T" in user-space smart-contract bytecode, and every prior chain has shipped bugs in those implementations. Time is hard: monotonicity is hard, validator-attested time is hard, deadlines that race with state transitions are hard. Making time a first-class type in the VM moves all of that to one well-audited place.

Design goals, in priority order:

1. **Determinism.** Two honest validators that observe the same block must produce identical state transitions. Every opcode is total over its typed inputs; every error variant is enumerated; no opcode reads off-chain state.
2. **Composability with ZK privacy.** The privacy layer (Component 1) needs the VM transition function to be expressible as a circuit. Stack-based, typed, bounded-step bytecode satisfies that without requiring the circuit to grow with contract complexity.
3. **Small surface area.** Five time opcodes plus a minimal supporting set (`PUSH`, `POP`, `DUP`, `SWAP`, `NOW`, `HEIGHT`, `AND`, `OR`, `NOT`, `EQ`, `HALT`). Everything else is built by composition.
4. **Honest failure modes.** "Wait" and "give up" are different states. Contract templates do not silently skip ticks, do not let unilateral aborts refund, and surface deadline violations as a distinct error variant rather than as a soft guard failure.

---

## 2. Time

### 2.1 Canonical time source

`Timestamp` is an unsigned 64-bit count of seconds since the Unix epoch.

Inside the VM, the value returned by `NOW` (and the `Clock::now` trait method) is supplied by **consensus** — specifically, the median timestamp of the validators that signed the current block, clamped to be strictly greater than the previous block's timestamp.

- Validators cannot move time backwards.
- An individual malicious validator cannot skew time by more than the median permits.
- The VM never consults wall clocks.

`HEIGHT` returns the current block height in parallel; height is strictly monotonic by construction and is the preferred deadline source for contracts that don't need wall-clock semantics.

### 2.2 Types

| Type | Representation | Notes |
| --- | --- | --- |
| `Timestamp` | `u64` (Unix seconds) | Saturating arithmetic. `Timestamp::MAX` encodes "no upper bound". |
| `Duration` | `u64` seconds | Unsigned; `is_zero()` is the scheduler's "invalid" predicate. |
| `TimeDelta` | `i64` seconds | Signed difference produced by `TIMEDELTA`. |
| `Interval` | `[start, end)` (half-open) | Backwards intervals normalise to empty. |
| `BlockHeight` | `u64` | Strictly monotonic. |

### 2.3 `Clock` trait

```rust
pub trait Clock {
    fn now(&self) -> Timestamp;
    fn height(&self) -> BlockHeight;
}
```

Production: wired to consensus state.
Tests: `ManualClock`, which march time forward under explicit control and refuses to rewind (panics on rewind).

---

## 3. Opcodes

### 3.1 Opcode catalogue

| Tag | Mnemonic | Stack effect (top last) | Semantics |
| --- | --- | --- | --- |
| 0x01 | `PUSH(v)` | `… → … v` | Push a literal. |
| 0x02 | `POP` | `… v → …` | Drop top. |
| 0x03 | `DUP` | `… v → … v v` | Duplicate top. |
| 0x04 | `SWAP` | `… a b → … b a` | Swap top two. |
| 0x05 | `NOW` | `… → … t` | Push current consensus time. |
| 0x06 | `HEIGHT` | `… → … h` | Push current block height. |
| 0x10 | `AFTER` | `… t →  …` | Continue iff `now > t`, else `GuardFailed`. |
| 0x11 | `BEFORE` | `… t → …` | Continue iff `now < t`, else `GuardFailed`. |
| 0x12 | `DEADLINE` | `… t → …` | Abort with `DeadlineExceeded` iff `now > t`. |
| 0x13 | `RECURRING` | `… i s e → …` | Emit a `Schedule` to the outbox. |
| 0x14 | `TIMEDELTA` | `… a b → … (b - a : Delta)` | Push signed delta. |
| 0x20 | `AND` | `… a b → … (a ∧ b)` | Boolean conjunction. |
| 0x21 | `OR` | `… a b → … (a ∨ b)` | Boolean disjunction. |
| 0x22 | `NOT` | `… a → … (¬a)` | Boolean negation. |
| 0x23 | `EQ` | `… a b → … (a = b)` | Typed equality. |
| 0xFF | `HALT` | n/a | Stop cleanly; final stack is output. |

### 3.2 Typing

The stack is strictly typed at run time. Wrong-typed inputs raise `VmError::TypeMismatch { expected, got }`. There is no implicit coercion. `EQ` requires both operands to have the same type.

### 3.3 Step budget

Each opcode costs one step. The default budget is 100 000 steps; running out raises `VmError::OutOfGas`. The budget exists to bound denial-of-service surface, not to price computation — pricing lives in the protocol fee market, outside this crate.

### 3.4 `RECURRING` and the scheduler

`RECURRING` does not loop inside the executor. That would make a contract's gas cost depend on wall-clock time, which is an attack surface. Instead, `RECURRING` emits a `Schedule { interval, window }` descriptor to the executor's outbox, and the block-driven scheduler replays the contract body deterministically at each tick using `Schedule::ticks_before(now)`.

The split is **the** structural reason recurring contracts (salary, subscription) are safe under arbitrarily long pauses. See `Schedule::ticks_before` and `Schedule::tick_at` for the catch-up semantics.

### 3.5 Composability with the ZK privacy layer

Component 1 verifies state transitions in-circuit. The temporal VM is circuit-friendly by construction:

- Bounded stack depth (default 1024).
- Bounded step count (default 100 000).
- Total error semantics (every input either produces a value or a named error).
- No off-chain reads.

A circuit verifying a private contract execution proves:

> Given (private) program `P`, (public) clock `C`, and (public) prior contract state `S`, the executor produces (public) output `O` and emits (public) schedule set `Σ`.

Nothing in the executor reads private state; the privacy layer feeds inputs in and reads outputs out at the boundary.

---

## 4. Conditional release

### 4.1 Three-valued resolution

```
enum Resolution { Ready, Pending, Failed }
```

The three-valued algebra is the entire point of the conditions crate. A naive boolean would conflate "wait" with "give up", and we need that distinction for both correctness (don't slash a recurring payment that simply hasn't reached its first tick) and UX ("we are waiting on the market" is not the same message as "this can never resolve").

| Op | `Ready` | `Pending` | `Failed` |
| --- | --- | --- | --- |
| `Ready ∧ x` | `x` | `Pending` | `Failed` |
| `Pending ∧ x` | `Pending` | `Pending` | `Failed` |
| `Failed ∧ x` | `Failed` | `Failed` | `Failed` |
| `Ready ∨ x` | `Ready` | `Ready` | `Ready` |
| `Pending ∨ x` | `Ready` | `Pending` | `Pending` |
| `Failed ∨ x` | `Ready` | `Pending` | `Failed` |
| `¬ Ready` | — | — | `Failed` |
| `¬ Pending` | — | — | `Pending` |
| `¬ Failed` | — | — | `Ready` |

### 4.2 Time leaves

| Leaf | `Ready` when | `Failed` when |
| --- | --- | --- |
| `After(ts)` | `now > ts` | never (always `Pending` until `Ready`) |
| `Before(ts)` | `now < ts` | `now ≥ ts` |
| `Deadline(ts)` | `now < ts` | `now ≥ ts` |
| `Within(interval)` | `interval.contains(now)` | `now ≥ interval.end` |

`Before` and `Deadline` are semantically identical at the resolver level. The distinction is in how the contract templates handle the transition — `Deadline` failure typically triggers a hard revert, `Before` failure typically transitions to a refund.

### 4.3 External leaves and `OracleResolver`

External (prediction-market) conditions resolve through the `OracleResolver` trait:

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

The temporal VM ships a `PendingResolver` for tests. The production resolver lives in Component 3 (prediction markets) and resolves against on-chain market state.

---

## 5. P2P primitive contracts

### 5.1 Escrow

```
Funding ──fund both──> Active ──confirm both──> Released
                            ├──abort both before deadline──> Refunded
                            └──open dispute──> Disputed ──settle──> (Released | refunded)
```

Invariants:

- A unilateral abort does **not** refund. Both parties must request abort within the deadline, otherwise the abort window closes and the only ways out are mutual confirm or dispute.
- The dispute path attaches an oracle reference + an "expected" outcome hash. The oracle answer chosen at the contract's creation time is the one that releases to the seller; any other outcome refunds to the buyer.
- Bad-faith aborts (one-sided requests that close the window) emit a `StaleAbort` reputation event consumed by Component 4.

### 5.2 Recurring payment

```
RecurringPayment {
    payer, payee, amount_per_tick, interval, window,
    balance, ticks_paid, ticks_missed, consecutive_missed, paused_at,
}
```

`advance(clock)` is the only state-changing transition for ticking:

1. If paused, return empty.
2. Compute `due = schedule.ticks_before(clock.now())`.
3. For each tick between `processed` and `due`:
   - If `balance ≥ amount_per_tick`: pay, increment `ticks_paid`, reset `consecutive_missed`.
   - Else: increment `ticks_missed` and `consecutive_missed`.
4. Return payouts.

Missed ticks are recorded, not silently dropped. The `consecutive_missed` counter is the input to the disputes crate's `ReputationEvent::PaymentsMissed`.

Pause / resume freezes payouts but does not freeze the clock. Ticks that would have fired during a pause catch up on resume. This matches the intent: a pause is a "no payouts" window, not a "no clock" window.

### 5.3 Milestone

A pre-funded escrow split into N ordered stages, each gated by a `Condition`. Stages are processed in declaration order. The first `Pending` stage stops further processing (the contract stays `Active`). The first `Failed` stage halts the contract, refunds the remainder to the payer, and transitions to `Failed`.

Stage amounts must sum to the total deposit — `Milestone::new` rejects misconfigured contracts with `BadConfig`.

Dispute path: settle the entire remaining balance to either party based on an oracle outcome.

### 5.4 Dead man's switch

Inactivity timer with weighted multi-beneficiary payout.

- `heartbeat(owner, clock)` resets `last_heartbeat`.
- `try_trigger(clock)` fires if `clock.now() > last_heartbeat + threshold`.
- Beneficiary shares are integer-divided; the rounding residue goes to the **last** beneficiary, so the sum of payouts equals the deposit exactly.

`try_trigger` is **callable by anyone**, not just beneficiaries. The chain itself drives it at every block in the integration layer, but a beneficiary may poke the contract without depending on chain-driven evaluation.

---

## 6. Integration boundaries

The temporal VM is one of five components. The interfaces below are the entire surface area between this component and the rest of the protocol.

### 6.1 Privacy layer (Component 1)

Consumed *from* the temporal VM:

- `Payout { to, amount, reason }` — the privacy layer translates each payout into a private note spend at settlement time.
- `Bytes32` — the privacy layer's diversified-address output is the input to every `Address` field in the contracts crate.

The privacy layer's circuit witnesses the temporal VM's transition function. Determinism, total error semantics, and bounded-step execution are the properties the circuit relies on; see §3.5.

### 6.2 Prediction market engine (Component 3)

Implements:

```rust
pub trait OracleResolver {
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer;
}
```

The temporal VM does not look inside a `Bytes32` oracle reference; the prediction-market engine owns the namespace and the resolution logic. The conditions crate is the only place that consumes this trait.

### 6.3 Veil Score / reputation (Component 4)

Implements:

```rust
pub trait ReputationSink {
    fn record(&mut self, event: ReputationEvent);
}

pub enum ReputationEvent {
    DisputeSettled(DisputeOutcome),
    PaymentsMissed { contract_id, defaulter, consecutive },
    StaleAbort(StaleAbort),
    CleanRelease { contract_id, parties },
}
```

The temporal VM emits events; the Veil Score engine consumes them and updates the private reputation score. The temporal VM never computes a score itself.

---

## 7. Slashing policy

```rust
SlashRules {
    max_permille: 100,          // 10 % standard slash
    bad_faith_bonus_permille: 200, // +20 % on bad faith
}
```

Total slash ≤ 30 % of the loser's bonded stake (saturating at 100 %). One source of truth across all four contract templates.

Bad-faith determination is made by Component 4 (Veil Score) when consuming `DisputeOutcome` events — not by the contract templates themselves. The temporal VM emits the *event*; the reputation pipeline emits the *judgment*.

---

## 8. Attack surface

Explicitly covered by tests:

- **Timestamp manipulation backwards.** `ManualClock::set` panics on rewind; `RecurringPayment::advance` is idempotent at a fixed clock value (replaying the same block doesn't extract additional payouts).
- **Grief via unbounded ticks.** `Schedule::ticks_before` is capped at `window.end`. A clock value far past the window produces exactly `window_duration / interval` ticks, not unbounded work.
- **Simultaneous confirm / abort.** Whichever transition consensus orders first wins; the second transition rejects cleanly with `InvalidState`. No state corruption.
- **Deadline-after race.** `DEADLINE` is a hard revert (`DeadlineExceeded`), distinct from `AFTER` / `BEFORE`'s `GuardFailed`. Explorers can label the failure honestly.
- **Type confusion.** All opcodes typed at run time; wrong-typed inputs raise `TypeMismatch` rather than coercing.
- **Step-budget exhaustion.** `OutOfGas` after a configurable bound.

Not yet covered (open work, will land before Component 5 / final assembly):

- Cross-contract reentrancy. The VM is currently single-contract; reentrancy semantics will be defined alongside the integration layer.
- Validator-collusion bounds. The clamping rule for `now` (median, monotonic) is specified but the formal bound on validator skew is part of the consensus spec, not this component.

---

## 9. Crate map

```
protocol/temporal-vm/
├── Cargo.toml                  workspace
├── rust-toolchain.toml         pinned to 1.95.0
├── README.md
├── SPEC.md                     this document
└── crates/
    ├── lace-time/              Timestamp, Duration, Interval, Clock
    ├── lace-vm/                opcodes, executor, scheduler
    ├── lace-conditions/        three-valued Condition algebra
    ├── lace-contracts/         escrow, recurring, milestone, dead-man
    │   └── tests/              cross-crate integration tests
    └── lace-disputes/          slashing rules, reputation events
```

Test count: 55 (5 conditions + 18 contracts unit + 6 contracts integration + 6 disputes + 6 time + 14 vm).
