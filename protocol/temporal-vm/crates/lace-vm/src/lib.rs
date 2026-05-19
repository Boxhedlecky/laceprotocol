//! Lace temporal VM -- bytecode, opcodes, executor, scheduler.
//!
//! This crate hosts the *evaluation engine* for time-typed bytecode.
//! It does not know about money, escrow, or reputation; it only knows
//! how to evaluate a [`Program`] against a [`Clock`] and emit
//! [`Schedule`]s for recurring contracts.
//!
//! Composability with the privacy layer (Component 1) is structural:
//! every opcode is total over its typed inputs, every error is
//! deterministic, and no opcode reads off-chain state. This is the
//! property the ZK circuit relies on -- a recipient who is shown the
//! decrypted program and the schedule outputs can verify the
//! transition without holding any private witness beyond the spend
//! key.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod executor;
pub mod opcode;
pub mod scheduler;
pub mod value;

pub use executor::{Executor, Outputs, DEFAULT_MAX_STACK, DEFAULT_STEP_BUDGET};
pub use opcode::{Op, Program};
pub use scheduler::Schedule;
pub use value::{Bytes32, Value, VmError};
