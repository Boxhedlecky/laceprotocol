# API Reference

Consolidated index of the public API across the seven crates. For full doc-comment text see `cargo doc --workspace --no-deps`.

---

## `lace-veil-types`

### Types

| Item | Purpose |
|------|---------|
| `Bytes32` | 32-byte opaque identifier. |
| `Address` | Newtype around `Bytes32` for protocol participants. |
| `Amount` (alias for `u128`) | LACE quantity. |
| `BlockHeight` (alias for `u64`) | Absolute block height. |
| `BlockSpan` (alias for `u64`) | Block duration. |
| `Bps` | Basis-point scalar (0..=10_000). |
| `Score` | Veil Score in bps. |
| `ScoreBand` | Five-band partition of `Score`. |
| `ScoreCommitment` | Hiding commitment to a score witness. |
| `LoanId`, `AttestationId` | Identifier newtypes. |
| `ScoreEvent` | Unified event envelope ingested by the score engine. |

### Notable methods

- `Score::from_bps(u32) -> Score`
- `Score::band() -> ScoreBand`
- `ScoreBand::for_score(Score) -> ScoreBand`
- `ScoreBand::index() -> usize` (0..=4)
- `ScoreBand::ALL: [ScoreBand; 5]`
- `Bps::apply(amount) -> Amount` — `amount * self / 10_000` saturating.

---

## `lace-veil-score`

### Types

- `VeilEngine` — the score state machine. Holds per-address accumulators.
- `AddressState` — per-address record.
- `ScoreWeights` — four-component blending weights (must sum to 10_000).
- `DecayParams` — drift-toward-neutral parameters.
- `SaturationParams` — tenure / payment / calibration asymptotes.

### Methods

```rust
impl VeilEngine {
    pub fn new(weights: ScoreWeights, decay: DecayParams, saturation: SaturationParams) -> Self;
    pub fn knows(&self, a: &Address) -> bool;
    pub fn state_of(&self, a: &Address) -> Option<&AddressState>;
    pub fn score_of(&self, a: &Address) -> Score;
    pub fn ingest(&mut self, event: ScoreEvent) -> Score;
    pub fn states(&self) -> &BTreeMap<Address, AddressState>;
}
```

### Constants

- `ScoreWeights::DEFAULT` — `40 / 25 / 20 / 15`.
- `DecayParams::DEFAULT` — 50 bps drift / ~1 week.
- `SaturationParams::DEFAULT` — 1y tenure asymptote, 50-payment payment asymptote.

---

## `lace-veil-proofs`

### Types

- `Statement` — `Threshold | ZeroDefaults | CalibrationBand | Tenure`.
- `Witness` — private inputs (score, calibration, first_seen, last_missed_at, blinding).
- `Proof` — opaque proof artefact.
- `VerifyError` — `CommitmentMismatch | PredicateFailed | MalformedStatement`.

### Functions

```rust
pub fn commit(w: &Witness) -> ScoreCommitment;
pub fn prove(statement: &Statement, witness: &Witness) -> Result<Proof, VerifyError>;
pub fn verify(statement: &Statement, proof: &Proof) -> Result<(), VerifyError>;
```

### Method conveniences

- `Statement::subject() -> Address`
- `Statement::commitment() -> ScoreCommitment`

---

## `lace-veil-attest`

### Types

- `AttestGraph` — the attestation state.
- `Attestation`, `AttesterLedger` — records.
- `AttestParams` — sybil curve, budget, decay, slash.
- `AttestError` — `DuplicateId | SelfAttestation | InvalidWeight | BudgetExceeded | NotFound`.
- `AttestOutcome` — carries the `ScoreEvent`s to feed into the score engine.

### Methods

```rust
impl AttestGraph {
    pub fn new() -> Self;
    pub fn post(&mut self, id, subject, attester, raw_weight_bps, attester_band, params, at)
        -> Result<AttestOutcome, AttestError>;
    pub fn revoke(&mut self, id, at) -> Result<AttestOutcome, AttestError>;
    pub fn settle_dispute_bad_faith(&mut self, id, params, at) -> Result<AttestOutcome, AttestError>;
    pub fn tick_decay(&mut self, now, params) -> AttestOutcome;
    pub fn get(&self, id: &AttestationId) -> Option<&Attestation>;
    pub fn ledger(&self, a: &Address) -> Option<&AttesterLedger>;
}
pub fn derive_id(subject: Address, attester: Address, posted_at: BlockHeight) -> AttestationId;
```

### Constants

