//! Common types for the Lace consensus mechanism.
//!
//! Mirrors the contract surface of the prior four components: opaque 32-byte
//! ids, `Amount` as `u128`, basis-point scalars for any value in `[0, 1]`,
//! `BlockHeight` as `u64`. Anything richer than these primitives lives in the
//! crate that owns the semantics (validator set in [`lace-cons-pos`], fork
//! choice state in [`lace-cons-fork-choice`], reward math in
//! [`lace-cons-rewards`], and so on).
//!
//! The types in this crate are deliberately *plain data*: they do not own
//! cryptographic verification, state transitions, or any side effects. That
//! lets the downstream crates compose them without picking up implicit
//! ordering constraints.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;
use serde::{Deserialize, Serialize};

/// A 32-byte opaque identifier. Same shape as the temporal-VM,
/// prediction-market, and reputation `Bytes32`; used here for block hashes,
/// state roots, DA commitments, validator public-key digests, and proof
/// commitments.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Bytes32(pub [u8; 32]);

impl Bytes32 {
    /// All zero bytes. Useful as a sentinel and in tests.
    pub const ZERO: Bytes32 = Bytes32([0u8; 32]);

    /// Construct from a raw byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Bytes32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl From<[u8; 32]> for Bytes32 {
    fn from(b: [u8; 32]) -> Self {
        Self(b)
    }
}

/// A validator's stable identifier. Distinct nominal type so the validator
/// set API cannot accidentally take a block hash or state root where a
/// validator id is expected.
///
/// In production this is the BLAKE2b-256 digest of the validator's consensus
/// public key. In the devnet it is a synthetic id derived from the
/// validator's seat index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ValidatorId(pub Bytes32);

impl ValidatorId {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A block hash. A `Bytes32` newtype kept distinct from `StateRoot` and
/// other ids so they cannot be silently swapped.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockHash(pub Bytes32);

impl BlockHash {
    /// All-zero sentinel. Used as the parent hash of the genesis block.
    pub const GENESIS_PARENT: BlockHash = BlockHash(Bytes32::ZERO);

    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A commitment to post-block state. The state tree itself is owned by the
/// node's storage layer; this is what gets signed and proven over.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StateRoot(pub Bytes32);

impl StateRoot {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A commitment to a block's data-availability payload (transactions and
/// associated witnesses). Consumed by [`crate::da`] machinery; see
/// `lace-cons-da` for the actual store and sampling.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DaCommitment(pub Bytes32);

impl DaCommitment {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A commitment to a validity proof (the recursive aggregate that ZK
/// execution emits). The verifier resolves this to a real proof artifact
/// through `lace-cons-zk-exec`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProofCommitment(pub Bytes32);

impl ProofCommitment {
    /// Wrap a 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Bytes32::new(bytes))
    }
}

/// A non-negative quantity in the smallest indivisible LACE unit.
///
/// `u128` for consistency with every other component: intermediate
/// computations in reward weighting and slashing distribution can briefly
/// exceed `u64`.
pub type Amount = u128;

/// A block height. `u64` matches the temporal-VM `BlockHeight`.
pub type BlockHeight = u64;

/// A consensus slot. One slot is one block production opportunity. Slots
/// advance monotonically and are independent of clock time; the temporal VM
/// owns the block-time semantics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Slot(pub u64);

impl Slot {
    /// The genesis slot.
    pub const GENESIS: Slot = Slot(0);

    /// Construct from a raw `u64`.
    pub const fn new(s: u64) -> Self {
        Self(s)
    }

    /// Raw value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The next slot.
    pub const fn next(self) -> Slot {
        Slot(self.0 + 1)
    }
}

/// A consensus epoch. An epoch is a fixed window of slots; the validator
/// set is frozen for the duration of an epoch.
///
/// Epoch length is set by `EpochSchedule` in `lace-cons-pos`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Epoch(pub u64);

impl Epoch {
    /// The genesis epoch.
    pub const GENESIS: Epoch = Epoch(0);

