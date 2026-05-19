//! Recurring payment contract.
//!
//! Models salary, subscription, and loan-repayment flows. Time-driven:
//! the payer pre-funds a balance, and the chain deterministically
//! emits a tick payment at every scheduled interval inside the active
//! window, draining the balance one tick at a time.
//!
//! The "missed-payment" path is not a separate state. If the balance
//! cannot cover a tick, the tick is recorded as `missed`, the
//! contract's `consecutive_missed` counter advances, and the payment
//! is *not* silently skipped: the disputes crate consumes the missed
//! count when computing reputation slashing. This is deliberately
//! conservative -- a salary contract that quietly forgets unpaid
//! months would be worse than one that records the miss and lets the
//! recipient escalate.

use lace_time::{Clock, Duration, Interval, Timestamp};
use lace_vm::Schedule;
use serde::{Deserialize, Serialize};

use crate::{Address, Amount, ContractError, Payout, PayoutReason};

/// Recurring payment configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecurringConfig {
    /// Address funding the stream.
    pub payer: Address,
    /// Address receiving the stream.
    pub payee: Address,
    /// Amount paid out per tick.
    pub amount_per_tick: Amount,
    /// Time between ticks.
    pub interval: Duration,
    /// Start (inclusive) and end (exclusive) of the active window.
    pub window: Interval,
}

/// Recurring payment runtime state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecurringPayment {
    /// Static config.
    pub config: RecurringConfig,
    /// Funds available to pay out.
    pub balance: Amount,
    /// Ticks already paid.
    pub ticks_paid: u64,
    /// Ticks that fired but had no balance to pay.
    pub ticks_missed: u64,
    /// Consecutive misses since the last successful tick. Reset on
    /// any successful tick. Consumed by the disputes layer.
    pub consecutive_missed: u64,
    /// `Some(ts)` while paused; `None` while running. While paused,
    /// `advance` does not generate any ticks.
    pub paused_at: Option<Timestamp>,
}

impl RecurringPayment {
    /// Construct, validating the schedule.
    pub fn new(config: RecurringConfig) -> Result<Self, ContractError> {
        let schedule = Schedule {
            interval: config.interval,
            window: config.window,
        };
        schedule
            .validate()
            .map_err(|_| ContractError::BadConfig("invalid recurring schedule"))?;
        Ok(Self {
            config,
            balance: 0,
            ticks_paid: 0,
            ticks_missed: 0,
            consecutive_missed: 0,
            paused_at: None,
        })
    }

    /// Top up the funding balance.
    pub fn fund(&mut self, amount: Amount) -> Result<(), ContractError> {
        self.balance = self
            .balance
            .checked_add(amount)
            .ok_or(ContractError::AmountOverflow)?;
        Ok(())
    }

    /// Pause the stream. While paused, no ticks accrue.
    pub fn pause(&mut self, party: Address, clock: &dyn Clock) -> Result<(), ContractError> {
        if party != self.config.payer && party != self.config.payee {
            return Err(ContractError::UnauthorisedParty);
        }
        if self.paused_at.is_some() {
            return Err(ContractError::InvalidState("already paused"));
        }
        self.paused_at = Some(clock.now());
        Ok(())
    }

    /// Resume after a pause. Ticks that *would* have fired during the
    /// pause are simply skipped -- they are neither paid nor recorded
    /// as missed.
    pub fn resume(&mut self, party: Address) -> Result<(), ContractError> {
        if party != self.config.payer && party != self.config.payee {
            return Err(ContractError::UnauthorisedParty);
        }
        if self.paused_at.is_none() {
            return Err(ContractError::InvalidState("not paused"));
        }
        self.paused_at = None;
        Ok(())
    }

