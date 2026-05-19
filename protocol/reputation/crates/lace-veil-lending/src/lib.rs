//! Undercollateralised lending.
//!
//! Loan terms are keyed off the borrower's [`ScoreBand`]:
//!
//! | Band        | LTV (bps) | Liquidation threshold (bps) |
//! |-------------|-----------|------------------------------|
//! | Untrusted   |       — (no credit)                       |
//! | Emerging    |   6_000   | 7_500                        |
//! | Established |   8_000   | 9_000                        |
//! | Trusted     |  10_000   | 11_000                       |
//! | Exemplary   |  12_500   | 13_500                       |
//!
//! LTV above 10_000 bps means the protocol lends out *more* than the
//! posted LACE collateral. The shortfall is backed by the borrower's
//! Veil Score: a default in this regime liquidates the collateral
//! AND triggers a `Slashed` score event so the borrower's score
//! collapses. The disputes path lets a defaulter recover the score
//! over time (it does not erase the slash event).
//!
//! ## Lifecycle
//!
//! ```text
//!   open --> active --> repaid
//!                  \--> liquidating --> liquidated --> recovery
//! ```
//!
//! Missed scheduled payments feed straight back into the score
//! engine (the lending crate emits `PaymentMissed` events into the
//! score sink before any liquidation logic runs), so the score
//! drops *as* the loan deteriorates, not after.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use lace_veil_types::{
    Address, Amount, BlockHeight, BlockSpan, LoanId, ScoreBand, ScoreEvent,
};
use serde::{Deserialize, Serialize};

/// Per-band lending parameters. Indexed by `ScoreBand::index()`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandTerms {
    /// Maximum loan-to-collateral ratio, in bps. 0 means the band is
    /// not eligible to borrow.
    pub max_ltv_bps: [u32; 5],
    /// Liquidation threshold, in bps. When `outstanding * 10_000 /
    /// collateral` reaches this, the loan enters liquidation.
    pub liquidation_threshold_bps: [u32; 5],
}

impl BandTerms {
    /// Launch defaults. Tagged for governance adjustment.
    // TODO(governance): launch committee finalises.
    pub const DEFAULT: BandTerms = BandTerms {
        max_ltv_bps: [0, 6_000, 8_000, 10_000, 12_500],
        liquidation_threshold_bps: [0, 7_500, 9_000, 11_000, 13_500],
    };

    /// Max LTV for a given band.
    pub const fn ltv(self, band: ScoreBand) -> u32 {
        self.max_ltv_bps[band.index()]
    }

    /// Liquidation threshold for a given band.
    pub const fn liquidation(self, band: ScoreBand) -> u32 {
        self.liquidation_threshold_bps[band.index()]
    }
}

/// Lending engine parameters.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LendingParams {
    /// Per-band LTV / liquidation table.
    pub band_terms: BandTerms,
    /// Default loan tenor (blocks). Loans schedule one principal
    /// repayment at `open_block + tenor`.
    pub default_tenor: BlockSpan,
    /// Grace period after the scheduled repayment before the loan is
    /// considered defaulted.
    pub grace_period: BlockSpan,
    /// Interest rate, in bps per `default_tenor`. Flat for v1.
    pub interest_bps_per_tenor: u32,
    /// Recovery window. After a default the borrower has this long
    /// to top up collateral and avoid full liquidation.
    pub recovery_window: BlockSpan,
}

impl LendingParams {
    /// Launch defaults: 30-day tenor (~216k blocks at 12s), 3-day
    /// grace, 250 bps interest per tenor, 7-day recovery.
    // TODO(governance): launch committee finalises.
    pub const DEFAULT: LendingParams = LendingParams {
        band_terms: BandTerms::DEFAULT,
        default_tenor: 216_000,
        grace_period: 21_600,
        interest_bps_per_tenor: 250,
        recovery_window: 50_400,
    };
}

