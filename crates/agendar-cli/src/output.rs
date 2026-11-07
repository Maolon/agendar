//! Stable JSON envelopes and exit codes.

use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode};
use agendar_core::service::Freshness;

/// Print one success envelope on stdout.
pub fn success(command: &str, data: Value, freshness: Option<Freshness>) -> i32 {
    let envelope = json!({
        "v": 1,
        "ok": true,
        "command": command,
        "data": data,
        "freshness": freshness,
    });
    match serde_json::to_string(&envelope) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&json!({
                    "v": 1,
                    "ok": false,
                    "error": {
                        "code": "E_IO",
                        "message": format!("Cannot serialize the command result: {error}."),
                        "hint": "Report this without retrying destructive actions.",
                        "retryable": false,
                    }
                }))
                .unwrap_or_default()
            );
            5
        }
    }
}

/// Print raw JSON on stdout without the CLI envelope (`state export`).
pub fn raw_json<T: serde::Serialize>(value: &T) -> i32 {
    match serde_json::to_string(value) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(serialize_error) => error(&AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the output: {serialize_error}."),
        )),
    }
}

/// Print canonical JSON (recursively sorted keys, no insignificant whitespace).
pub fn raw_canonical(value: &Value) -> i32 {
    println!("{}", agendar_core::model::canonical_json(value));
    0
}

/// Print one error envelope on stderr and return its exit code.
pub fn error(error: &AgendarError) -> i32 {
    let mut body = json!({
        "code": error.code().as_str(),
        "message": error.message(),
    });
    if let Some(field) = error.field() {
        body["field"] = Value::String(field.to_owned());
    }
    body["hint"] = Value::String(
        error
            .hint()
            .unwrap_or("Fix the input and retry without deleting logs.")
            .to_owned(),
    );
    body["retryable"] = Value::Bool(error.retryable());
    let envelope = json!({"v": 1, "ok": false, "error": body});
    eprintln!(
        "{}",
        serde_json::to_string(&envelope).unwrap_or_else(|_| {
            "{\"v\":1,\"ok\":false,\"error\":{\"code\":\"E_IO\",\"message\":\"Cannot serialize the error.\",\"retryable\":false}}".to_owned()
        })
    );
    exit_code(error.code())
}

/// Stable exit-code mapping
pub const fn exit_code(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::Usage | ErrorCode::Validation => 2,
        ErrorCode::NotFound | ErrorCode::Context => 3,
        ErrorCode::Policy | ErrorCode::Budget => 4,
        ErrorCode::Io | ErrorCode::Index | ErrorCode::BlobCorrupt => 5,
        ErrorCode::Busy | ErrorCode::WriterFork | ErrorCode::RequestConflict => 6,
        ErrorCode::Timeout => 7,
        ErrorCode::StaleAnswer | ErrorCode::Withdrawn => 8,
        ErrorCode::Unsupported | ErrorCode::NotImplemented | ErrorCode::Consent => 9,
        ErrorCode::Cancelled => 130,
    }
}