    /// Construct from a raw `u64`.
    pub const fn new(e: u64) -> Self {
        Self(e)
    }

    /// Raw value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The next epoch.
    pub const fn next(self) -> Epoch {
        Epoch(self.0 + 1)
    }
}

/// A basis-point scalar (0..=10_000). Same shape as `lace-veil-types::Bps`;
/// every reputation multiplier and reward fraction is quoted in bps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Bps(u32);

impl Bps {
    /// Zero.
    pub const ZERO: Bps = Bps(0);
    /// One hundred per cent.
    pub const ONE: Bps = Bps(10_000);
    /// Maximum raw basis-point value.
    pub const MAX: u32 = 10_000;

    /// Construct from a raw basis-point value. Saturates above 10_000.
    pub const fn from_bps(bps: u32) -> Self {
        if bps > Self::MAX {
            Bps(Self::MAX)
        } else {
            Bps(bps)
        }
    }

    /// Raw value in 0..=10_000.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Apply this fraction to an amount: `floor(amount * self / 10_000)`.
    pub const fn apply(self, amount: Amount) -> Amount {
        amount.saturating_mul(self.0 as Amount) / 10_000
    }
}

/// The multiplier a validator's Veil Score contributes to proposer selection
/// weight and to block reward share.
///
/// Quoted in basis points *above* one — `0` means "no multiplier, plain
/// stake weighting", `5_000` means "1.5×", and so on. Caps at `MAX_MULTIPLIER`
/// so that a single high-score validator cannot dominate the set.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReputationMultiplier(u32);

impl ReputationMultiplier {
    /// No reputation effect (multiplier = 1.0).
    pub const NEUTRAL: ReputationMultiplier = ReputationMultiplier(0);

    /// Cap on the bonus a validator may earn from reputation. `10_000` =
    /// "up to 2.0× plain stake". `// TODO(governance)`: launch committee
    /// adjusts.
    pub const MAX_BONUS_BPS: u32 = 10_000;

    /// Construct from a raw bonus in bps (0 = neutral). Saturates at
    /// [`Self::MAX_BONUS_BPS`].
    pub const fn from_bonus_bps(bps: u32) -> Self {
        if bps > Self::MAX_BONUS_BPS {
            ReputationMultiplier(Self::MAX_BONUS_BPS)
        } else {
            ReputationMultiplier(bps)
        }
    }

    /// Raw bonus in basis points (0..=MAX_BONUS_BPS).
    pub const fn raw_bonus_bps(self) -> u32 {
        self.0
    }

    /// Apply the multiplier to a base value: `base * (1 + bonus/10_000)`.
    /// Saturating; preserves invariants under extreme stake.
    pub const fn apply(self, base: Amount) -> Amount {
        let bonus = (base.saturating_mul(self.0 as Amount)) / 10_000;
        base.saturating_add(bonus)
    }
}

/// A staked amount, including any pending unbond. Distinct nominal type so
/// the validator-set API does not accidentally treat a free balance as
/// stake.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stake(pub Amount);

impl Stake {
    /// Zero stake.
    pub const ZERO: Stake = Stake(0);

    /// Construct from an `Amount`.
    pub const fn new(amount: Amount) -> Self {
        Self(amount)
    }

    /// Raw amount.
    pub const fn amount(self) -> Amount {
        self.0
    }

    /// Add stake. Saturating.
    pub const fn saturating_add(self, other: Stake) -> Stake {
        Stake(self.0.saturating_add(other.0))
    }