    /// Advance the contract up to `clock.now()`, emitting payouts for
    /// every tick that has fired and hasn't been processed yet.
    ///
    /// Returns the list of generated payouts. Idempotent up to clock
    /// movement: calling `advance` twice at the same `now` produces
    /// payouts the first time and an empty vector the second.
    pub fn advance(&mut self, clock: &dyn Clock) -> Vec<Payout> {
        if self.paused_at.is_some() {
            return Vec::new();
        }
        let schedule = Schedule {
            interval: self.config.interval,
            window: self.config.window,
        };
        let due = schedule.ticks_before(clock.now());
        let processed = self.ticks_paid.saturating_add(self.ticks_missed);
        if due <= processed {
            return Vec::new();
        }
        let mut payouts = Vec::new();
        for _ in processed..due {
            if self.balance >= self.config.amount_per_tick {
                self.balance -= self.config.amount_per_tick;
                self.ticks_paid += 1;
                self.consecutive_missed = 0;
                payouts.push(Payout {
                    to: self.config.payee,
                    amount: self.config.amount_per_tick,
                    reason: PayoutReason::RecurringTick,
                });
            } else {
                self.ticks_missed += 1;
                self.consecutive_missed += 1;
            }
        }
        payouts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lace_time::ManualClock;
    use lace_vm::Bytes32;

    fn addr(b: u8) -> Address {
        let mut x = [0u8; 32];
        x[0] = b;
        Bytes32(x)
    }

    fn fresh() -> RecurringPayment {
        RecurringPayment::new(RecurringConfig {
            payer: addr(1),
            payee: addr(2),
            amount_per_tick: 100,
            interval: Duration::from_secs(60),
            window: Interval::new(Timestamp::from_secs(0), Timestamp::from_secs(600)),
        })
        .unwrap()
    }

    #[test]
    fn ticks_fire_at_expected_times() {
        let mut r = fresh();
        r.fund(1_000).unwrap();
        let mut clock = ManualClock::at(Timestamp::from_secs(0));
        assert!(r.advance(&clock).is_empty());
        clock.set(Timestamp::from_secs(150));
        let payouts = r.advance(&clock);
        // ticks at t=0,60,120 -> ticks_before(150) == 2
        assert_eq!(payouts.len(), 2);
        assert_eq!(r.ticks_paid, 2);
        assert_eq!(r.balance, 800);
    }

    #[test]
    fn missed_ticks_advance_counter() {
        let mut r = fresh();
        r.fund(150).unwrap();
        let mut clock = ManualClock::at(Timestamp::from_secs(250));
        let payouts = r.advance(&clock);
        // ticks_before(250) == 4 (60,120,180,240 boundaries crossed)
        assert_eq!(payouts.len(), 1);
        assert_eq!(r.ticks_paid, 1);
        assert_eq!(r.ticks_missed, 3);
        assert_eq!(r.consecutive_missed, 3);

        clock.advance(Duration::from_secs(60));
        r.fund(100).unwrap();
        let payouts = r.advance(&clock);
        assert_eq!(payouts.len(), 1);
        assert_eq!(r.consecutive_missed, 0);
    }

    #[test]
    fn pause_freezes_ticks() {
        let mut r = fresh();
        r.fund(1_000).unwrap();
        let mut clock = ManualClock::at(Timestamp::from_secs(0));
        r.pause(addr(1), &clock).unwrap();
        clock.set(Timestamp::from_secs(300));
        assert!(r.advance(&clock).is_empty());
        r.resume(addr(1)).unwrap();
        // Schedule.ticks_before(300) is 5 (ticks at t=0,60,120,180,240
        // are all strictly < 300). None of them were paid during the
        // pause -- the pause is a "no payouts" window, not a "no clock"
        // window -- so they all catch up now.
        let payouts = r.advance(&clock);
        assert_eq!(payouts.len(), 5);
    }

    #[test]
    fn unauthorised_pause_rejected() {
        let mut r = fresh();
        let clock = ManualClock::at(Timestamp::from_secs(0));
        assert_eq!(
            r.pause(addr(99), &clock).unwrap_err(),
            ContractError::UnauthorisedParty
        );
    }
}
