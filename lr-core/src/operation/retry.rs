use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{OperationError, OperationErrorKind};

/// Whether repeating the caller's operation is known to be safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySafety {
    Idempotent,
    NonIdempotent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: u32,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        multiplier: u32,
    ) -> Result<Self, OperationError> {
        if max_attempts == 0 {
            return Err(OperationError::validation(
                "retry max_attempts must be at least one",
            ));
        }
        if multiplier == 0 {
            return Err(OperationError::validation(
                "retry multiplier must be at least one",
            ));
        }
        if max_delay_ms < initial_delay_ms {
            return Err(OperationError::validation(
                "retry max_delay_ms cannot be less than initial_delay_ms",
            ));
        }
        Ok(Self {
            max_attempts,
            initial_delay_ms,
            max_delay_ms,
            multiplier,
        })
    }

    pub fn transient_io() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 100,
            max_delay_ms: 1_000,
            multiplier: 2,
        }
    }

    pub fn decide(
        &self,
        safety: RetrySafety,
        completed_attempts: u32,
        error: &OperationError,
    ) -> RetryDecision {
        if safety != RetrySafety::Idempotent {
            return RetryDecision::Stop(RetryStopReason::NonIdempotent);
        }
        if !error.retryable {
            return RetryDecision::Stop(RetryStopReason::PermanentError);
        }
        if completed_attempts >= self.max_attempts {
            return RetryDecision::Stop(RetryStopReason::AttemptsExhausted);
        }

        let exponent = completed_attempts.saturating_sub(1);
        let factor = u64::from(self.multiplier).saturating_pow(exponent);
        let delay_ms = self
            .initial_delay_ms
            .saturating_mul(factor)
            .min(self.max_delay_ms);
        RetryDecision::RetryAfter(Duration::from_millis(delay_ms))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAfter(Duration),
    Stop(RetryStopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryStopReason {
    NonIdempotent,
    PermanentError,
    AttemptsExhausted,
}

pub trait RetrySleeper {
    fn sleep(&self, duration: Duration);
}

pub struct ThreadSleeper;

impl RetrySleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Run an operation under an explicit retry safety declaration.
///
/// The helper never retries a non-idempotent operation, even when the error is
/// marked transient. It also never sleeps after the final failed attempt.
pub fn run_with_retry<T, F, S>(
    policy: &RetryPolicy,
    safety: RetrySafety,
    sleeper: &S,
    mut operation: F,
) -> Result<T, OperationError>
where
    F: FnMut(u32) -> Result<T, OperationError>,
    S: RetrySleeper + ?Sized,
{
    for attempt in 1..=policy.max_attempts {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(error) => match policy.decide(safety, attempt, &error) {
                RetryDecision::RetryAfter(delay) => sleeper.sleep(delay),
                RetryDecision::Stop(_) => return Err(error),
            },
        }
    }

    Err(OperationError::new(
        OperationErrorKind::State,
        None::<String>,
        "retry loop exited without a result",
        false,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingSleeper(Mutex<Vec<Duration>>);

    impl RetrySleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.0.lock().unwrap().push(duration);
        }
    }

    fn transient_error() -> OperationError {
        OperationError::new(
            OperationErrorKind::Io,
            None::<String>,
            "temporarily unavailable",
            true,
        )
    }

    #[test]
    fn retries_idempotent_transient_failures_with_capped_backoff() {
        let policy = RetryPolicy::new(4, 10, 15, 2).unwrap();
        let sleeper = RecordingSleeper::default();
        let mut calls = 0;

        let result = run_with_retry(&policy, RetrySafety::Idempotent, &sleeper, |_| {
            calls += 1;
            if calls < 4 {
                Err(transient_error())
            } else {
                Ok("done")
            }
        })
        .unwrap();

        assert_eq!(result, "done");
        assert_eq!(calls, 4);
        assert_eq!(
            *sleeper.0.lock().unwrap(),
            vec![
                Duration::from_millis(10),
                Duration::from_millis(15),
                Duration::from_millis(15)
            ]
        );
    }

    #[test]
    fn never_retries_non_idempotent_operations() {
        let policy = RetryPolicy::transient_io();
        let sleeper = RecordingSleeper::default();
        let mut calls = 0;

        let error =
            run_with_retry::<(), _, _>(&policy, RetrySafety::NonIdempotent, &sleeper, |_| {
                calls += 1;
                Err(transient_error())
            })
            .unwrap_err();

        assert_eq!(calls, 1);
        assert!(error.retryable);
        assert!(sleeper.0.lock().unwrap().is_empty());
    }

    #[test]
    fn permanent_errors_stop_without_sleeping() {
        let policy = RetryPolicy::transient_io();
        let sleeper = RecordingSleeper::default();
        let error = OperationError::validation("invalid configuration");

        let actual = run_with_retry::<(), _, _>(&policy, RetrySafety::Idempotent, &sleeper, |_| {
            Err(error.clone())
        })
        .unwrap_err();

        assert_eq!(actual, error);
        assert!(sleeper.0.lock().unwrap().is_empty());
    }
}
