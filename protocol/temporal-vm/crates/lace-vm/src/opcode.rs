//! Bytecode opcodes for the temporal VM.
//!
//! The five time opcodes named in the protocol brief (`AFTER`,
//! `BEFORE`, `DEADLINE`, `RECURRING`, `TIMEDELTA`) sit alongside a
//! minimal supporting opcode set (stack push/pop/dup/swap, equality,
//! `NOW`, `AND`, `OR`, `NOT`, `HALT`). Everything else a contract
//! needs is built by composition.

use lace_time::{Duration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::value::{Bytes32, Value};

/// A single instruction.
///
/// We use an enum rather than a raw byte stream because the executor
/// is internal-only -- there's no third-party JIT to satisfy -- and
/// the enum form is dramatically easier to test, audit, and serialise
/// for replay. The wire format pins one byte per discriminant; see
/// [`Op::tag`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    // --- Stack & literals -------------------------------------------------
    /// Push a literal value.
    Push(Value),
    /// Drop the top of stack.
    Pop,
    /// Duplicate the top of stack.
    Dup,
    /// Swap the top two stack entries.
    Swap,
    /// Push the current consensus time.
    Now,
    /// Push the current block height.
    Height,

    // --- Time opcodes (the five named primitives) -------------------------
    /// `AFTER` -- pop a `Time` and continue iff `now > ts`, otherwise raise
    /// [`VmError::GuardFailed`](crate::value::VmError::GuardFailed).
    After,
    /// `BEFORE` -- pop a `Time` and continue iff `now < ts`.
    Before,
    /// `DEADLINE` -- pop a `Time` and abort with
    /// [`VmError::DeadlineExceeded`](crate::value::VmError::DeadlineExceeded)
    /// if `now > ts`. Unlike `AFTER` / `BEFORE` this is a hard
    /// failure, not a soft guard: it surfaces as a distinct error so
    /// contracts can branch on it.
    Deadline,
    /// `RECURRING(interval, start, end)` -- pop three values (top of
    /// stack first: `end: Time`, then `start: Time`, then `interval:
    /// Duration`) and yield a [`crate::scheduler::Schedule`] to the
    /// caller via the executor's outbox.
    Recurring,
    /// `TIMEDELTA(t1, t2)` -- pop two timestamps (top of stack first:
    /// `t2`, then `t1`) and push the signed delta `t2 - t1`.
    TimeDelta,

    // --- Boolean logic -----------------------------------------------------
    /// `&&` over two booleans.
    And,
    /// `||` over two booleans.
    Or,
    /// `!` over one boolean.
    Not,
    /// Equality. Pops two values of identical type and pushes a `Bool`.
    Eq,

    // --- Control -----------------------------------------------------------
    /// Halt. The executor stops cleanly; the stack is the program output.
    Halt,
}

impl Op {
    /// Wire tag for this opcode. Used for canonical serialisation. We
    /// commit to a stable mapping so on-chain bytecode is reproducible
    /// across implementations.
    pub const fn tag(&self) -> u8 {
        match self {
            Op::Push(_) => 0x01,
            Op::Pop => 0x02,
            Op::Dup => 0x03,
            Op::Swap => 0x04,
            Op::Now => 0x05,
            Op::Height => 0x06,
            Op::After => 0x10,
            Op::Before => 0x11,
            Op::Deadline => 0x12,
            Op::Recurring => 0x13,
            Op::TimeDelta => 0x14,
            Op::And => 0x20,
            Op::Or => 0x21,
            Op::Not => 0x22,
            Op::Eq => 0x23,
            Op::Halt => 0xFF,
        }
    }
}

/// A compiled program -- a flat sequence of [`Op`]s.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// Opcode sequence.
    pub ops: Vec<Op>,
}

impl Program {
    /// Empty program.
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Builder helper: append an opcode and return self by mutable ref.
    pub fn push(&mut self, op: Op) -> &mut Self {
        self.ops.push(op);
        self
    }

    /// Builder helper: append a literal push.
    pub fn literal(&mut self, v: Value) -> &mut Self {
        self.ops.push(Op::Push(v));
        self
    }

    /// Builder helper: append a `Time` literal push.
    pub fn time(&mut self, ts: Timestamp) -> &mut Self {
        self.literal(Value::Time(ts))
    }

    /// Builder helper: append a `Duration` literal push.
    pub fn duration(&mut self, d: Duration) -> &mut Self {
        self.literal(Value::Duration(d))
    }

    /// Builder helper: append a `Bytes32` literal push.
    pub fn bytes(&mut self, b: Bytes32) -> &mut Self {
        self.literal(Value::Bytes(b))
    }
}