/// State machine for a single loan.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoanStatus {
    /// Active and within terms.
    Active,
    /// Past the grace period without full repayment; recovery window
    /// is running.
    Defaulted,
    /// Liquidation has been executed.
    Liquidated,
    /// Borrower repaid in full (or recovered during the recovery
    /// window).
    Repaid,
}

/// A single loan.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loan {
    /// Stable id.
    pub id: LoanId,
    /// Borrower.
    pub borrower: Address,
    /// Block the loan was opened.
    pub opened_at: BlockHeight,
    /// Scheduled repayment block (`opened_at + tenor`).
    pub due_at: BlockHeight,
    /// Borrower-posted LACE collateral.
    pub collateral: Amount,
    /// Principal disbursed to the borrower.
    pub principal: Amount,
    /// Total still owed: principal + interest - any partial repayment.
    pub outstanding: Amount,
    /// Borrower's score band at origination. The LTV used at open
    /// time is derived from this band; subsequent score drops change
    /// the liquidation calculation through the score engine, not
    /// here.
    pub origination_band: ScoreBand,
    /// Current state.
    pub status: LoanStatus,
    /// Block at which the loan entered Defaulted (0 if never).
    pub defaulted_at: BlockHeight,
    /// Cumulative LACE applied as recovery during the recovery
    /// window.
    pub recovered: Amount,
}

impl Loan {
    /// Loan-to-collateral ratio in bps.
    pub fn ltv_bps(&self) -> u32 {
        if self.collateral == 0 {
            return u32::MAX;
        }
        let r = self.outstanding.saturating_mul(10_000) / self.collateral;
        r.min(u32::MAX as Amount) as u32
    }
}

/// Errors from lending operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LendingError {
    /// The borrower's band is below the credit floor.
    NotEligible,
    /// Requested principal exceeds the band's max LTV against the
    /// posted collateral.
    OverLtv,
    /// Loan id already exists.
    DuplicateId,
    /// Loan not found.
    NotFound,
    /// Operation requires the loan be in a specific status.
    BadStatus,
    /// Collateral / principal must be positive.
    ZeroAmount,
}

/// Lending engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LendingEngine {
    loans: BTreeMap<LoanId, Loan>,
    /// Lending parameters.
    pub params: LendingParams,
}

impl Default for LendingEngine {
    fn default() -> Self {
        Self {
            loans: BTreeMap::new(),
            params: LendingParams::DEFAULT,
        }
    }
}

/// Outcome of a [`LendingEngine::open`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenOutcome {
    /// The freshly-opened loan.
    pub loan: Loan,
}

/// Outcome of a [`LendingEngine::tick`] call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// Score events to feed into the score engine.
    pub events: alloc::vec::Vec<ScoreEvent>,
    /// Loans that transitioned to `Defaulted` on this tick.
    pub newly_defaulted: alloc::vec::Vec<LoanId>,
    /// Loans that transitioned to `Liquidated` on this tick.
    pub newly_liquidated: alloc::vec::Vec<LoanId>,
}

/// Outcome of a [`LendingEngine::repay`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepayOutcome {
    /// Updated loan.
    pub loan: Loan,
    /// LACE returned to the borrower (the collateral, if the loan
    /// is now fully repaid).
    pub returned_collateral: Amount,
    /// Score event emitted on full repayment (a `PaymentMet`).
    pub event: Option<ScoreEvent>,
}

/// Outcome of a [`LendingEngine::liquidate`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidateOutcome {
    /// Loan after liquidation.
    pub loan: Loan,
    /// LACE seized from collateral.
    pub seized: Amount,
    /// Shortfall after seizing all collateral (positive only when
    /// the loan was undercollateralised at default). The stake crate
    /// is expected to slash this from the borrower's reputation
    /// stake.
    pub shortfall: Amount,
    /// `Slashed` event the score engine should ingest, recording
    /// the default.
    pub event: ScoreEvent,
}

