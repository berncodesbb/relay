//! Attempt budget enforcement.
//!
//! [`BudgetGuard`] tracks one attempt against its grant's [`Budget`] and
//! fires the attempt's [`CancelToken`] the moment a ceiling is crossed.
//! Cancellation is cooperative and pervasive: the token threads into
//! `RunContext`, into every `ToolContext`, and into the process supervisor,
//! so token exhaustion, attempt-count exhaustion, or duration exhaustion
//! all stop work mid-tool rather than after it.
//!
//! Checks happen (a) cooperatively, before and after every model call and
//! around tool batches, and (b) through a background
//! [`watchdog`](self::spawn_watchdog) when the orchestrator runs on real
//! threads, which enforces `max_duration_ms` wall-clock even if an
//! executor stops checking.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use relay_domain::Budget;
use relay_providers::contract::CancelToken;

use crate::spec::{Clock, Spawn};

/// Which budget ceiling was crossed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetExhausted {
    /// Input or output tokens exceeded the grant's ceilings.
    #[error(
        "token budget exhausted: input {} > {:?}, output {} > {:?}",
        input_tokens,
        max_input_tokens,
        output_tokens,
        max_output_tokens
    )]
    Tokens {
        /// Input tokens consumed so far.
        input_tokens: u64,
        /// The input ceiling, when set.
        max_input_tokens: Option<u64>,
        /// Output tokens consumed so far.
        output_tokens: u64,
        /// The output ceiling, when set.
        max_output_tokens: Option<u64>,
    },
    /// More model/tool attempts than the grant permits.
    #[error("attempt-count budget exhausted: {attempts} > {max_attempts:?}")]
    Attempts {
        /// Attempts performed so far.
        attempts: u64,
        /// The attempt ceiling.
        max_attempts: Option<u64>,
    },
    /// The wall-clock duration ceiling was crossed.
    #[error("duration budget exhausted: {elapsed_ms}ms > {max_duration_ms:?}ms")]
    Duration {
        /// Milliseconds elapsed since the attempt started.
        elapsed_ms: i64,
        /// The duration ceiling.
        max_duration_ms: Option<i64>,
    },
}

#[derive(Debug, Default)]
struct BudgetState {
    input_tokens: u64,
    output_tokens: u64,
    attempts: u64,
    fired: Option<BudgetExhausted>,
}

/// Tracks one attempt against its budget and fires the attempt's
/// cancellation token when a ceiling is crossed.
pub struct BudgetGuard {
    budget: Budget,
    started_at_ms: i64,
    clock: Clock,
    signal: CancelToken,
    state: Mutex<BudgetState>,
}

impl BudgetGuard {
    /// Arm a guard for an attempt started at `started_at_ms`.
    pub fn new(budget: Budget, started_at_ms: i64, clock: Clock, signal: CancelToken) -> Self {
        BudgetGuard {
            budget,
            started_at_ms,
            clock,
            signal,
            state: Mutex::new(BudgetState::default()),
        }
    }

    /// The budget being enforced.
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Record one model call's token usage, then enforce every ceiling.
    /// Exhaustion fires the cancellation token before returning the error.
    pub fn record_model_usage(
        &self,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), BudgetExhausted> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.input_tokens = state.input_tokens.saturating_add(input_tokens);
        state.output_tokens = state.output_tokens.saturating_add(output_tokens);
        if let Some(max_input) = self.budget.max_input_tokens
            && state.input_tokens > max_input
        {
            let exhausted = BudgetExhausted::Tokens {
                input_tokens: state.input_tokens,
                max_input_tokens: self.budget.max_input_tokens,
                output_tokens: state.output_tokens,
                max_output_tokens: self.budget.max_output_tokens,
            };
            return Err(self.fire_locked(&mut state, exhausted));
        }
        if let Some(max_output) = self.budget.max_output_tokens
            && state.output_tokens > max_output
        {
            let exhausted = BudgetExhausted::Tokens {
                input_tokens: state.input_tokens,
                max_input_tokens: self.budget.max_input_tokens,
                output_tokens: state.output_tokens,
                max_output_tokens: self.budget.max_output_tokens,
            };
            return Err(self.fire_locked(&mut state, exhausted));
        }
        self.enforce_locked(&mut state)
    }

    /// Record one model/tool attempt, then enforce every ceiling.
    pub fn record_attempt(&self) -> Result<(), BudgetExhausted> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.attempts += 1;
        if let Some(max_attempts) = self.budget.max_attempts
            && state.attempts > max_attempts
        {
            let exhausted = BudgetExhausted::Attempts {
                attempts: state.attempts,
                max_attempts: self.budget.max_attempts,
            };
            return Err(self.fire_locked(&mut state, exhausted));
        }
        self.enforce_locked(&mut state)
    }

    /// Check every ceiling without recording anything. Executors call this
    /// cooperatively; the orchestrator calls it around executor runs.
    pub fn enforce(&self) -> Result<(), BudgetExhausted> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.enforce_locked(&mut state)
    }

    /// The exhaustion that fired the cancellation token, when one did.
    pub fn exhaustion(&self) -> Option<BudgetExhausted> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .fired
            .clone()
    }

    fn enforce_locked(&self, state: &mut BudgetState) -> Result<(), BudgetExhausted> {
        if let Some(fired) = &state.fired {
            return Err(fired.clone());
        }
        if let Some(max_duration) = self.budget.max_duration_ms {
            let now = (self.clock)();
            let elapsed = now - self.started_at_ms;
            if elapsed > max_duration {
                return Err(self.fire_locked(
                    state,
                    BudgetExhausted::Duration {
                        elapsed_ms: elapsed,
                        max_duration_ms: self.budget.max_duration_ms,
                    },
                ));
            }
        }
        Ok(())
    }

    /// Fire the attempt's cancellation token and record why. Requires the
    /// state lock to already be held.
    fn fire_locked(&self, state: &mut BudgetState, exhausted: BudgetExhausted) -> BudgetExhausted {
        if state.fired.is_none() {
            state.fired = Some(exhausted.clone());
            self.signal.cancel(format!("budget exhausted: {exhausted}"));
        }
        exhausted
    }
}

