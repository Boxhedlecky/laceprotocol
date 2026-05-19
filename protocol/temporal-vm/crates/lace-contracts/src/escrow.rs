//! Mutual-confirm escrow with abort window and dispute escalation.
//!
//! Lifecycle:
//!
//! 1. `Funding` -- both buyer and seller deposit their stakes.
//! 2. `Active` -- both parties have funded; release / abort / dispute
//!    are reachable from here.
//! 3. `Released` / `Refunded` / `Disputed` -- terminal.
//!
//! Release path: both parties call [`Escrow::confirm`]. As soon as both
//! flags are set the contract enters `Released` and emits a single
//! [`Payout`] of the deposit to the seller's address.
//!
//! Abort path: either party can call [`Escrow::request_abort`] before
//! the `abort_deadline`. If the *other* party also requests abort
//! before the same deadline, the contract enters `Refunded` and both
//! deposits return to their original posters. Crucially, a unilateral
//! abort request alone does **not** refund -- bad-faith aborts are
//! caught by the slashing rules in the disputes crate.
//!
//! Dispute path: either party can call [`Escrow::open_dispute`]. This
//! attaches an oracle reference; settlement is deferred until that
//! oracle resolves. Slashing on dispute losers is also handled by the
//! disputes crate; the escrow itself only knows how to honour the
//! oracle outcome.

use lace_time::{Clock, Timestamp};
use lace_vm::Bytes32;
use serde::{Deserialize, Serialize};

use crate::{Address, Amount, ContractError, Payout, PayoutReason};

/// Escrow state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscrowState {
    /// Awaiting deposits.
    Funding,
    /// Both parties funded; awaiting confirm / abort / dispute.
    Active,
    /// Funds released to seller.
    Released,
    /// Funds returned to both parties.
    Refunded,
    /// Settlement deferred to an oracle outcome.
    Disputed,
}

/// Confirmation flags carried by [`Escrow`]. Split out so the state
/// struct stays readable.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    /// Buyer has signalled `confirm`.
    pub buyer_confirmed: bool,
    /// Seller has signalled `confirm`.
    pub seller_confirmed: bool,
    /// Buyer has signalled `abort`.
    pub buyer_aborted: bool,
    /// Seller has signalled `abort`.
    pub seller_aborted: bool,
    /// Buyer has funded.
    pub buyer_funded: bool,
    /// Seller has funded.
    pub seller_funded: bool,
}

/// Static parameters set at contract creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowConfig {
    /// Buyer's address (the party paying).
    pub buyer: Address,
    /// Seller's address (the party being paid).
    pub seller: Address,
    /// Amount the buyer is depositing.
    pub buyer_deposit: Amount,
    /// Optional seller-side bond. Must be returned on a clean abort.
    pub seller_bond: Amount,
    /// Latest time at which an abort can be requested. After this,
    /// the only ways out are mutual confirm or dispute.
    pub abort_deadline: Timestamp,
}

/// Escrow instance state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escrow {
    /// Static config.
    pub config: EscrowConfig,
    /// Current lifecycle state.
    pub state: EscrowState,
    /// Confirmation / abort / funding flags.
    pub flags: Flags,
    /// Oracle reference set once a dispute is opened.
    pub dispute_oracle: Option<Bytes32>,
    /// Expected outcome that releases the funds to the seller. If
    /// the oracle resolves to anything else, funds refund.
    pub dispute_release_outcome: Option<Bytes32>,
}

impl Escrow {
    /// Construct a new escrow in the `Funding` state.
    pub fn new(config: EscrowConfig) -> Self {
        Self {
            config,
            state: EscrowState::Funding,
            flags: Flags::default(),
            dispute_oracle: None,
            dispute_release_outcome: None,
        }
    }

    /// Mark a party as having deposited their stake.
    pub fn fund(&mut self, party: Address) -> Result<(), ContractError> {
        if self.state != EscrowState::Funding {
            return Err(ContractError::InvalidState("escrow not in Funding"));
        }
        match party {
            p if p == self.config.buyer => self.flags.buyer_funded = true,
            p if p == self.config.seller => {
                // Seller-side bond is optional. We allow `fund` calls
                // from the seller even when `seller_bond` is zero so
                // the caller doesn't have to special-case the no-bond
                // path; this matches "the seller is ready" semantics.
                self.flags.seller_funded = true;
            }
            _ => return Err(ContractError::UnauthorisedParty),
        }
        if self.flags.buyer_funded && self.flags.seller_funded {
            self.state = EscrowState::Active;
        }
        Ok(())
    }

    /// Confirm by a party. When both have confirmed, the contract
    /// transitions to `Released` and emits the release payout.
    pub fn confirm(&mut self, party: Address) -> Result<Vec<Payout>, ContractError> {
        if self.state != EscrowState::Active {
            return Err(ContractError::InvalidState("escrow not Active"));
        }
        if party == self.config.buyer {
            self.flags.buyer_confirmed = true;
        } else if party == self.config.seller {
            self.flags.seller_confirmed = true;
        } else {
            return Err(ContractError::UnauthorisedParty);
        }
        if self.flags.buyer_confirmed && self.flags.seller_confirmed {
            self.state = EscrowState::Released;
            let total = self
                .config
                .buyer_deposit
                .checked_add(self.config.seller_bond)
                .ok_or(ContractError::AmountOverflow)?;
            return Ok(vec![Payout {
                to: self.config.seller,
                amount: total,
                reason: PayoutReason::EscrowRelease,
            }]);
        }
        Ok(vec![])
    }