impl LendingEngine {
    /// New engine with launch defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a loan. The borrower must be in an eligible band, and
    /// `principal / collateral` must be `<= band.max_ltv`.
    pub fn open(
        &mut self,
        id: LoanId,
        borrower: Address,
        borrower_band: ScoreBand,
        collateral: Amount,
        principal: Amount,
        at: BlockHeight,
    ) -> Result<OpenOutcome, LendingError> {
        if self.loans.contains_key(&id) {
            return Err(LendingError::DuplicateId);
        }
        if collateral == 0 || principal == 0 {
            return Err(LendingError::ZeroAmount);
        }
        let max_ltv = self.params.band_terms.ltv(borrower_band);
        if max_ltv == 0 {
            return Err(LendingError::NotEligible);
        }
        // ltv_bps = principal * 10_000 / collateral.
        let ltv_bps = principal.saturating_mul(10_000) / collateral;
        if ltv_bps > max_ltv as Amount {
            return Err(LendingError::OverLtv);
        }
        let interest = principal.saturating_mul(self.params.interest_bps_per_tenor as Amount) / 10_000;
        let loan = Loan {
            id,
            borrower,
            opened_at: at,
            due_at: at.saturating_add(self.params.default_tenor),
            collateral,
            principal,
            outstanding: principal.saturating_add(interest),
            origination_band: borrower_band,
            status: LoanStatus::Active,
            defaulted_at: 0,
            recovered: 0,
        };
        self.loans.insert(id, loan);
        Ok(OpenOutcome { loan })
    }

    /// Borrow a loan by id.
    pub fn get(&self, id: &LoanId) -> Option<&Loan> {
        self.loans.get(id)
    }

    /// Repay LACE against a loan's outstanding. Full repayment
    /// transitions the loan to `Repaid` and returns the collateral.
    /// A partial repayment during the `Defaulted` recovery window
    /// counts against `recovered` and can pull the loan back to
    /// `Active` if it brings LTV under the band's liquidation
    /// threshold.
    pub fn repay(
        &mut self,
        id: LoanId,
        amount: Amount,
        at: BlockHeight,
    ) -> Result<RepayOutcome, LendingError> {
        let params = self.params;
        let loan = self.loans.get_mut(&id).ok_or(LendingError::NotFound)?;
        if matches!(loan.status, LoanStatus::Liquidated | LoanStatus::Repaid) {
            return Err(LendingError::BadStatus);
        }
        let applied = amount.min(loan.outstanding);
        loan.outstanding = loan.outstanding.saturating_sub(applied);
        if loan.status == LoanStatus::Defaulted {
            loan.recovered = loan.recovered.saturating_add(applied);
            // If the post-repay LTV is back under the band's
            // liquidation threshold, the loan returns to Active.
            let lt = params
                .band_terms
                .liquidation(loan.origination_band) as Amount;
            if loan.outstanding == 0
                || loan.outstanding.saturating_mul(10_000) / loan.collateral <= lt
            {
                loan.status = LoanStatus::Active;
            }
        }
        let mut returned = 0;
        let mut event = None;
        if loan.outstanding == 0 {
            loan.status = LoanStatus::Repaid;
            returned = loan.collateral;
            event = Some(ScoreEvent::PaymentMet {
                subject: loan.borrower,
                at,
            });
        }
        Ok(RepayOutcome {
            loan: *loan,
            returned_collateral: returned,
            event,
        })
    }

