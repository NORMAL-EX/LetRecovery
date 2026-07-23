use std::fmt;
use std::io;

use serde::{Deserialize, Serialize};

/// Stable categories shared by UI, checkpoint, retry, and support tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationErrorKind {
    Validation,
    Io,
    CommandStart,
    CommandExit,
    Verification,
    Unsupported,
    Cancelled,
    State,
    Unknown,
}

/// A serializable error that preserves category and retry intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationError {
    pub kind: OperationErrorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    pub retryable: bool,
}

impl OperationError {
    pub fn new(
        kind: OperationErrorKind,
        code: Option<impl Into<String>>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            kind,
            code: code.map(Into::into),
            message: message.into(),
            retryable,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(
            OperationErrorKind::Validation,
            None::<String>,
            message,
            false,
        )
    }

    pub fn state(message: impl Into<String>) -> Self {
        Self::new(OperationErrorKind::State, None::<String>, message, false)
    }

    pub fn verification(message: impl Into<String>) -> Self {
        Self::new(
            OperationErrorKind::Verification,
            None::<String>,
            message,
            false,
        )
    }

    pub fn command_start(program: &str, error: &io::Error) -> Self {
        Self::new(
            OperationErrorKind::CommandStart,
            Some(program),
            format!("failed to start {program}: {error}"),
            is_transient_io_error(error),
        )
    }

    pub fn command_exit(program: &str, exit_code: Option<i32>, detail: impl Into<String>) -> Self {
        Self::new(
            OperationErrorKind::CommandExit,
            exit_code.map(|code| code.to_string()),
            format!("{program} failed: {}", detail.into()),
            false,
        )
    }

    pub fn io(action: &str, error: &io::Error) -> Self {
        Self::new(
            OperationErrorKind::Io,
            Some(format!("{:?}", error.kind())),
            format!("{action}: {error}"),
            is_transient_io_error(error),
        )
    }

    pub fn transient_io(action: &str, error: impl fmt::Display) -> Self {
        Self::new(
            OperationErrorKind::Io,
            None::<String>,
            format!("{action}: {error}"),
            true,
        )
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(
            OperationErrorKind::Cancelled,
            None::<String>,
            message,
            false,
        )
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(code) = &self.code {
            write!(formatter, "{} [{code}]", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for OperationError {}

fn is_transient_io_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::NotConnected
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_retryability_is_explicit_and_stable() {
        let transient = OperationError::io(
            "read checkpoint",
            &io::Error::new(io::ErrorKind::Interrupted, "retry"),
        );
        assert!(transient.retryable);

        let permanent = OperationError::io(
            "read checkpoint",
            &io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        );
        assert!(!permanent.retryable);
    }

    #[test]
    fn display_retains_optional_code() {
        let error = OperationError::new(
            OperationErrorKind::CommandExit,
            Some("5"),
            "command failed",
            false,
        );
        assert_eq!(error.to_string(), "command failed [5]");
    }
}