    /// Request abort. If both parties have requested abort by the
    /// `abort_deadline`, the contract refunds.
    pub fn request_abort(
        &mut self,
        party: Address,
        clock: &dyn Clock,
    ) -> Result<Vec<Payout>, ContractError> {
        if self.state != EscrowState::Active {
            return Err(ContractError::InvalidState("escrow not Active"));
        }
        if clock.now() > self.config.abort_deadline {
            return Err(ContractError::InvalidState("abort window has closed"));
        }
        if party == self.config.buyer {
            self.flags.buyer_aborted = true;
        } else if party == self.config.seller {
            self.flags.seller_aborted = true;
        } else {
            return Err(ContractError::UnauthorisedParty);
        }
        if self.flags.buyer_aborted && self.flags.seller_aborted {
            self.state = EscrowState::Refunded;
            return Ok(vec![
                Payout {
                    to: self.config.buyer,
                    amount: self.config.buyer_deposit,
                    reason: PayoutReason::EscrowRefund,
                },
                Payout {
                    to: self.config.seller,
                    amount: self.config.seller_bond,
                    reason: PayoutReason::EscrowRefund,
                },
            ]);
        }
        Ok(vec![])
    }

    /// Open a dispute. Either party can initiate. After this point
    /// the only transition is [`Escrow::settle_dispute`] when the
    /// oracle resolves.
    pub fn open_dispute(
        &mut self,
        party: Address,
        oracle: Bytes32,
        release_outcome: Bytes32,
    ) -> Result<(), ContractError> {
        if self.state != EscrowState::Active {
            return Err(ContractError::InvalidState("escrow not Active"));
        }
        if party != self.config.buyer && party != self.config.seller {
            return Err(ContractError::UnauthorisedParty);
        }
        self.state = EscrowState::Disputed;
        self.dispute_oracle = Some(oracle);
        self.dispute_release_outcome = Some(release_outcome);
        Ok(())
    }

    /// Settle a dispute from an oracle answer.
    pub fn settle_dispute(&mut self, actual: Bytes32) -> Result<Vec<Payout>, ContractError> {
        if self.state != EscrowState::Disputed {
            return Err(ContractError::InvalidState("escrow not Disputed"));
        }
        let release = self
            .dispute_release_outcome
            .ok_or(ContractError::InvalidState("dispute missing outcome"))?;
        let total = self
            .config
            .buyer_deposit
            .checked_add(self.config.seller_bond)
            .ok_or(ContractError::AmountOverflow)?;
        let to = if actual == release {
            self.config.seller
        } else {
            self.config.buyer
        };
        self.state = EscrowState::Released;
        Ok(vec![Payout {
            to,
            amount: total,
            reason: PayoutReason::EscrowDisputed,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_time::ManualClock;

    fn addr(b: u8) -> Address {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    fn fresh() -> Escrow {
        Escrow::new(EscrowConfig {
            buyer: addr(1),
            seller: addr(2),
            buyer_deposit: 100,
            seller_bond: 10,
            abort_deadline: Timestamp::from_secs(1_000),
        })
    }

    fn activate(e: &mut Escrow) {
        e.fund(addr(1)).unwrap();
        e.fund(addr(2)).unwrap();
        assert_eq!(e.state, EscrowState::Active);
    }

    #[test]
    fn release_requires_both_confirms() {
        let mut e = fresh();
        activate(&mut e);
        assert!(e.confirm(addr(1)).unwrap().is_empty());
        let payouts = e.confirm(addr(2)).unwrap();
        assert_eq!(payouts.len(), 1);
        assert_eq!(payouts[0].to, addr(2));
        assert_eq!(payouts[0].amount, 110);
        assert_eq!(e.state, EscrowState::Released);
    }

    #[test]
    fn unilateral_abort_does_not_refund() {
        let mut e = fresh();
        activate(&mut e);
        let clock = ManualClock::at(Timestamp::from_secs(500));
        assert!(e.request_abort(addr(1), &clock).unwrap().is_empty());
        assert_eq!(e.state, EscrowState::Active);
    }

    #[test]
    fn both_parties_abort_refunds() {
        let mut e = fresh();
        activate(&mut e);
        let clock = ManualClock::at(Timestamp::from_secs(500));
        e.request_abort(addr(1), &clock).unwrap();
        let payouts = e.request_abort(addr(2), &clock).unwrap();
        assert_eq!(payouts.len(), 2);
        assert_eq!(e.state, EscrowState::Refunded);
    }

    #[test]
    fn abort_rejected_after_deadline() {
        let mut e = fresh();
        activate(&mut e);
        let clock = ManualClock::at(Timestamp::from_secs(2_000));
        assert!(matches!(
            e.request_abort(addr(1), &clock).unwrap_err(),
            ContractError::InvalidState(_)
        ));
    }

    #[test]
    fn unauthorised_party_rejected() {
        let mut e = fresh();
        activate(&mut e);
        assert_eq!(
            e.confirm(addr(99)).unwrap_err(),
            ContractError::UnauthorisedParty
        );
    }

    #[test]
    fn dispute_resolves_to_seller_on_release_outcome() {
        let mut e = fresh();
        activate(&mut e);
        let oracle = addr(50);
        let release = addr(51);
        e.open_dispute(addr(1), oracle, release).unwrap();
        let payouts = e.settle_dispute(release).unwrap();
        assert_eq!(payouts[0].to, addr(2));
    }

    #[test]
    fn dispute_resolves_to_buyer_on_other_outcome() {
        let mut e = fresh();
        activate(&mut e);
        let oracle = addr(50);
        let release = addr(51);
        e.open_dispute(addr(2), oracle, release).unwrap();
        let payouts = e.settle_dispute(addr(99)).unwrap();
        assert_eq!(payouts[0].to, addr(1));
    }
}
