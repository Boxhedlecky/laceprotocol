//! The temporal VM executor.
//!
//! Stack-based, step-bounded, deterministic. The executor is the only
//! place in the workspace that actually consults [`Clock::now`] -- all
//! other crates accept a `&dyn Clock` and route it through here, which
//! keeps the time-source surface area auditable.

use lace_time::{Clock, TimeDelta};

use crate::opcode::{Op, Program};
use crate::scheduler::Schedule;
use crate::value::{Value, VmError};

/// Default maximum stack depth. Picked to be generous for hand-rolled
/// contracts but well under any plausible memory bound.
pub const DEFAULT_MAX_STACK: usize = 1024;

/// Default per-execution step budget. Each opcode costs one step.
/// The `RECURRING` schedule emission is intentionally cheap because
/// it does *not* execute the recurring body -- that runs out-of-band
/// in the scheduler.
pub const DEFAULT_STEP_BUDGET: u64 = 100_000;

/// Outputs produced by a successful execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outputs {
    /// Final stack, top last.
    pub stack: Vec<Value>,
    /// Schedules emitted by `RECURRING`. The scheduler picks these up
    /// after the executor returns and queues them for tick replay.
    pub schedules: Vec<Schedule>,
}

/// The executor state, parametrised over a [`Clock`].
pub struct Executor<'c, C: Clock + ?Sized> {
    /// Clock supplying `NOW` and `HEIGHT`.
    pub clock: &'c C,
    /// Stack-depth limit.
    pub max_stack: usize,
    /// Per-execution step budget.
    pub step_budget: u64,
}

impl<'c, C: Clock + ?Sized> Executor<'c, C> {
    /// Construct an executor with default limits.
    pub fn new(clock: &'c C) -> Self {
        Self {
            clock,
            max_stack: DEFAULT_MAX_STACK,
            step_budget: DEFAULT_STEP_BUDGET,
        }
    }

    /// Run a program to completion.
    pub fn run(&self, program: &Program) -> Result<Outputs, VmError> {
        let mut stack: Vec<Value> = Vec::with_capacity(16);
        let mut schedules: Vec<Schedule> = Vec::new();
        let mut steps: u64 = 0;

        for op in &program.ops {
            steps = steps.checked_add(1).ok_or(VmError::OutOfGas)?;
            if steps > self.step_budget {
                return Err(VmError::OutOfGas);
            }

            match op {
                Op::Push(v) => self.push(&mut stack, v.clone())?,
                Op::Pop => {
                    pop(&mut stack)?;
                }
                Op::Dup => {
                    let top = stack.last().ok_or(VmError::StackUnderflow)?.clone();
                    self.push(&mut stack, top)?;
                }
                Op::Swap => {
                    let n = stack.len();
                    if n < 2 {
                        return Err(VmError::StackUnderflow);
                    }
                    stack.swap(n - 1, n - 2);
                }
                Op::Now => self.push(&mut stack, Value::Time(self.clock.now()))?,
                Op::Height => self.push(&mut stack, Value::Height(self.clock.height()))?,
                Op::After => {
                    let ts = pop(&mut stack)?.into_time()?;
                    if self.clock.now() <= ts {
                        return Err(VmError::GuardFailed);
                    }
                }
                Op::Before => {
                    let ts = pop(&mut stack)?.into_time()?;
                    if self.clock.now() >= ts {
                        return Err(VmError::GuardFailed);
                    }
                }
                Op::Deadline => {
                    let deadline = pop(&mut stack)?.into_time()?;
                    let now = self.clock.now();
                    if now > deadline {
                        return Err(VmError::DeadlineExceeded { deadline, now });
                    }
                }
                Op::Recurring => {
                    // Stack (top last): interval, start, end
                    let end = pop(&mut stack)?.into_time()?;
                    let start = pop(&mut stack)?.into_time()?;
                    let interval = pop(&mut stack)?.into_duration()?;
                    let schedule = Schedule {
                        interval,
                        window: lace_time::Interval::new(start, end),
                    };
                    schedule.validate()?;
                    schedules.push(schedule);
                }
                Op::TimeDelta => {
                    // Stack (top last): t1, t2. Compute t2 - t1.
                    let t2 = pop(&mut stack)?.into_time()?;
                    let t1 = pop(&mut stack)?.into_time()?;
                    self.push(&mut stack, Value::Delta(TimeDelta::between(t1, t2)))?;
                }
                Op::And => {
                    let b = pop(&mut stack)?.into_bool()?;
                    let a = pop(&mut stack)?.into_bool()?;
                    self.push(&mut stack, Value::Bool(a && b))?;
                }
                Op::Or => {
                    let b = pop(&mut stack)?.into_bool()?;
                    let a = pop(&mut stack)?.into_bool()?;
                    self.push(&mut stack, Value::Bool(a || b))?;
                }
                Op::Not => {
                    let b = pop(&mut stack)?.into_bool()?;
                    self.push(&mut stack, Value::Bool(!b))?;
                }
                Op::Eq => {
                    let b = pop(&mut stack)?;
                    let a = pop(&mut stack)?;
                    if a.type_name() != b.type_name() {
                        return Err(VmError::TypeMismatch {
                            expected: a.type_name(),
                            got: b.type_name(),
                        });
                    }
                    self.push(&mut stack, Value::Bool(a == b))?;
                }
                Op::Halt => break,
            }
        }

        Ok(Outputs { stack, schedules })
    }