/// Poll a guard on a background job until its attempt ends, enforcing the
/// duration ceiling even if the executor stops checking. The loop exits
/// when the guard fires, the attempt's signal is cancelled for any reason,
/// or the stop token fires.
pub fn spawn_watchdog(guard: Arc<BudgetGuard>, stop: CancelToken, spawn: &Spawn) {
    let signal = guard.signal.clone();
    spawn(Box::new(move || {
        while !signal.is_cancelled() && !stop.is_cancelled() {
            if guard.enforce().is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::spec::ManualClock;

    fn guard(budget: Budget, clock: &Arc<ManualClock>) -> (Arc<BudgetGuard>, CancelToken) {
        let signal = CancelToken::new();
        let guard = BudgetGuard::new(
            budget,
            1_000,
            Arc::clone(clock).into_clock(),
            signal.clone(),
        );
        (Arc::new(guard), signal)
    }

    #[test]
    fn token_exhaustion_fires_the_cancel_token() {
        let clock = Arc::new(ManualClock::new(1_000));
        let (guard, signal) = guard(
            Budget {
                max_input_tokens: Some(10),
                ..Budget::default()
            },
            &clock,
        );
        assert!(guard.record_model_usage(4, 4).is_ok());
        assert!(!signal.is_cancelled());
        let exhausted = guard.record_model_usage(8, 0).expect_err("over ceiling");
        assert!(matches!(exhausted, BudgetExhausted::Tokens { .. }));
        assert!(signal.is_cancelled());
        assert!(
            signal
                .reason()
                .as_deref()
                .is_some_and(|reason| reason.contains("budget exhausted"))
        );
        // Further checks keep reporting exhaustion.
        assert!(guard.enforce().is_err());
    }

    #[test]
    fn attempt_counts_and_durations_are_enforced() {
        let clock = Arc::new(ManualClock::new(1_000));
        let (attempts_guard, attempts_signal) = guard(
            Budget {
                max_attempts: Some(2),
                ..Budget::default()
            },
            &clock,
        );
        assert!(attempts_guard.record_attempt().is_ok());
        assert!(attempts_guard.record_attempt().is_ok());
        assert!(matches!(
            attempts_guard.record_attempt(),
            Err(BudgetExhausted::Attempts { .. })
        ));
        assert!(attempts_signal.is_cancelled());

        let clock = Arc::new(ManualClock::new(1_000));
        let (duration_guard, duration_signal) = guard(
            Budget {
                max_duration_ms: Some(500),
                ..Budget::default()
            },
            &clock,
        );
        clock.advance(400);
        assert!(duration_guard.enforce().is_ok());
        clock.advance(200);
        assert!(matches!(
            duration_guard.enforce(),
            Err(BudgetExhausted::Duration { .. })
        ));
        assert!(duration_signal.is_cancelled());
    }

    #[test]
    fn unset_ceilings_never_fire() {
        let clock = Arc::new(ManualClock::new(1_000));
        let (guard, signal) = guard(Budget::default(), &clock);
        clock.advance(1_000_000);
        for _ in 0..100 {
            assert!(guard.record_model_usage(u64::MAX / 1_000, 0).is_ok());
            assert!(guard.record_attempt().is_ok());
        }
        assert!(!signal.is_cancelled());
    }
}
