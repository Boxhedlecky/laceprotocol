//! Lace privacy layer -- field, hash, and key-derivation primitives.
//!
//! This crate is the foundation that every other privacy-layer crate depends
//! on. It exposes:
//!
//! - [`Scalar`]: the BN254 scalar field, used as the native field for all
//!   circuit arithmetic.
//! - [`Hash`]: an opaque 32-byte tag produced by the in-circuit hash function.
//! - [`hash`]: the in-circuit hash function (currently a Blake2b-based stand-in
//!   pending Poseidon2 integration; see module docs for the migration plan).
//! - [`keys`]: the wallet key hierarchy (`SpendingKey`, `FullViewingKey`,
//!   `IncomingViewingKey`, `OutgoingViewingKey`, `NullifierKey`).
//! - [`address`]: diversified address derivation.
//!
//! ## Stability
//!
//! Pre-1.0. Every public type may change. The hash function in particular is
//! a stand-in; switching to Poseidon2 will change every commitment, nullifier,
//! and Merkle-root value the protocol produces. Do not persist data produced
//! by this crate against the assumption it will remain valid.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod address;
pub mod hash;
pub mod keys;

/// Re-export of the BN254 scalar field as the native circuit field.
///
/// All in-circuit arithmetic in the Lace privacy layer operates over this
/// field. Out-of-circuit code that needs to feed values into circuits should
/// convert to `Scalar` at the boundary rather than carrying the field type
/// through application code.
pub type Scalar = halo2curves::bn256::Fr;

pub use hash::{hash, Hash};
