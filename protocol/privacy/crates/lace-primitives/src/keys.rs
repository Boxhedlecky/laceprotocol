//! Wallet key hierarchy.
//!
//! A wallet holds a single [`SpendingKey`] from which every other key is
//! deterministically derived. Different keys give different capabilities:
//!
//! ```text
//!     SpendingKey (sk)              -- can do everything
//!         |
//!         |  derive
//!         v
//!     FullViewingKey (fvk)          -- can scan AND derive nullifiers (self-audit)
//!        / \
//!       /   \
//!      v     v
//!  IncomingVK  OutgoingVK           -- can scan only (no nullifiers)
//!  NullifierKey (nk)                -- separately exposed; needed in-circuit
//! ```
//!
//! Keys are pure-function derivations: re-deriving from the same `SpendingKey`
//! yields the same downstream keys. There is no key-generation randomness
//! beyond the original `SpendingKey`.
//!
//! # Production status
//!
//! Key derivation uses [`crate::hash`] with distinct domain-separation tags.
//! Because the underlying hash is currently a Blake2b stand-in, persisted
//! keys will change when Poseidon2 lands. Do not use this for real wallets.

use core::fmt;

use ff::{FromUniformBytes, PrimeField};
use rand_core::{CryptoRng, RngCore};
use subtle::{Choice, ConstantTimeEq};

use crate::{hash, Scalar};

/// The root secret of a wallet. Never leaves the wallet device.
///
/// Wrapped in a newtype to make accidental serialization or logging visually
/// obvious. The inner field element is intentionally not `Copy` to discourage
/// implicit duplication.
#[derive(Clone)]
pub struct SpendingKey(Scalar);

impl SpendingKey {
    /// Generate a fresh spending key from a CSPRNG.
    pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let mut bytes = [0u8; 64];
        rng.fill_bytes(&mut bytes);
        Self(Scalar::from_uniform_bytes(&bytes))
    }

    /// Construct from a known scalar (test vectors, key import).
    pub fn from_scalar(s: Scalar) -> Self {
        Self(s)
    }

    /// Derive the full viewing key.
    pub fn full_viewing_key(&self) -> FullViewingKey {
        let tag = Scalar::from_uniform_bytes(&pad_tag(b"lace/keys/fvk"));
        FullViewingKey(hash(&[self.0, tag]).to_scalar())
    }

    /// Derive the nullifier key.
    ///
    /// The nullifier key is exposed separately from the FVK because the spend
    /// circuit needs it as a witness, whereas an FVK is the right object to
    /// hand to a wallet-watching service. Keeping them separate means a
    /// watching service never sees `nk`.
    pub fn nullifier_key(&self) -> NullifierKey {
        let tag = Scalar::from_uniform_bytes(&pad_tag(b"lace/keys/nk"));
        NullifierKey(hash(&[self.0, tag]).to_scalar())
    }

    /// Borrow the inner scalar. Intentionally pub(crate) -- downstream crates
    /// should derive keys, not read the spending key directly.
    #[allow(dead_code)] // consumed by lace-notes, which has not landed yet
    pub(crate) fn inner(&self) -> &Scalar {
        &self.0
    }
}

impl ConstantTimeEq for SpendingKey {
    fn ct_eq(&self, other: &Self) -> Choice {
        // Field elements expose a constant-time eq via their byte repr.
        self.0.to_repr().ct_eq(&other.0.to_repr())
    }
}

impl PartialEq for SpendingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).into()
    }
}
impl Eq for SpendingKey {}

impl fmt::Debug for SpendingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the inner scalar. Accidental Debug logging of a
        // spending key would compromise every note ever sent to this wallet.
        f.write_str("SpendingKey(<redacted>)")
    }
}

/// A key that can both decrypt incoming notes and derive nullifiers. Suitable
/// for self-audit and recovery.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FullViewingKey(Scalar);

impl FullViewingKey {
    /// Derive the incoming viewing key (decrypt only, no nullifiers).
    pub fn incoming(&self) -> IncomingViewingKey {
        let tag = Scalar::from_uniform_bytes(&pad_tag(b"lace/keys/ivk"));
        IncomingViewingKey(hash(&[self.0, tag]).to_scalar())
    }

    /// Derive the outgoing viewing key (decrypt own outgoing notes).
    pub fn outgoing(&self) -> OutgoingViewingKey {
        let tag = Scalar::from_uniform_bytes(&pad_tag(b"lace/keys/ovk"));
        OutgoingViewingKey(hash(&[self.0, tag]).to_scalar())
    }

