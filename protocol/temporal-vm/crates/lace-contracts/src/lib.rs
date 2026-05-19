//! P2P primitive contract templates.
//!
//! Four templates, each a small deterministic state machine over the
//! types in [`lace_time`] and [`lace_conditions`]:
//!
//! - [`escrow::Escrow`] -- mutual-confirm escrow with dispute escalation.
//! - [`recurring::RecurringPayment`] -- salary / subscription / loan.
//! - [`milestone::Milestone`] -- staged release tied to mixed conditions.
//! - [`deadman::DeadMan`] -- inactivity-triggered transfer.
//!
//! Each template owns a pool of funds (modelled abstractly as a
//! [`Amount`]) and exposes a small set of transitions. State machines
//! are kept distinct rather than unified behind a trait because their
//! transition vocabularies don't generalise cleanly: an escrow has a
//! `confirm`, a recurring has a `pause`, a deadman has a `heartbeat`.
//! Folding all of that into a single trait would lose more than it
//! would save.
//!
//! Funds movement is intentionally abstract. The contracts express
//! *which* address should receive *how much* under *which* condition;
//! the actual settlement (private note spends, fee collection,
//! reputation updates) happens in the integration layer when the
//! components are assembled into the full protocol.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

use lace_vm::Bytes32;
use serde::{Deserialize, Serialize};

pub mod deadman;
pub mod escrow;
pub mod milestone;
pub mod recurring;

/// An on-chain address. Opaque 32 bytes -- the privacy layer
/// supplies the actual diversified-address derivation.
pub type Address = Bytes32;

/// An amount of LACE, in base units (no fixed decimal scaling
/// inside this crate). Settlement happens against the privacy
/// layer's note representation, which is responsible for unit
/// agreement.
pub type Amount = u128;

/// A unique contract instance identifier. Assigned by the chain at
/// contract creation; the templates here treat it as opaque.
pub type ContractId = Bytes32;

/// A planned funds movement. Templates emit a list of these on every
/// successful transition; the integration layer is responsible for
/// translating them into note spends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payout {
    /// Destination address.
    pub to: Address,
    /// Amount, in base units.
    pub amount: Amount,
    /// Free-form tag describing the reason. Stable across
    /// implementations so block explorers can render consistent
    /// labels.
    pub reason: PayoutReason,
}

/// Why a payout fired. Used purely for explorer display and for
/// downstream reputation accounting.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayoutReason {
    /// Escrow released under mutual confirmation.
    EscrowRelease,
    /// Escrow refunded after a both-party abort.
    EscrowRefund,
    /// Escrow paid out per dispute outcome.
    EscrowDisputed,
    /// Recurring payment tick.
    RecurringTick,
    /// Milestone release.
    MilestoneRelease,
    /// Inheritance / dead-man payout.
    Inheritance,
    /// Slashing payment (penalty deducted from a counterparty).
    Slash,
}

/// Errors raised by the contract templates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractError {
    /// Caller is not one of the recognised participants.
    UnauthorisedParty,
    /// Transition requires a state we are not in.
    InvalidState(&'static str),
    /// A condition resolved to `Failed` and the contract cannot
    /// recover from it.
    ConditionFailed,
    /// Numeric overflow in funds arithmetic. Shouldn't happen at
    /// real-world LACE amounts, but the templates verify rather than
    /// trust.
    AmountOverflow,
    /// Configuration is internally inconsistent (e.g. milestone
    /// amounts sum to more than the deposit).
    BadConfig(&'static str),
}