- `AttestParams::DEFAULT` — sybil curve `[5, 25, 50, 85, 100]` %, budget 50_000 bps, decay 1y, bad-faith slash 50 %.

---

## `lace-veil-stake`

### Types

- `StakeEngine` — stake / slash / reward state machine.
- `StakePosition`, `SlashOutcome`, `SlashDistribution`.
- `StakeParams`, `SlashRouting`.
- `StakeError` — `BelowMinimum | InsufficientStake | CooldownActive | NothingCooling`.

### Methods

```rust
impl StakeEngine {
    pub fn new() -> Self;
    pub fn with_params(params: StakeParams) -> Self;
    pub fn position_of(&self, a: &Address) -> Option<&StakePosition>;
    pub fn stake(&mut self, subject, amount, at) -> Result<(), StakeError>;
    pub fn request_unstake(&mut self, subject, amount, at) -> Result<(), StakeError>;
    pub fn withdraw(&mut self, subject, at) -> Result<Amount, StakeError>;
    pub fn slash(&mut self, subject, counterparty, amount, at) -> SlashOutcome;
    pub fn settle_reward(&mut self, subject, at) -> Amount;
}
```

### Constants

- `SlashRouting::PROTOCOL` — **60 / 25 / 15** (counterparty / burn / ecosystem). Protocol-fixed.
- `StakeParams::DEFAULT` — min 100 LACE, cooldown 14d, reward 50 bps / 30d.

---

## `lace-veil-lending`

### Types

- `LendingEngine`, `Loan`, `LoanStatus`.
- `LendingParams`, `BandTerms`.
- `LendingError` — `NotEligible | OverLtv | DuplicateId | NotFound | BadStatus | ZeroAmount`.
- `OpenOutcome`, `RepayOutcome`, `LiquidateOutcome`, `TickOutcome`.

### Methods

```rust
impl LendingEngine {
    pub fn new() -> Self;
    pub fn open(&mut self, id, borrower, band, collateral, principal, at)
        -> Result<OpenOutcome, LendingError>;
    pub fn get(&self, id: &LoanId) -> Option<&Loan>;
    pub fn repay(&mut self, id, amount, at) -> Result<RepayOutcome, LendingError>;
    pub fn liquidate(&mut self, id, at) -> Result<LiquidateOutcome, LendingError>;
    pub fn tick(&mut self, now) -> TickOutcome;
}
```

### Constants

- `BandTerms::DEFAULT` — `[0, 60, 80, 100, 125]` % LTV; `[0, 75, 90, 110, 135]` % liquidation.
- `LendingParams::DEFAULT` — 30d tenor, 3d grace, 250 bps interest, 7d recovery.

---

## `lace-veil-governance`

### Types

- `GovernanceParams`, `ScoreBandMultipliers`.
- `Vote`, `Tally`.

### Functions

```rust
pub fn vote_weight(stake: Amount, band: ScoreBand, params: GovernanceParams) -> Amount;
pub fn tally_votes(votes: &[Vote], params: GovernanceParams) -> Tally;
pub fn sorted_by_weight(votes: &[Vote], params: GovernanceParams) -> Vec<(Address, Amount)>;
```

### Constants

- `ScoreBandMultipliers::DEFAULT` — `[0.5x, 0.75x, 1.0x, 1.5x, 2.0x]`.
- `GovernanceParams::DEFAULT` — min voting stake 100 LACE.

---

## Common patterns

### Bootstrapping the score for a new wallet

```rust
let mut engine = VeilEngine::default();
engine.ingest(ScoreEvent::FirstSeen { subject, at });
```

### Reading a band for a downstream consumer

```rust
let band = engine.score_of(&subject).band();
```

### Producing a threshold proof

```rust
let blinding = sample_blinding();  // 32-byte randomness
let witness  = Witness::new(score_bps, calibration_bps, first_seen, last_missed_at, blinding);
let commitment = commit(&witness);

let stmt = Statement::Threshold { subject, commitment, threshold_bps: 6_000 };
let proof = prove(&stmt, &witness)?;

// Consumer side:
verify(&stmt, &proof)?;
```

### Opening, liquidating, and slashing a defaulting loan

```rust
let open = lending.open(loan_id, borrower, ScoreBand::Exemplary, 1_000, 1_250, now)?;

// Time passes, borrower misses repayment...
let tick = lending.tick(later);
for event in tick.events {
    engine.ingest(event);   // PaymentMissed -> score drops
}

// After recovery window:
let liq = lending.liquidate(loan_id, even_later)?;
engine.ingest(liq.event);   // Slashed -> default recorded

stake.slash(borrower, lender, liq.shortfall, even_later);
```
