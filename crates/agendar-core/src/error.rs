//! Typed errors and stable public error codes.
//!
//! The CLI and the Tauri host map these codes to the exit codes and JSON error
//! envelope The wire decoder only needs `Validation`
//! today; the remaining codes are part of the shared contract.

use std::fmt;

pub mod details;

/// Stable public error codes
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    Usage,
    Validation,
    NotFound,
    Context,
    Policy,
    Budget,
    Io,
    Index,
    BlobCorrupt,
    Busy,
    WriterFork,
    RequestConflict,
    Timeout,
    StaleAnswer,
    Withdrawn,
    Unsupported,
    NotImplemented,
    Consent,
    Cancelled,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "E_USAGE",
            Self::Validation => "E_VALIDATION",
            Self::NotFound => "E_NOT_FOUND",
            Self::Context => "E_CONTEXT",
            Self::Policy => "E_POLICY",
            Self::Budget => "E_BUDGET",
            Self::Io => "E_IO",
            Self::Index => "E_INDEX",
            Self::BlobCorrupt => "E_BLOB_CORRUPT",
            Self::Busy => "E_BUSY",
            Self::WriterFork => "E_WRITER_FORK",
            Self::RequestConflict => "E_REQUEST_CONFLICT",
            Self::Timeout => "E_TIMEOUT",
            Self::StaleAnswer => "E_STALE_ANSWER",
            Self::Withdrawn => "E_WITHDRAWN",
            Self::Unsupported => "E_UNSUPPORTED",
            Self::NotImplemented => "E_NOT_IMPLEMENTED",
            Self::Consent => "E_CONSENT",
            Self::Cancelled => "E_CANCELLED",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Public Agendar error with an optional JSON Pointer field, stable detail code,
/// hint and retryability flag.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AgendarError {
    code: ErrorCode,
    message: String,
    field: Option<String>,
    detail: Option<String>,
    hint: Option<String>,
    retryable: bool,
}

impl AgendarError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
            detail: None,
            hint: None,
            retryable: false,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }

    pub fn validation_at(
        field: impl Into<String>,
        detail: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(ErrorCode::Validation, message)
            .with_field(field)
            .with_detail(detail)
    }

    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub const fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub const fn retryable(&self) -> bool {
        self.retryable
    }
}

impl From<std::io::Error> for AgendarError {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("Cannot access the Vault: {error}"))
    }
}

pub type Result<T> = std::result::Result<T, AgendarError>;
