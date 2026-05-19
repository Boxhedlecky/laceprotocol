# Lace Protocol — Privacy Layer

**Component 1 of 5.** This directory contains the ZK privacy layer: the cryptographic foundation that makes every other Lace component private by default.

## Status

Pre-production. Under active design and implementation. Not audited. Not safe for real funds. Targeted for external audit before any mainnet activity.

## What this component owns

The privacy layer is responsible for everything that touches user state confidentiality:

1. **Shielded state model.** UTXO-style note commitments and a global nullifier set. There is no public account balance sheet — balances are derived locally by each wallet from the notes it can decrypt.
2. **Proving system.** Selection, implementation, and recursion of the underlying SNARK used for every shielded transaction. See `SPEC.md` for the proving-system evaluation and choice.
3. **Transaction circuits.** Send, receive, shield (public → private), unshield (private → public), and the generic private-contract-interaction circuit consumed by the VM.
4. **Disclosure keys.** Four scoped, revocable disclosure key types — per-transaction, temporal, threshold, and counterparty — for selective revelation.
5. **Recursive batching.** Aggregation of many shielded transactions into one block-level proof.

## What this component does *not* own

To keep boundaries clean:

- **Consensus / block production** — separate component.
- **The temporal VM and timelock semantics** — separate component. Consumes private-contract-interaction circuits from here.
- **Prediction-market state machine** — separate component. Markets store their state shielded and resolve via this layer's circuits.
- **Veil Score reputation accumulation** — separate component. Reputation is *expressed* via this layer's disclosure keys but *computed* elsewhere.
- **Presale, token contracts, ERC-20 mechanics on Ethereum** — separate concern entirely; lives in the L1-bridge component, not here.

## How the other four components consume this

| Component | What it consumes from privacy layer |
|---|---|
| **Temporal VM (timelocks)** | The private-contract-interaction circuit family. Timelock state lives in shielded notes whose unlock predicate includes a block-height or wall-clock condition checked inside the circuit. |
| **Prediction markets** | Shielded position notes (so a participant's exposure is private) and threshold disclosure keys (so a market resolver can prove "this address staked ≥ X" without revealing the exact stake). |
| **Veil Score reputation** | Counterparty disclosure keys (for mutual reputation attestation between two parties) and temporal disclosure keys (so a lender can verify payment history within a window without seeing the rest). |
| **Consensus / block production** | The recursive proof aggregator. Block producers wrap a block's transaction proofs into one succinct proof verified by all validators. |

## Layout

```
protocol/privacy/
├── README.md            # this file
├── SPEC.md              # full technical spec (proving system, primitives, circuits, keys)
├── INTEGRATION.md       # interfaces and APIs other components depend on
├── crates/              # Rust workspace (added once proving system is chosen)
└── tests/               # cross-crate integration tests
```

## Reading order

1. `SPEC.md` — start here. Explains *what* is being built and *why* the chosen proving system over the alternatives.
2. `INTEGRATION.md` — the contract surface that the other four components depend on. Read this before changing any public API.
3. Per-crate `README`s once the workspace lands.

## Threat model summary

The privacy layer assumes:
- A global passive observer who sees every transaction broadcast and every block.
- An active adversary who may submit malicious transactions, attempt to forge proofs, or attempt to reuse nullifiers.
- An adversary who has compromised the wallet of one specific party and is attempting to learn about unrelated parties.

The privacy layer does *not* defend against:
- Endpoint compromise of the party whose privacy is being protected.
- Side-channel leakage from out-of-band communication (e.g. revealing one's own address on social media).
- Network-layer linkability (Tor / mixnet integration is the responsibility of the node binary, not this component).

See `SPEC.md §10` for the formal threat model.

---

*Building in the open. Issues, PRs, and audit notes welcome.*
