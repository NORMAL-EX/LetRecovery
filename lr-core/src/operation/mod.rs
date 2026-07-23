//! Durable, endpoint-neutral operation state for install, backup, and repair workflows.
//!
//! The module records progress and makes retry safety explicit. It does not
//! execute disk commands and therefore remains safe to exercise in unit tests.

mod checkpoint;
mod error;
mod retry;
mod support;

pub use checkpoint::{
    CheckpointStore, OperationCheckpoint, OperationJournal, OperationKind, OperationStatus,
    StepDefinition, StepStatus, StepTransition, TargetFingerprint, CHECKPOINT_SCHEMA_VERSION,
};
pub use error::{OperationError, OperationErrorKind};
pub use retry::{
    run_with_retry, RetryDecision, RetryPolicy, RetrySafety, RetrySleeper, ThreadSleeper,
};
pub use support::{
    SupportAttachment, SupportBundle, SupportBundleBuilder, SupportOperationSummary,
};

/// Current Unix time in milliseconds, saturating if the system clock is before
/// the Unix epoch or the platform duration is larger than `u64`.
pub fn unix_time_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