    /// Subtract stake. Saturating at zero.
    pub const fn saturating_sub(self, other: Stake) -> Stake {
        Stake(self.0.saturating_sub(other.0))
    }
}

/// A consensus signature. Opaque to this crate; verification is the job of
/// the crate that owns the signing key system (the privacy layer's BLS
/// integration in production, a synthetic Ed25519 stand-in in the devnet).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Signature(pub Bytes32);

impl Signature {
    /// All-zero sentinel. Used in tests and as a placeholder for the
    /// devnet's deterministic signer.
    pub const PLACEHOLDER: Signature = Signature(Bytes32::ZERO);
}

/// The header of a consensus block. The body lives in the DA layer; nodes
/// fetch and verify it through `lace-cons-da`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Parent block this header builds on.
    pub parent: BlockHash,
    /// Slot in which this block was produced.
    pub slot: Slot,
    /// Block height (parent.height + 1).
    pub height: BlockHeight,
    /// Validator that produced the block.
    pub proposer: ValidatorId,
    /// Post-block state commitment.
    pub state_root: StateRoot,
    /// Commitment to the block's data-availability payload.
    pub da_commitment: DaCommitment,
    /// Commitment to the validity proof attesting that the transition from
    /// the parent's state to `state_root` is correct.
    pub proof_commitment: ProofCommitment,
    /// Signature by the proposer over `(parent, slot, height, state_root,
    /// da_commitment, proof_commitment)`. In the devnet this is the
    /// placeholder signer; production wires through to the BLS signer.
    pub signature: Signature,
}

impl BlockHeader {
    /// The block hash. Stand-in: a real implementation hashes the canonical
    /// serialisation. `// TODO(canonical-hash)`: swap to BLAKE2b-256.
    pub fn hash(&self) -> BlockHash {
        // Devnet stand-in: XOR-fold the salient fields into a Bytes32. Good
        // enough for distinctness in tests; not collision-resistant.
        let mut out = [0u8; 32];
        for (i, b) in self.parent.0 .0.iter().enumerate() {
            out[i] ^= *b;
        }
        for (i, b) in self.state_root.0 .0.iter().enumerate() {
            out[i] ^= *b;
        }
        out[0] ^= (self.slot.0 & 0xff) as u8;
        out[1] ^= ((self.slot.0 >> 8) & 0xff) as u8;
        out[2] ^= (self.height & 0xff) as u8;
        out[3] ^= ((self.height >> 8) & 0xff) as u8;
        BlockHash(Bytes32(out))
    }
}

/// A full block: header plus the *commitments* needed to fetch the body
/// from the DA layer. The body bytes themselves are not in this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// The header.
    pub header: BlockHeader,
    /// Identifiers of transactions in this block. The transactions
    /// themselves live in the DA store keyed on
    /// `header.da_commitment`.
    pub tx_ids: Vec<Bytes32>,
}

/// The kind of a consensus vote. Two rounds, BFT-style: pre-vote signals a
/// validator has seen a candidate block; pre-commit binds the validator to
/// finalising it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VoteKind {
    /// Round one: "I have seen this block."
    PreVote,
    /// Round two: "I will not finalise a competing block at this height."
    PreCommit,
}

/// A consensus vote. The fork-choice and finality crates consume streams of
/// these and decide what counts as canonical and what counts as final.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Vote {
    /// Kind: pre-vote or pre-commit.
    pub kind: VoteKind,
    /// Slot the vote covers.
    pub slot: Slot,
    /// Height the vote covers.
    pub height: BlockHeight,
    /// Block being voted for.
    pub target: BlockHash,
    /// Voter.
    pub validator: ValidatorId,
    /// Signature over `(kind, slot, height, target, validator)`.
    pub signature: Signature,
}

/// A justification: the set of pre-commits proving that a block was
/// finalised. The finality gadget produces these; the bridge crate
/// re-verifies them on the destination chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Justification {
    /// Block being justified.
    pub target: BlockHash,
    /// Height of `target`.
    pub height: BlockHeight,
    /// Pre-commits supporting the target. Must come from validators whose
    /// total reputation-weighted stake meets the finality threshold.
    pub precommits: Vec<Vote>,
}