    /// Liquidate a loan that has been `Defaulted` and is past its
    /// recovery window. Returns the seized collateral, any
    /// shortfall, and a `Slashed` score event.
    pub fn liquidate(
        &mut self,
        id: LoanId,
        at: BlockHeight,
    ) -> Result<LiquidateOutcome, LendingError> {
        let params = self.params;
        let loan = self.loans.get_mut(&id).ok_or(LendingError::NotFound)?;
        if loan.status != LoanStatus::Defaulted {
            return Err(LendingError::BadStatus);
        }
        if at < loan.defaulted_at.saturating_add(params.recovery_window) {
            return Err(LendingError::BadStatus);
        }
        let seized = loan.collateral;
        loan.collateral = 0;
        let shortfall = loan.outstanding.saturating_sub(seized);
        loan.outstanding = loan.outstanding.saturating_sub(seized);
        let realised_loss = seized + shortfall;
        loan.status = LoanStatus::Liquidated;
        let event = ScoreEvent::Slashed {
            subject: loan.borrower,
            amount: realised_loss,
            at,
        };
        Ok(LiquidateOutcome {
            loan: *loan,
            seized,
            shortfall,
            event,
        })
    }

    /// Advance the engine to block `now`. Transitions active loans
    /// past their grace period to `Defaulted`, emitting a
    /// `PaymentMissed` event so the score drops immediately (before
    /// any liquidation). Transitions defaulted loans past their
    /// recovery window to `Liquidated`-eligible (the actual
    /// liquidation is a separate call so the caller can choose
    /// timing / batching).
    pub fn tick(&mut self, now: BlockHeight) -> TickOutcome {
        let mut out = TickOutcome::default();
        let grace = self.params.grace_period;
        let recovery = self.params.recovery_window;
        for loan in self.loans.values_mut() {
            match loan.status {
                LoanStatus::Active => {
                    let deadline = loan.due_at.saturating_add(grace);
                    if now > deadline && loan.outstanding > 0 {
                        loan.status = LoanStatus::Defaulted;
                        loan.defaulted_at = now;
                        out.newly_defaulted.push(loan.id);
                        out.events.push(ScoreEvent::PaymentMissed {
                            subject: loan.borrower,
                            consecutive: 1,
                            at: now,
                        });
                    }
                }
                LoanStatus::Defaulted => {
                    if now > loan.defaulted_at.saturating_add(recovery) {
                        // Eligible for liquidation -- the caller
                        // executes via `liquidate`. We do not mutate
                        // status here.
                    }
                }
                LoanStatus::Liquidated | LoanStatus::Repaid => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::new([b; 32])
    }
    fn loan_id(b: u8) -> LoanId {
        LoanId::new([b; 32])
    }

    #[test]
    fn untrusted_band_cannot_borrow() {
        let mut e = LendingEngine::new();
        let err = e
            .open(loan_id(1), addr(2), ScoreBand::Untrusted, 1_000, 500, 100)
            .unwrap_err();
        assert_eq!(err, LendingError::NotEligible);
    }

    #[test]
    fn exemplary_band_overcollateralisation_below_125_pct() {
        let mut e = LendingEngine::new();
        let out = e
            .open(loan_id(1), addr(2), ScoreBand::Exemplary, 1_000, 1_250, 100)
            .unwrap();
        assert_eq!(out.loan.principal, 1_250);
        assert_eq!(out.loan.collateral, 1_000);
        assert_eq!(out.loan.status, LoanStatus::Active);
    }

    #[test]
    fn principal_above_band_ltv_rejected() {
        let mut e = LendingEngine::new();
        let err = e
            .open(loan_id(1), addr(2), ScoreBand::Trusted, 1_000, 1_001, 100)
            .unwrap_err();
        assert_eq!(err, LendingError::OverLtv);
    }

    #[test]
    fn opening_loan_records_interest() {
        let mut e = LendingEngine::new();
        let out = e
            .open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100)
            .unwrap();
        // 800 + (800 * 250 / 10_000) = 800 + 20 = 820.
        assert_eq!(out.loan.outstanding, 820);
    }

    #[test]
    fn full_repayment_returns_collateral_and_emits_payment_met() {
        let mut e = LendingEngine::new();
        let opened = e
            .open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100)
            .unwrap();
        let out = e.repay(loan_id(1), opened.loan.outstanding, 200).unwrap();
        assert_eq!(out.loan.status, LoanStatus::Repaid);
        assert_eq!(out.returned_collateral, 1_000);
        match out.event.unwrap() {
            ScoreEvent::PaymentMet { subject, .. } => assert_eq!(subject, addr(2)),
            _ => panic!(),
        }
    }

