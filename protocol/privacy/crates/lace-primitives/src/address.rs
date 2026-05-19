//! Diversified shielded addresses.
//!
//! A single [`IncomingViewingKey`](crate::keys::IncomingViewingKey) can sponsor
//! `2^88` distinct on-chain addresses, each indexed by an 88-bit
//! [`Diversifier`]. The recipient decrypts notes to any of these addresses with
//! the same `ivk`. An observer cannot link two diversified addresses of the
//! same wallet.
//!
//! Real Lace addresses are points on the JubJub embedded curve. This module
//! currently models them as scalars; the JubJub group operation will be wired
//! in when the spend circuit is implemented, since the same JubJub instance
//! must be used in-circuit and out-of-circuit.
//!
//! # Production status
//!
//! `Address::derive` is a placeholder that produces a domain-separated hash of
//! `(ivk, diversifier)`. This is *not* the production address scheme -- the
//! production scheme will be `addr = ivk * G_d` where `G_d` is a
//! diversifier-dependent JubJub generator. The interface is the same; only
//! the body changes. See [`SPEC.md §4.4`].
//!
//! [`SPEC.md §4.4`]: ../../../SPEC.md

use ff::{FromUniformBytes, PrimeField};

use crate::{hash, keys::IncomingViewingKey, Scalar};

/// 88-bit diversifier index. Wallets typically pick a fresh diversifier per
/// payment request; 88 bits is enough that random selection collides with
/// probability < 2^-44 even at 10^10 requests.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Diversifier(pub [u8; 11]);

impl Diversifier {
    /// The all-zero diversifier. Used as the default for wallets that do not
    /// want per-payment unlinkability.
    pub const ZERO: Self = Self([0u8; 11]);

    /// Pad the 11-byte diversifier into 32 bytes for hash inputs.
    fn to_scalar(self) -> Scalar {
        let mut wide = [0u8; 64];
        wide[..11].copy_from_slice(&self.0);
        wide[11..23].copy_from_slice(b"lace/addr/d/");
        Scalar::from_uniform_bytes(&wide)
    }
}

/// A diversified shielded address. On the wire this is a 32-byte tag derived
/// from `(ivk, diversifier)`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Address(Scalar);

impl Address {
    /// Derive the address for the given incoming viewing key and diversifier.
    pub fn derive(ivk: &IncomingViewingKey, d: Diversifier) -> Self {
        Self(hash(&[*ivk.inner(), d.to_scalar()]).to_scalar())
    }

    /// Borrow the address as a field element. Used by the note commitment
    /// scheme in lace-notes.
    pub fn as_scalar(&self) -> &Scalar {
        &self.0
    }

    /// Serialize to 32 bytes (canonical little-endian field encoding).
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::SpendingKey;

    #[test]
    fn different_diversifiers_produce_different_addresses() {
        let sk = SpendingKey::from_scalar(Scalar::from(1u64));
        let ivk = sk.full_viewing_key().incoming();
        let a0 = Address::derive(&ivk, Diversifier::ZERO);
        let a1 = Address::derive(&ivk, Diversifier([1u8; 11]));
        assert_ne!(a0, a1);
    }

    #[test]
    fn derivation_is_deterministic() {
        let sk = SpendingKey::from_scalar(Scalar::from(1u64));
        let ivk = sk.full_viewing_key().incoming();
        let a = Address::derive(&ivk, Diversifier([7u8; 11]));
        let b = Address::derive(&ivk, Diversifier([7u8; 11]));
        assert_eq!(a, b);
    }

    #[test]
    fn different_ivks_produce_different_addresses_at_same_diversifier() {
        let sk1 = SpendingKey::from_scalar(Scalar::from(1u64));
        let sk2 = SpendingKey::from_scalar(Scalar::from(2u64));
        let ivk1 = sk1.full_viewing_key().incoming();
        let ivk2 = sk2.full_viewing_key().incoming();
        let a1 = Address::derive(&ivk1, Diversifier::ZERO);
        let a2 = Address::derive(&ivk2, Diversifier::ZERO);
        assert_ne!(a1, a2);
    }
}