    #[inline]
    fn push(&self, stack: &mut Vec<Value>, v: Value) -> Result<(), VmError> {
        if stack.len() >= self.max_stack {
            return Err(VmError::StackOverflow);
        }
        stack.push(v);
        Ok(())
    }
}

#[inline]
fn pop(stack: &mut Vec<Value>) -> Result<Value, VmError> {
    stack.pop().ok_or(VmError::StackUnderflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::Op;
    use lace_time::{Duration, ManualClock, Timestamp};

    fn run_at(now: u64, program: Program) -> Result<Outputs, VmError> {
        let clock = ManualClock::at(Timestamp::from_secs(now));
        Executor::new(&clock).run(&program)
    }

    #[test]
    fn after_blocks_until_time_passes() {
        let mut p = Program::new();
        p.time(Timestamp::from_secs(1_000)).push(Op::After);

        // now == deadline -> guard fails (strict >).
        assert_eq!(run_at(1_000, p.clone()).unwrap_err(), VmError::GuardFailed);
        // now > deadline -> proceeds.
        assert!(run_at(1_001, p).is_ok());
    }

    #[test]
    fn before_blocks_after_time_passes() {
        let mut p = Program::new();
        p.time(Timestamp::from_secs(1_000)).push(Op::Before);

        assert!(run_at(999, p.clone()).is_ok());
        assert_eq!(run_at(1_000, p).unwrap_err(), VmError::GuardFailed);
    }

    #[test]
    fn deadline_reports_distinct_error() {
        let mut p = Program::new();
        p.time(Timestamp::from_secs(1_000)).push(Op::Deadline);

        match run_at(1_001, p).unwrap_err() {
            VmError::DeadlineExceeded { deadline, now } => {
                assert_eq!(deadline.as_secs(), 1_000);
                assert_eq!(now.as_secs(), 1_001);
            }
            other => panic!("wrong error: {:?}", other),
        }
    }

    #[test]
    fn recurring_emits_schedule() {
        let mut p = Program::new();
        p.duration(Duration::from_secs(60))
            .time(Timestamp::from_secs(100))
            .time(Timestamp::from_secs(700))
            .push(Op::Recurring);

        let out = run_at(50, p).unwrap();
        assert_eq!(out.schedules.len(), 1);
        assert_eq!(out.schedules[0].interval.as_secs(), 60);
        assert_eq!(out.schedules[0].window.start.as_secs(), 100);
        assert_eq!(out.schedules[0].window.end.as_secs(), 700);
    }

    #[test]
    fn recurring_rejects_zero_interval() {
        let mut p = Program::new();
        p.duration(Duration::from_secs(0))
            .time(Timestamp::from_secs(0))
            .time(Timestamp::from_secs(100))
            .push(Op::Recurring);
        assert!(matches!(
            run_at(0, p).unwrap_err(),
            VmError::InvalidSchedule(_)
        ));
    }

    #[test]
    fn timedelta_pushes_signed_delta() {
        let mut p = Program::new();
        p.time(Timestamp::from_secs(1_000))
            .time(Timestamp::from_secs(1_500))
            .push(Op::TimeDelta);
        let out = run_at(0, p).unwrap();
        assert_eq!(out.stack, vec![Value::Delta(TimeDelta(500))]);
    }

    #[test]
    fn type_mismatch_on_after_with_non_time() {
        let mut p = Program::new();
        p.literal(Value::Bool(true)).push(Op::After);
        match run_at(0, p).unwrap_err() {
            VmError::TypeMismatch { expected, got } => {
                assert_eq!(expected, "time");
                assert_eq!(got, "bool");
            }
            e => panic!("wrong error: {:?}", e),
        }
    }

    #[test]
    fn composable_after_and_before_form_window() {
        // execute iff start < now < end
        let mut p = Program::new();
        p.time(Timestamp::from_secs(1_000))
            .push(Op::After)
            .time(Timestamp::from_secs(2_000))
            .push(Op::Before);

        assert_eq!(run_at(500, p.clone()).unwrap_err(), VmError::GuardFailed);
        assert!(run_at(1_500, p.clone()).is_ok());
        assert_eq!(run_at(2_500, p).unwrap_err(), VmError::GuardFailed);
    }

    #[test]
    fn out_of_gas_terminates_runaway_program() {
        let mut p = Program::new();
        for _ in 0..50 {
            p.push(Op::Now).push(Op::Pop);
        }
        let clock = ManualClock::at(Timestamp::from_secs(0));
        let exe = Executor {
            clock: &clock,
            max_stack: DEFAULT_MAX_STACK,
            step_budget: 10,
        };
        assert_eq!(exe.run(&p).unwrap_err(), VmError::OutOfGas);
    }
}
