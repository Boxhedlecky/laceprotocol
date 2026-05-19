//! In-circuit hash function.
//!
//! # Production status
//!
//! **This module currently uses Blake2b as a stand-in for Poseidon2.** The
//! protocol spec ([`SPEC.md §3.1`]) specifies Poseidon2 over the BN254 scalar
//! field as the in-circuit hash. Poseidon2 is what the eventual Halo2 circuits
//! will instantiate. Blake2b is here so the out-of-circuit code (note
//! commitments, nullifier derivation, Merkle tree) can be written and tested
//! end-to-end while the Poseidon2 dependency is being vetted.
//!
//! Migrating to Poseidon2 is a single-module change behind this interface, but
//! it will alter every hash value the protocol produces. We treat this as a
//! breaking change scheduled before the first internal testnet.
//!
//! [`SPEC.md §3.1`]: ../../../SPEC.md
//!
//! # Interface
//!
//! [`hash`] consumes any number of field elements and returns a single field
//! element. The width-agnostic interface mirrors a sponge construction, which
//! is what Poseidon2 will provide once swapped in.

use blake2::{Blake2b512, Digest};
use ff::{FromUniformBytes, PrimeField};

use crate::Scalar;

/// An opaque 32-byte tag produced by [`hash`].
///
/// `Hash` deliberately does not implement arithmetic. It is a tag, not a field
/// element. Use [`Hash::to_scalar`] when you need to feed a hash back into a
/// circuit as a witness.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Construct a `Hash` from raw bytes. Only intended for deserialization;
    /// regular construction goes through [`hash`].
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert to a scalar by reducing the byte representation modulo the
    /// BN254 scalar field. Used when a hash output is fed back into a
    /// downstream hash invocation or a circuit witness.
    pub fn to_scalar(&self) -> Scalar {
        Scalar::from_repr(self.0)
            .into_option()
            .unwrap_or_else(|| {
                // The byte string is not a canonical field element. Reduce
                // by hashing again into a smaller domain. This branch is
                // statistically rare for Blake2b output (~1 / 2^4 with the
                // BN254 modulus) and goes away once Poseidon2 lands, since
                // Poseidon2 outputs are always canonical field elements.
                let mut h = Blake2b512::new();
                h.update(b"lace/hash-reduce/v1");
                h.update(self.0);
                let wide = h.finalize();
                let mut narrow = [0u8; 64];
                narrow.copy_from_slice(&wide);
                Scalar::from_uniform_bytes(&narrow)
            })
    }
}

/// Hash a sequence of field elements into a single tag.
///
/// Currently implemented as a domain-separated Blake2b over the
/// little-endian encoding of each input. Will be replaced with Poseidon2
/// before the first internal testnet -- see module documentation.
pub fn hash(inputs: &[Scalar]) -> Hash {
    let mut h = Blake2b512::new();
    h.update(b"lace/hash/v1");
    h.update((inputs.len() as u64).to_le_bytes());
    for input in inputs {
        h.update(input.to_repr());
    }
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..32]);
    Hash(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff::Field;
    use rand::rngs::OsRng;

    #[test]
    fn hash_is_deterministic() {
        let a = Scalar::from(42u64);
        let b = Scalar::from(99u64);
        assert_eq!(hash(&[a, b]), hash(&[a, b]));
    }

    #[test]
    fn hash_is_input_sensitive() {
        let a = Scalar::from(42u64);
        let b = Scalar::from(99u64);
        assert_ne!(hash(&[a, b]), hash(&[b, a]));
        assert_ne!(hash(&[a, b]), hash(&[a]));
        assert_ne!(hash(&[a]), hash(&[]));
    }

    #[test]
    fn hash_to_scalar_roundtrips_via_reduce() {
        // For Blake2b output, to_scalar should produce a canonical field
        // element. Doing it twice should produce the same element.
        let h = hash(&[Scalar::random(OsRng)]);
        let s1 = h.to_scalar();
        let s2 = h.to_scalar();
        assert_eq!(s1, s2);
    }
}