    /// Inner scalar. Crate-internal; the `lace-notes` crate (next commit)
    /// is the intended consumer.
    #[allow(dead_code)] // consumed by lace-notes, which has not landed yet
    pub(crate) fn inner(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for FullViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FullViewingKey(<redacted>)")
    }
}

/// A key that decrypts incoming notes. Cannot derive nullifiers, so cannot be
/// used to learn which of the wallet's notes are still unspent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IncomingViewingKey(Scalar);

impl IncomingViewingKey {
    /// Inner scalar. Crate-internal; the `lace-notes` crate (next commit)
    /// is the intended consumer.
    #[allow(dead_code)] // consumed by lace-notes, which has not landed yet
    pub(crate) fn inner(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IncomingViewingKey(<redacted>)")
    }
}

/// A key that decrypts the sender-side metadata of the wallet's own outgoing
/// notes (useful for backup / audit of "what did I send").
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OutgoingViewingKey(Scalar);

impl OutgoingViewingKey {
    /// Inner scalar. Crate-internal; the `lace-notes` crate (next commit)
    /// is the intended consumer.
    #[allow(dead_code)] // consumed by lace-notes, which has not landed yet
    pub(crate) fn inner(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for OutgoingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutgoingViewingKey(<redacted>)")
    }
}

/// A key used in-circuit to derive nullifiers. Exposed separately because the
/// spend circuit needs it as a private witness; handing out a `NullifierKey`
/// to a watcher would let them link your future spends.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NullifierKey(Scalar);

impl NullifierKey {
    /// Inner scalar. Crate-internal; the `lace-notes` crate (next commit)
    /// is the intended consumer.
    #[allow(dead_code)] // consumed by lace-notes, which has not landed yet
    pub(crate) fn inner(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for NullifierKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NullifierKey(<redacted>)")
    }
}

/// Pad a short ASCII tag into 64 bytes for `Scalar::from_uniform_bytes`.
///
/// Domain-separation tags need to land in field elements for the hash inputs.
/// We pad with zeros after the tag bytes; tags are constants in source, so
/// uniqueness is enforced at code-review time, not at runtime.
fn pad_tag(tag: &[u8]) -> [u8; 64] {
    debug_assert!(tag.len() <= 64, "domain tag too long");
    let mut out = [0u8; 64];
    out[..tag.len()].copy_from_slice(tag);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn derivation_is_deterministic() {
        let sk = SpendingKey::from_scalar(Scalar::from(12345u64));
        let fvk1 = sk.full_viewing_key();
        let fvk2 = sk.full_viewing_key();
        assert_eq!(fvk1, fvk2);

        let ivk1 = fvk1.incoming();
        let ivk2 = fvk1.incoming();
        assert_eq!(ivk1, ivk2);
    }

    #[test]
    fn different_sks_produce_different_fvks() {
        let sk1 = SpendingKey::from_scalar(Scalar::from(1u64));
        let sk2 = SpendingKey::from_scalar(Scalar::from(2u64));
        assert_ne!(sk1.full_viewing_key(), sk2.full_viewing_key());
    }

    #[test]
    fn ivk_ovk_nk_are_pairwise_distinct() {
        // Domain separation must prevent the four derived keys from
        // colliding even given the same input scalar.
        let sk = SpendingKey::random(&mut OsRng);
        let fvk = sk.full_viewing_key();
        let ivk = fvk.incoming();
        let ovk = fvk.outgoing();
        let nk = sk.nullifier_key();

        assert_ne!(ivk.inner(), ovk.inner());
        assert_ne!(ivk.inner(), nk.inner());
        assert_ne!(ovk.inner(), nk.inner());
        assert_ne!(ivk.inner(), fvk.inner());
    }

    #[test]
    fn spending_key_eq_is_constant_time() {
        // Smoke test that ConstantTimeEq is wired up; we can't directly
        // measure constant-timeness here, but we can check it agrees with
        // semantic equality.
        let sk1 = SpendingKey::from_scalar(Scalar::from(7u64));
        let sk2 = SpendingKey::from_scalar(Scalar::from(7u64));
        let sk3 = SpendingKey::from_scalar(Scalar::from(8u64));
        assert!(bool::from(sk1.ct_eq(&sk2)));
        assert!(!bool::from(sk1.ct_eq(&sk3)));
    }
}
