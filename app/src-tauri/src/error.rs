//! Stable API envelope for the Tauri IPC commands.
//!
//! Every command resolves to `{v:1,ok:true,data}` or a typed error object; the
//! frontend's `ApiError` mirrors these fields exactly.

use serde::Serialize;
use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode};

/// One field-level detail in an error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiDetail {
    pub field: String,
    pub message: String,
}

/// A typed error that can cross the IPC boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<ApiDetail>>,
    pub hint: String,
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            field: None,
            details: None,
            hint: "Fix the input and retry without deleting logs.".to_owned(),
            retryable: false,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn with_detail(mut self, field: impl Into<String>, message: impl Into<String>) -> Self {
        self.details.get_or_insert_with(Vec::new).push(ApiDetail {
            field: field.into(),
            message: message.into(),
        });
        self
    }

    pub const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation.as_str(), message).with_field(field)
    }

    pub fn not_implemented(command: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented.as_str(),
            format!("{command} is not implemented in this version."),
        )
        .with_hint("This capability arrives in a later version.")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Consent.as_str(), message)
            .with_hint("This window is not allowed to call this command.")
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io.as_str(), message).retryable(true)
    }
}

impl From<AgendarError> for AppError {
    fn from(error: AgendarError) -> Self {
        let mut mapped = Self {
            code: error.code().as_str().to_owned(),
            message: error.message().to_owned(),
            field: error.field().map(str::to_owned),
            details: None,
            hint: error
                .hint()
                .unwrap_or("Fix the input and retry without deleting logs.")
                .to_owned(),
            retryable: error.retryable(),
        };
        if let (Some(field), Some(detail)) = (error.field(), error.detail()) {
            mapped.details = Some(vec![ApiDetail {
                field: field.to_owned(),
                message: detail.to_owned(),
            }]);
        }
        mapped
    }
}

pub type ApiResult<T> = std::result::Result<T, AppError>;

/// A successful IPC envelope.
pub fn ok<T: Serialize>(data: T) -> Value {
    match serde_json::to_value(data) {
        Ok(data) => json!({"v": 1, "ok": true, "data": data}),
        Err(error) => err(AppError::new(
            ErrorCode::Io.as_str(),
            format!("Cannot serialize the result: {error}."),
        )),
    }
}

/// A failed IPC envelope.
pub fn err(error: AppError) -> Value {
    json!({"v": 1, "ok": false, "error": error})
}

/// Runs a backend call and wraps the outcome into the wire envelope.
pub fn envelope<T: Serialize>(result: ApiResult<T>) -> Value {
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
