# Lace Privacy Layer — Technical Specification

**Status:** Draft. Pre-audit. Subject to change as circuits are implemented and reviewed.

**Version:** 0.1.0-draft

This document specifies the cryptographic design of Lace Protocol's privacy layer. It is the source of truth for the proving-system choice, state model, circuit family, and disclosure key system. Implementation tracks this spec; when implementation diverges, the spec is updated in the same commit.

---

## Table of contents

1. [Goals and non-goals](#1-goals-and-non-goals)
2. [Proving-system evaluation](#2-proving-system-evaluation)
3. [Selected proving system: Halo2-KZG over BN254](#3-selected-proving-system-halo2-kzg-over-bn254)
4. [Note model and shielded state](#4-note-model-and-shielded-state)
5. [Merkle commitment tree](#5-merkle-commitment-tree)
6. [Nullifier scheme](#6-nullifier-scheme)
7. [Transaction circuit family](#7-transaction-circuit-family)
8. [Disclosure key system](#8-disclosure-key-system)
9. [Recursive proof batching](#9-recursive-proof-batching)
10. [Threat model](#10-threat-model)
11. [Open questions](#11-open-questions)

---

## 1. Goals and non-goals

### Goals

- **Privacy by default.** Sender, receiver, amount, and asset type are private for every transaction unless the user explicitly publishes a disclosure key. No opt-in privacy toggle exists; there is no transparent transaction type at the base layer (only `shield` and `unshield` for moving value across the public ↔ private boundary).
- **Scoped, revocable disclosure.** Users can prove specific facts about their history to specific parties without exposing other facts. Disclosure keys are scoped (one key reveals one slice) and revocable (a future state-tree epoch can invalidate them).
- **Throughput via recursion.** A block's worth of shielded transactions verifies in one succinct proof on every node, decoupling verification cost from transaction count.
- **No statistical leakage between transactions.** Two transactions by the same sender are unlinkable to an observer who does not hold a relevant disclosure key.

### Non-goals

- **Post-quantum security.** The proving system is BN254-based (pairing-friendly elliptic curve, ~128-bit classical security). A post-quantum migration is a future hard fork concern, not v1.
- **Anonymity-set hiding from a *global* network observer.** Network-layer privacy (mixnet or onion routing of broadcasts) is the responsibility of the node binary, not this component. This layer guarantees *cryptographic* privacy of state; it does not hide that *some* transaction was broadcast at time T from IP X.
- **MEV resistance.** Out of scope for the privacy layer. The ordering layer handles fairness.

---

## 2. Proving-system evaluation

The proving system is the single most consequential choice in this component. Every shielded transaction generates a proof; every node verifies one (or one aggregated). The choice is effectively irreversible because it propagates into circuit code, key-management, and on-chain (or in-protocol) verifier logic.

The three credible candidates as of 2026 are:

- **Halo2** (PSE fork, KZG commitment scheme, BN254 curve)
- **Plonky3** (Polygon Zero, small-field STARK with optional SNARK wrap)
- **Generic STARK** (Winterfell-style FRI commitment, no wrapping)

### Evaluation criteria

A shielded-UTXO L1 weights criteria differently than, say, a rollup. The five that matter:

| Criterion | Why it matters here |
|---|---|
| **Verifier cost on a Lace validator** | Every full node verifies every block proof. Sets the hardware floor for participating in consensus. |
| **Prover time on consumer hardware** | A shielded transaction is generated client-side. If proving takes 30s on a laptop, UX dies. |
| **Proof size** | Proofs are gossiped across the validator network. Affects bandwidth and DA cost. |
| **Recursion maturity** | We *must* batch many tx proofs into one block proof. The recursion story has to be production-ready, not a research paper. |
| **Audit / production track record in the shielded-UTXO setting** | Privacy bugs are catastrophic and silent. We strongly prefer a stack that has been audited *for this exact pattern* (Zcash Orchard, Penumbra, Aleo, etc.). |

A sixth criterion is **Ethereum verifier gas cost**. This matters only for the L1-bridge component (which lives elsewhere), not for the privacy layer itself. We note it where relevant but do not weight it heavily here.

### Side-by-side

| | Halo2-KZG (BN254) | Plonky3 (small field + FRI) | STARK (large field + FRI) |
|---|---|---|---|
| Proof size | ~1–4 KB | ~80–200 KB (no wrap) | ~50–200 KB |
| Verify time (single proof) | ~5–15 ms | ~5–15 ms | ~10–30 ms |
| Prove time (single shielded transfer) | 2–6 s on a laptop | 0.5–2 s on a laptop | 1–5 s on a laptop |
| Trusted setup | Universal (KZG ceremony, reusable) | None | None |
| Recursion | Accumulation schemes, mature; production in Scroll, Taiko | Native, mature; production in Polygon zkEVM | Possible but expensive; mostly Cairo-specific tooling |
| Ethereum verifier gas (relevant for L1 bridge) | ~250–500 K gas (BN254 precompiles) | Requires Halo2/Groth16 wrap to be feasible (~600K–1M after wrap) | Requires SNARK wrap; prohibitively expensive raw |
| Shielded-UTXO production references | **Zcash Orchard** (the canonical reference), Penumbra | None at production scale for shielded payments | None at production scale for shielded payments |
| Tooling maturity for circuit authoring | PSE halo2, halo2-solidity-verifier, snark-verifier, axiom-eth — excellent | Plonky3 SDK is young; circuit DSL still solidifying | Winterfell + bespoke circuit code; weakest authoring experience |

### Tradeoff narrative

**Plonky3** is genuinely faster on the prover side and avoids any trusted setup, which is attractive. But its production track record in *shielded payments* is zero. Polygon uses it for zkEVM rollups, which is a fundamentally different threat model (rollups care about correctness, not privacy of state). Adopting Plonky3 means pioneering both the proving stack *and* the privacy circuits at the same time. For an L1 holding user funds, that's two unknowns multiplied.

**Generic STARKs** are knocked out by proof size. Shielding a single payment with a 100 KB proof gossiped across the validator set is bandwidth-prohibitive at any meaningful TPS. Post-quantum security is real but not urgent enough to pay this price.

**Halo2-KZG** wins on the criterion that matters most for this specific application: **Zcash Orchard runs in production today using exactly this stack, defending exactly this threat model (shielded UTXOs with nullifiers and selective disclosure).** Audit reports from NCC Group and others are public. The PSE fork is actively maintained, the snark-verifier ecosystem is rich, and recursion via accumulation (or via a final Plonk wrap) is well understood.

The trusted-setup objection is real but mitigated: we reuse the existing KZG/Perpetual-Powers-of-Tau ceremony (multi-thousand-participant, well-publicized). No new ceremony required.

### Decision

**Halo2 with KZG commitment over the BN254 curve.** Rationale: maturity-in-context dominates raw performance here. We choose the stack that has been audited for shielded payments, accepting a 3–5× prover-time penalty vs. Plonky3 in exchange for an existing safety track record.

This decision will be revisited if (a) Plonky3 accumulates a comparable shielded-payment audit history, or (b) a post-quantum migration becomes urgent. Both are multi-year horizons.

---

## 3. Selected proving system: Halo2-KZG over BN254

### 3.1 Concrete parameters

- **Curve:** BN254 (a.k.a. BN128, alt_bn128) — pairing-friendly, has Ethereum precompiles.
- **Scalar field** `Fr`: 254-bit prime. All circuit arithmetic operates over `Fr`.
- **Commitment scheme:** KZG10 with the Perpetual Powers of Tau (`pse-trusted-setup` SRS, ≥ 2^21 degree).
- **Arithmetization:** Plonkish (Halo2's UltraPLONK-style with custom gates and lookup arguments).
- **Hash inside circuits:** **Poseidon2** over `Fr` (8 full + 56 partial rounds, sponge construction with rate=2 capacity=1). Poseidon2 chosen over Poseidon for ~2× constraint reduction with no known security regression as of 2026.
- **Symmetric encryption (out of circuit):** ChaCha20-Poly1305 with 256-bit keys.
- **Key-agreement:** X25519. (We do *not* perform the DH in-circuit; it produces an output that is decrypted client-side.)
- **Signature scheme:** Schnorr over JubJub (embedded curve over `Fr`), verified in-circuit when authorization is required.

### 3.2 Field choice rationale

BN254 scalar field is large (254 bits), which means more constraints per arithmetic op than a small-field system. We accept this because:
- BN254 has Ethereum pairing precompiles, which keeps the L1-bridge component's options open.
- Mature tooling for Poseidon2 / JubJub / KZG is BN254-native.
- Recursive verifier circuits are well-studied for this curve (Plonk-style accumulation).

### 3.3 Crate selection

We will depend on the PSE-maintained Halo2 fork (`halo2_proofs` from `privacy-scaling-explorations/halo2`), pinned by commit hash. We will not vendor a private fork; upstream contributions for any fixes we need.

Additional ecosystem crates:
- `snark-verifier` — for the recursive verifier circuit.
- `halo2curves` — BN254 + JubJub primitives.
- `poseidon2` — reference Poseidon2 implementation (audited variant).
- `ff` and `group` — field/group traits.

---

## 4. Note model and shielded state

The state of the chain is the set of *unspent notes*. There is no account model and no public balance sheet. A user's balance is the sum of amounts in notes they can decrypt.

### 4.1 Note structure

A `Note` is a 5-tuple:

```
Note = (addr, asset_id, value, rho, psi)
```

| Field | Size | Description |
|---|---|---|
| `addr` | 32 B | Recipient's diversified shielded address (point on JubJub). |
| `asset_id` | 32 B | Asset identifier (e.g. native LACE, or a tokenized asset). |
| `value` | 8 B | Note value, `u64`. |
| `rho` | 32 B | Nullifier seed, sampled uniformly at note creation. |
| `psi` | 32 B | Note randomness for commitment binding. |

### 4.2 Note commitment

The on-chain representation of a note is its commitment `cm`:

```
cm = Poseidon2(addr_x, addr_y, asset_id, value, rho, psi)
```

Where `addr_x, addr_y` are the affine coordinates of the recipient's address point.

Properties:
- **Hiding** — given `cm`, an observer learns nothing about the underlying fields (Poseidon2 is a CRH; we rely on `psi` providing entropy).
- **Binding** — finding two distinct notes hashing to the same `cm` reduces to a Poseidon2 collision.

### 4.3 Note encryption (out of circuit)

For the recipient to spend a note, they must learn its fields. The sender encrypts the note to the recipient's address:

```
shared_secret = X25519(sender_eph_sk, recipient_pk)
key           = HKDF-SHA256(shared_secret, info="lace/note-v1")
ciphertext    = ChaCha20-Poly1305(key, nonce=0, plaintext=encode(Note))
```

The transaction broadcasts `(ciphertext, sender_eph_pk)`. The recipient trial-decrypts every new transaction's ciphertext with their incoming viewing key. Decryption is a constant ~100 µs per attempt.

We do **not** perform note decryption in-circuit. The sender publishes the encrypted payload alongside the transaction; the recipient decrypts client-side. The circuit only proves that `cm` correctly commits to a note whose value and address are consistent with the rest of the transaction.

### 4.4 Addresses

A user has:
- **Spending key** `sk` — never leaves the wallet.
- **Full viewing key** `fvk = derive(sk)` — can decrypt incoming notes and reconstruct nullifiers (for self-audit).
- **Incoming viewing key** `ivk` — can decrypt incoming notes only, cannot derive nullifiers.
- **Outgoing viewing key** `ovk` — can decrypt the sender-side note metadata of *outgoing* transactions made by the same wallet.
- **Diversified address** `addr_d = ivk · G_d` for a diversifier `d ∈ {0,1}^88`. A user has 2^88 distinct addresses sharing one `ivk`.

Diversifiers solve the "every transaction looks like it goes to the same address" problem without breaking note decryption.

---

## 5. Merkle commitment tree

Commitments are inserted into a single append-only Merkle tree. Tree state is consensus state.

### 5.1 Parameters

- **Hash:** Poseidon2 over `Fr` (same instance as note commitment).
- **Depth:** 32 (i.e. up to 2^32 ≈ 4.3 billion notes).
- **Arity:** binary.
- **Update model:** incremental, with rolling subtree caches. New leaves append to the next free position; interior hashes recomputed lazily.

### 5.2 Root anchors

Every block, the new tree root is committed to the chain header as the **anchor** for that block. Transactions reference an anchor (a recent root) when proving their input note is in the tree. We accept any anchor from the most recent 100 blocks (roughly 5–10 minutes wall-clock), which gives wallets a long-enough window to construct a proof without making the validator state grow unboundedly.

Anchors older than 100 blocks are pruned from the active set. Their commitments remain in the tree (it is append-only); only the *acceptable anchor set* shrinks.

### 5.3 Insertion proof inside the circuit

A spend circuit proves *membership* of an input note's commitment in some accepted anchor:

```
witness:   cm, path[32], path_indices[32]
public:    anchor
constraint: MerkleVerify(cm, path, path_indices) == anchor
```

`MerkleVerify` is 32 Poseidon2 invocations. Cost: dominant component of a spend proof (~70% of constraints).

---

## 6. Nullifier scheme

Each note must be spendable exactly once. The mechanism is a *nullifier*: a deterministic, unlinkable per-note tag the sender publishes when spending.

### 6.1 Construction

```
nf = Poseidon2(nk, rho)
```

Where:
- `nk` = nullifier key, derived from the owner's spending key: `nk = Poseidon2(sk || "nk-derivation")`.
- `rho` = the nullifier seed inside the note.

The chain maintains a global **nullifier set** `NS`. A transaction that publishes a nullifier already in `NS` is invalid. A valid spend inserts its nullifier into `NS`.

### 6.2 Properties

- **Unlinkable to commitment.** Given `cm` and `nf` for the same note, an observer without `nk` cannot link them. (`Poseidon2` is one-way; the inputs `(addr, asset_id, value, psi)` in `cm` and `(nk, rho)` in `nf` are disjoint except for `rho`, which is hidden inside `cm`.)
- **Deterministic per note.** A given note has exactly one valid nullifier, so a malicious owner cannot spend twice by publishing different nullifiers.
- **Collision resistance.** Two distinct notes producing the same `nf` requires either a Poseidon2 collision or `nk1=nk2 ∧ rho1=rho2`, both of which are infeasible.

### 6.3 Nullifier set storage

`NS` is a sparse Merkle tree of depth 256 over Poseidon2, with leaves keyed by `nf` and values in `{absent, present}`. A non-membership proof for `nf` is checked in-circuit at spend time and a membership update is performed at apply time.

We considered a simple key-value set (cheaper apply, no non-membership proof needed because we can check directly). The SMT is chosen because it admits **light-client verification** of nullifier state, which the bridge component will need.

---

## 7. Transaction circuit family

A transaction proves the existence of a valid state transition. Five circuit families cover all transaction types.

### 7.1 `Spend` circuit (input note → nullifier)

**Public inputs:** anchor, nf, value_commitment_in
**Private witness:** Note, path, path_indices, sk, value_blinding_in

**Constraints:**
1. `cm = Poseidon2(addr.x, addr.y, asset_id, value, rho, psi)`
2. `MerkleVerify(cm, path, path_indices) == anchor`
3. `nk = Poseidon2(sk || "nk")`
4. `addr` is consistent with `sk` (Schnorr key check)
5. `nf = Poseidon2(nk, rho)`
6. `value_commitment_in = Pedersen(value, asset_id, value_blinding_in)` — a homomorphic Pedersen commitment used to balance amounts across the transaction without revealing them.

### 7.2 `Output` circuit (new shielded note)

**Public inputs:** cm, value_commitment_out, encrypted_note_ciphertext_hash
**Private witness:** Note, value_blinding_out

**Constraints:**
1. `cm = Poseidon2(addr.x, addr.y, asset_id, value, rho, psi)`
2. `value_commitment_out = Pedersen(value, asset_id, value_blinding_out)`
3. The ciphertext hash binds the encrypted note to this output (so the relayer cannot strip it).

### 7.3 `Send` (composite)

A `Send` transaction is composed of:
- 1..N `Spend` proofs (the inputs).
- 1..M `Output` proofs (the outputs, including any change-back-to-self).
- A **balance proof**: the sum of input Pedersen commitments equals the sum of output Pedersen commitments plus the public fee. Pedersen commitments are additively homomorphic, so this is verified outside the SNARK with a single elliptic-curve equation.

### 7.4 `Shield` (public → private)

Moves value from a transparent balance into a shielded note.

**Public inputs:** public_amount, public_sender, cm, value_commitment_out
**Private witness:** Note, value_blinding_out

**Constraints:**
1. The `Output` circuit's constraints.
2. `value == public_amount` (so the on-chain Pedersen check ties to the public amount).

### 7.5 `Unshield` (private → public)

Moves value from a shielded note into a transparent balance.

**Public inputs:** anchor, nf, public_amount, public_recipient
**Private witness:** Note, path, path_indices, sk

**Constraints:**
1. The `Spend` circuit's constraints.
2. `value == public_amount`.

### 7.6 `PrivateInvoke` (private contract interaction)

Generic circuit for contract calls. The contract VM emits a circuit-fragment per contract, which is composed with `Spend`/`Output` to form a transaction proving:
- Some shielded state (note(s)) transitioned according to contract logic.
- The transition consumes specific input nullifiers and produces specific output commitments.

This is the integration surface for the temporal VM and the prediction-market state machine. Spec for the contract-fragment ABI lives in `INTEGRATION.md`.

---

## 8. Disclosure key system

Selective disclosure is what makes shielded state *useful* outside of the wallet that owns it. We expose four key types. Each is **scoped** (one key reveals one slice of history), **revocable** (via epoch rotation), and **cryptographically independent** (compromise of one does not enable forging another).

### 8.1 Common machinery

Each disclosure key carries:
- A **scope predicate** `P` (what it covers)
- A **verifier proof** `π` (the user's commitment that `P` is honest)
- An **epoch** `e` (revocability handle)
- A **recipient binding** `recipient_pk` (so a stolen key is useless to anyone else)

A disclosure key is a Halo2 proof of the predicate, encrypted to `recipient_pk` with the same X25519 / ChaCha20-Poly1305 scheme used for notes. Recipient decrypts, then verifies the proof.

Revocation: each address publishes a small **disclosure-epoch root** in the chain state. Every disclosure proof references an epoch. The owner can rotate the epoch root, invalidating all previously-issued keys for that address. A receiver checks `epoch == current_epoch` as part of verification.

### 8.2 Per-transaction key

**Predicate:** "Transaction `tx_id` is mine and its plaintext is `T`."

**Construction:** The key is a proof that:
1. `tx_id` appears in some block referenced by an anchor.
2. The note ciphertexts in `tx_id` decrypt under the holder's `ivk`/`ovk` to plaintext notes matching the disclosed `T`.

Verifier learns `T` (sender, receiver, amount, asset, memo) for exactly this one transaction.

### 8.3 Temporal key

**Predicate:** "All transactions of mine between block heights `h_lo` and `h_hi` are exactly the set `S`."

**Construction:** A proof that for every block in `[h_lo, h_hi]`:
- The owner's complete set of incoming and outgoing transactions in that block is `S ∩ block`.
- No other transaction in that block was owned by this address.

This is the most expensive disclosure type — the proof grows linearly in `(h_hi - h_lo)`. We bound the range to ≤ 100,000 blocks per key (≈ 1 week wall-clock at 6-second blocks) and recommend wallets generate multiple temporal keys for longer audits.

Verifier learns: the full set of transactions in the window. Verifier learns nothing about transactions outside the window.

### 8.4 Threshold key

**Predicate:** "My balance (sum of unspent note values for asset `a` at anchor `A`) is `≥ V`" (or `≤ V`).

**Construction:** A proof that the owner can produce a set of notes `{N_i}` such that:
1. Every `N_i` is unspent at anchor `A` (membership in tree, non-membership in `NS`).
2. Every `N_i.addr` derives from the owner's `sk`.
3. `Σ N_i.value ≥ V` (range proof on the sum).

The verifier learns: a boolean, plus the anchor and asset. The verifier learns nothing about which notes were used or the exact balance.

A wallet may also issue a **negative threshold key** ("balance ≤ V"). This is harder to make sound — the prover must show that *no* additional unspent note exists. We achieve this by binding the proof to the holder's complete `fvk` and proving the enumeration is exhaustive, which requires the prover to disclose their `fvk` to the verifier as part of the key (since the verifier must independently scan the tree).

Negative threshold keys therefore leak the `fvk` to the verifier. We surface this UX-loudly. (An exact-balance disclosure is implemented as a positive + negative threshold key on the same `V`.)

### 8.5 Counterparty key

**Predicate:** "Between me (address `A`) and counterparty (address `B`), the complete set of mutual transactions in the range `[h_lo, h_hi]` is `S`. No transactions between `A` and `B` exist outside `S`."

**Construction:** Requires *both* parties to participate. Each party signs a key that:
1. Lists every transaction in `S` (in chronological order with amounts and asset IDs).
2. Proves under their own `fvk` that no other transactions exist between `A` and `B` in the window.
3. Is binding by including a Schnorr signature from both `sk_A` and `sk_B` over the key contents.

A single party cannot forge a counterparty key — the counterparty's signature is required. This makes counterparty keys uniquely useful for dispute resolution: a party trying to claim "X never paid me" cannot get a counterparty key disagreeing without X's cooperation.

### 8.6 Cryptographic independence

Compromise analysis:

| Compromised | Reveals |
|---|---|
| Per-tx key for `tx_i` | Only `tx_i`. Other transactions remain hidden. |
| Temporal key for `[h_lo, h_hi]` | Only transactions in that window. Other windows safe. |
| Threshold key (positive) for `V` at anchor `A` | Only the predicate `balance ≥ V`. Exact balance, note set, and other anchors remain hidden. |
| Threshold key (negative) for `V` at anchor `A` | The predicate **and** the owner's `fvk`. **All historical incoming transactions are now visible to the verifier.** This is documented and surfaced to users. |
| Counterparty key with `B` | Only A↔B transactions in the window. A's transactions with C remain hidden. |

The negative-threshold-key leakage is the one sharp edge in this design. We accept it because the use case (proving "I cannot pay this debt") is rare and the alternative (a non-`fvk`-revealing proof of exhaustive enumeration) is an open research problem.

---

## 9. Recursive proof batching

Per-transaction proofs are ~1–4 KB each. A block with 1000 transactions would carry ~4 MB of proofs and require 1000 verifier invocations per node. We batch via recursion.

### 9.1 Approach: tree of accumulation

We use the `snark-verifier` accumulation scheme. A *block proof* is a Halo2 proof whose statement is:

> "There exist 1024 transaction proofs `π_1 ... π_1024`, each of which verifies under the appropriate per-circuit verifying key, and whose public inputs are consistent with this block's state transition."

Construction:
1. The block producer collects ≤ 1024 transactions.
2. Pairs of transaction proofs are accumulated into a single intermediate proof (10 levels of binary tree).
3. The root accumulator is the block proof.

A non-full block (< 1024 tx) pads with no-op proofs.

### 9.2 Verifier cost

Block-proof verification is one Halo2 verification: ~15ms on a modern CPU. Independent of `n`.

### 9.3 Prover cost

Block proving is the bottleneck. With current Halo2 + snark-verifier on a 64-core machine, ~5–10 seconds for a 1024-tx block. We expect block-producer-class hardware to handle this. Solo validators will eventually rely on out-of-protocol proof markets.

---

## 10. Threat model

### 10.1 Adversary capabilities

We assume an adversary who:
- Observes the entire chain history and every gossip message.
- Submits arbitrary transactions of their choice.
- Controls some fraction of validators (consensus-layer concern; the privacy layer is honest-validator-agnostic).
- May have compromised the wallet of one or more *specific* parties whose privacy is *not* being claimed.

We do **not** assume:
- The adversary can break BN254 discrete log, JubJub discrete log, X25519, ChaCha20-Poly1305, or Poseidon2 with feasible work.
- The adversary can compromise the KZG ceremony (we rely on the existing multi-thousand-participant ceremony).
- The adversary has out-of-band access to the protected party's secret keys.

### 10.2 Privacy claims

Under the assumptions above, an adversary not in possession of any of the protected party's disclosure keys learns nothing beyond:
- That *some* transaction was broadcast at time T (network-layer; not our concern).
- The shape of the transaction (number of inputs, number of outputs, fee). This is intentional — fee and input/output count cannot be hidden cheaply.

In particular, the adversary cannot learn: sender, receiver, amount, asset, memo, balance, or transaction linkage.

### 10.3 Known limitations

- **Transaction shape leakage.** A "1-in, 1-out" transaction is distinguishable from "2-in, 5-out". Sophisticated users wishing to defeat this can pad inputs/outputs with self-spend dummies; the protocol does not enforce this.
- **Timing correlation.** An observer who knows party A is offline from 02:00 to 06:00 can rule out A as the sender of any transaction in that window. Mitigated by wallet-level batching and delay (UX trade-off).
- **Negative threshold key fvk leakage** (see §8.5).
- **Anchor-window correlation.** Transactions referencing the same anchor were constructed within a ~10-minute window. This leaks a coarse timing signal. Considered acceptable.

---

## 11. Open questions

These are explicitly unresolved and will be settled before audit:

1. **Poseidon2 vs. Reinforced Concrete (RC).** RC has better constraint counts but a shorter cryptanalysis history. We default to Poseidon2 unless RC's track record meaningfully matures before we lock circuits.
2. **JubJub vs. Bandersnatch as the embedded curve.** Bandersnatch has faster scalar mult inside Halo2 circuits but smaller cryptanalysis surface than JubJub. Track record matters more here than 20% constraint reduction.
3. **Whether to commit ciphertexts to the tree.** Currently we hash ciphertexts only into the transaction body. Committing them into the note tree gives stronger non-equivocation but doubles tree-write cost.
4. **Length of the acceptable-anchor window.** 100 blocks is a UX/storage trade-off. May shrink to 32 or grow to 256 based on real wallet behavior.
5. **Negative threshold keys.** Can we eliminate the `fvk` leakage with a proof of exhaustive enumeration that doesn't require the verifier to scan the tree themselves? Open research.
6. **Quantum migration path.** Not v1, but the v1 design should not foreclose moving to a STARK or lattice-based scheme in v2. Audit this assumption.

---

*Last updated: 2026-05-19*