/// Errors common to every consensus crate. Each crate may layer its own
/// error type on top, but anything that needs to cross a boundary uses one
/// of these variants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsensusError {
    /// A signature did not verify.
    BadSignature,
    /// A vote or block referenced a validator not in the active set.
    UnknownValidator,
    /// A vote came in for a slot that has already finalised on a competing
    /// block.
    SlotAlreadyFinal,
    /// A block extends a parent that the node does not know.
    UnknownParent,
    /// A block's height is not `parent.height + 1`.
    HeightMismatch,
    /// A block's slot is not strictly greater than the parent's slot.
    SlotNonMonotonic,
    /// The validity proof did not verify against the state transition.
    BadProof,
    /// The DA commitment did not match the data the sampler retrieved.
    BadDaCommitment,
    /// A slashing predicate fired.
    Slashable,
    /// A bridge packet did not verify against the source chain's
    /// justification.
    BadBridgeProof,
    /// A generic invariant violation. Should never fire; presence
    /// indicates a programming bug, not malicious input.
    Invariant,
}

impl fmt::Display for ConsensusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsensusError::BadSignature => write!(f, "bad signature"),
            ConsensusError::UnknownValidator => write!(f, "unknown validator"),
            ConsensusError::SlotAlreadyFinal => write!(f, "slot already final on competing block"),
            ConsensusError::UnknownParent => write!(f, "unknown parent block"),
            ConsensusError::HeightMismatch => write!(f, "block height mismatch"),
            ConsensusError::SlotNonMonotonic => write!(f, "slot not strictly greater than parent"),
            ConsensusError::BadProof => write!(f, "validity proof did not verify"),
            ConsensusError::BadDaCommitment => write!(f, "DA commitment mismatch"),
            ConsensusError::Slashable => write!(f, "slashable offence"),
            ConsensusError::BadBridgeProof => write!(f, "bridge proof did not verify"),
            ConsensusError::Invariant => write!(f, "invariant violation"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bps_clamps_above_one() {
        assert_eq!(Bps::from_bps(99_999).raw(), 10_000);
    }

    #[test]
    fn bps_apply_floors() {
        assert_eq!(Bps::from_bps(2_500).apply(1_000), 250);
        assert_eq!(Bps::from_bps(1).apply(1_000), 0);
    }

    #[test]
    fn rep_multiplier_neutral_is_identity() {
        assert_eq!(ReputationMultiplier::NEUTRAL.apply(1_000), 1_000);
    }

    #[test]
    fn rep_multiplier_saturates_at_max() {
        let big = ReputationMultiplier::from_bonus_bps(u32::MAX);
        assert_eq!(big.raw_bonus_bps(), ReputationMultiplier::MAX_BONUS_BPS);
    }

    #[test]
    fn rep_multiplier_bonus_math() {
        // 1.5x: base 1000 -> 1500
        let m = ReputationMultiplier::from_bonus_bps(5_000);
        assert_eq!(m.apply(1_000), 1_500);
    }

    #[test]
    fn stake_arithmetic_saturates() {
        let a = Stake::new(100);
        let b = Stake::new(40);
        assert_eq!(a.saturating_add(b).amount(), 140);
        assert_eq!(b.saturating_sub(a).amount(), 0);
    }

    #[test]
    fn slot_and_epoch_increment() {
        assert_eq!(Slot::GENESIS.next(), Slot::new(1));
        assert_eq!(Epoch::GENESIS.next(), Epoch::new(1));
    }

    #[test]
    fn block_hash_distinguishes_slots() {
        let mut header = BlockHeader {
            parent: BlockHash::GENESIS_PARENT,
            slot: Slot::new(1),
            height: 1,
            proposer: ValidatorId::new([1; 32]),
            state_root: StateRoot::new([2; 32]),
            da_commitment: DaCommitment::new([3; 32]),
            proof_commitment: ProofCommitment::new([4; 32]),
            signature: Signature::PLACEHOLDER,
        };
        let h1 = header.hash();
        header.slot = Slot::new(2);
        let h2 = header.hash();
        assert_ne!(h1, h2);
    }
}
