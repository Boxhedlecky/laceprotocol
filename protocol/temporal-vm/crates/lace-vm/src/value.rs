//! Typed values on the VM stack.
//!
//! The Lace temporal VM is stack-based with strict static-ish typing
//! enforced at run time: every opcode expects a specific shape of
//! stack and produces a specific shape of stack. Wrong-typed values
//! abort with [`VmError::TypeMismatch`] rather than silently coercing.
//! This is deliberate; the VM is small enough that a richer type
//! system would be overkill, but loose coercion in EVM-style stack
//! VMs has historically been a source of contract bugs.

use lace_time::{BlockHeight, Duration, TimeDelta, Timestamp};
use serde::{Deserialize, Serialize};

/// A 32-byte opaque identifier.
///
/// Used for addresses, oracle references, and condition hashes. The
/// VM never looks inside one; it only compares them for equality.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Bytes32(pub [u8; 32]);

impl core::fmt::Debug for Bytes32 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "0x")?;
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl Bytes32 {
    /// All-zero identifier. Useful as a sentinel "unset" value.
    pub const ZERO: Self = Self([0; 32]);
}

/// A typed VM stack value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    /// Boolean.
    Bool(bool),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// A consensus timestamp.
    Time(Timestamp),
    /// A duration in seconds.
    Duration(Duration),
    /// A signed delta between two timestamps.
    Delta(TimeDelta),
    /// A block height.
    Height(BlockHeight),
    /// A 32-byte opaque identifier.
    Bytes(Bytes32),
}

impl Value {
    /// Static type tag for error reporting.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::U64(_) => "u64",
            Value::Time(_) => "time",
            Value::Duration(_) => "duration",
            Value::Delta(_) => "delta",
            Value::Height(_) => "height",
            Value::Bytes(_) => "bytes32",
        }
    }
}

/// Errors raised by the VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmError {
    /// Stack was popped while empty.
    StackUnderflow,
    /// Stack would exceed the per-execution depth bound.
    StackOverflow,
    /// Wrong-typed value on the stack.
    TypeMismatch {
        /// Type the opcode expected.
        expected: &'static str,
        /// Type actually found.
        got: &'static str,
    },
    /// `DEADLINE` fired: current time has passed the supplied deadline.
    DeadlineExceeded {
        /// Configured deadline.
        deadline: Timestamp,
        /// Current time at the moment the deadline was checked.
        now: Timestamp,
    },
    /// A guard opcode (`AFTER` / `BEFORE`) blocked further execution.
    GuardFailed,
    /// Gas / step budget exhausted. Time-based attack defence.
    OutOfGas,
    /// Bytecode referenced an opcode the VM doesn't recognise.
    InvalidOpcode(u8),
    /// `RECURRING` was given an unusable interval (zero, or `end < start`).
    InvalidSchedule(&'static str),
}

impl core::fmt::Display for VmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VmError::StackUnderflow => write!(f, "stack underflow"),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {}, got {}", expected, got)
            }
            VmError::DeadlineExceeded { deadline, now } => {
                write!(f, "deadline exceeded: deadline={} now={}", deadline, now)
            }
            VmError::GuardFailed => write!(f, "guard condition failed"),
            VmError::OutOfGas => write!(f, "out of gas"),
            VmError::InvalidOpcode(op) => write!(f, "invalid opcode 0x{:02x}", op),
            VmError::InvalidSchedule(reason) => write!(f, "invalid schedule: {}", reason),
        }
    }
}

impl Value {
    /// Pop helper -- expect a `Time`.
    pub fn into_time(self) -> Result<Timestamp, VmError> {
        match self {
            Value::Time(t) => Ok(t),
            other => Err(VmError::TypeMismatch {
                expected: "time",
                got: other.type_name(),
            }),
        }
    }
    /// Pop helper -- expect a `Duration`.
    pub fn into_duration(self) -> Result<Duration, VmError> {
        match self {
            Value::Duration(d) => Ok(d),
            other => Err(VmError::TypeMismatch {
                expected: "duration",
                got: other.type_name(),
            }),
        }
    }
    /// Pop helper -- expect a `Bool`.
    pub fn into_bool(self) -> Result<bool, VmError> {
        match self {
            Value::Bool(b) => Ok(b),
            other => Err(VmError::TypeMismatch {
                expected: "bool",
                got: other.type_name(),
            }),
        }
    }
}