    #[test]
    fn partial_repayment_keeps_loan_active() {
        let mut e = LendingEngine::new();
        e.open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100).unwrap();
        let out = e.repay(loan_id(1), 100, 200).unwrap();
        assert_eq!(out.loan.status, LoanStatus::Active);
        assert!(out.event.is_none());
    }

    #[test]
    fn tick_past_grace_period_defaults_and_emits_miss() {
        let mut e = LendingEngine::new();
        let opened = e
            .open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100)
            .unwrap();
        let past_grace =
            opened.loan.due_at + LendingParams::DEFAULT.grace_period + 1;
        let out = e.tick(past_grace);
        assert_eq!(out.newly_defaulted, alloc::vec![loan_id(1)]);
        match &out.events[0] {
            ScoreEvent::PaymentMissed { subject, .. } => assert_eq!(*subject, addr(2)),
            _ => panic!(),
        }
        assert_eq!(e.get(&loan_id(1)).unwrap().status, LoanStatus::Defaulted);
    }

    #[test]
    fn liquidate_before_recovery_window_rejected() {
        let mut e = LendingEngine::new();
        let opened = e
            .open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100)
            .unwrap();
        let past_grace = opened.loan.due_at + LendingParams::DEFAULT.grace_period + 1;
        e.tick(past_grace);
        let err = e.liquidate(loan_id(1), past_grace + 1).unwrap_err();
        assert_eq!(err, LendingError::BadStatus);
    }

    #[test]
    fn liquidate_after_recovery_seizes_and_slashes() {
        let mut e = LendingEngine::new();
        let opened = e
            .open(loan_id(1), addr(2), ScoreBand::Exemplary, 1_000, 1_250, 100)
            .unwrap();
        let past_grace = opened.loan.due_at + LendingParams::DEFAULT.grace_period + 1;
        e.tick(past_grace);
        let after_recovery = past_grace + LendingParams::DEFAULT.recovery_window + 1;
        let out = e.liquidate(loan_id(1), after_recovery).unwrap();
        assert_eq!(out.seized, 1_000);
        // Outstanding was 1_250 * 1.025 = 1281; shortfall = 281.
        assert_eq!(out.shortfall, 281);
        assert_eq!(out.loan.status, LoanStatus::Liquidated);
        match out.event {
            ScoreEvent::Slashed { amount, .. } => assert_eq!(amount, 1_000 + 281),
            _ => panic!(),
        }
    }

    #[test]
    fn recovery_path_returns_loan_to_active() {
        let mut e = LendingEngine::new();
        let opened = e
            .open(loan_id(1), addr(2), ScoreBand::Established, 1_000, 800, 100)
            .unwrap();
        let past_grace = opened.loan.due_at + LendingParams::DEFAULT.grace_period + 1;
        e.tick(past_grace);
        assert_eq!(e.get(&loan_id(1)).unwrap().status, LoanStatus::Defaulted);
        // Big repayment knocks LTV well under the liquidation
        // threshold.
        let out = e.repay(loan_id(1), 800, past_grace + 10).unwrap();
        assert_eq!(out.loan.status, LoanStatus::Active);
    }

    #[test]
    fn ltv_bps_reports_outstanding_over_collateral() {
        let l = Loan {
            id: loan_id(1),
            borrower: addr(2),
            opened_at: 0,
            due_at: 100,
            collateral: 1_000,
            principal: 1_000,
            outstanding: 1_250,
            origination_band: ScoreBand::Exemplary,
            status: LoanStatus::Active,
            defaulted_at: 0,
            recovered: 0,
        };
        assert_eq!(l.ltv_bps(), 12_500);
    }
}
