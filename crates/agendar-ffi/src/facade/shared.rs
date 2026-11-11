//! Helpers the contract v1 calls share (ffi only): the two kinds of read, the
//! id and time parsers, and the names a row shows.
#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use agendar_core::error::details;
use agendar_core::model::{
    EventId, Harness, NodePath, OptionId, ProjectSlug, ProjectState, Sha256, UtcTime,
};
use agendar_core::service::Fresh;
use agendar_core::vault::{Clock, SystemClock};
use agendar_core::{AgendarError, ErrorCode};

use super::{FfiError, RefusalCode, Session};

impl Session {
    /// The cached index reduced at the current time. Reads no Vault file.
    pub(super) fn cached(&self) -> Result<Fresh, FfiError> {
        Ok(self.service.refresh(SystemClock.now(), true)?)
    }

    /// Converge the index with the Vault, waiting at most `deadline_ms` for
    /// the index locks. A lock that stays busy is `Refused(RefreshTimeout)`.
    /// The ingest itself is not bounded.
    pub(super) fn bounded_refresh(&self, deadline_ms: u32) -> Result<Fresh, FfiError> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(deadline_ms));
        self.service
            .refresh_until(SystemClock.now(), false, Some(deadline))
            .map_err(|error| {
                if error.code() == ErrorCode::Busy {
                    FfiError::refused(
                        RefusalCode::RefreshTimeout,
                        "The Vault index stayed busy; nothing was read.",
                    )
                } else {
                    error.into()
                }
            })
    }
}

/// `E_NOT_FOUND` for an id this device has not read.
pub(super) fn not_found(what: &str, id: &str) -> FfiError {
    AgendarError::new(ErrorCode::NotFound, format!("{what} {id} was not found."))
        .with_detail(details::NOT_FOUND)
        .into()
}

/// A value from FFI callers that did not parse. The core's parsers say why in a
/// sentence only, so the input's name and the rule are added here.
fn named<T>(
    parsed: agendar_core::Result<T>,
    field: &str,
    detail: &'static str,
) -> Result<T, FfiError> {
    parsed.map_err(|error| {
        let error = match error.field() {
            Some(_) => error,
            None => error.with_field(field),
        };
        match error.detail() {
            Some(_) => error,
            None => error.with_detail(detail),
        }
        .into()
    })
}

/// An event id from FFI callers (`field` names the argument); `E_VALIDATION` with
/// `invalid_event_id` when it is not one.
pub(super) fn event_id(field: &str, value: &str) -> Result<EventId, FfiError> {
    named(EventId::parse(value), field, details::INVALID_EVENT_ID)
}

/// The `expected_revision` of a checked write; `invalid_revision` when it is
/// not a revision.
pub(super) fn revision(value: &str) -> Result<Sha256, FfiError> {
    named(
        Sha256::parse(value),
        "/expected_revision",
        details::INVALID_REVISION,
    )
}

/// An option id from FFI callers; `invalid_option_id` when it is not one.
pub(super) fn option_id(field: &str, value: &str) -> Result<OptionId, FfiError> {
    named(OptionId::parse(value), field, details::INVALID_OPTION_ID)
}

/// A project slug from FFI callers; `invalid_project` when it is not one.
pub(super) fn project_slug(field: &str, value: &str) -> Result<ProjectSlug, FfiError> {
    named(ProjectSlug::parse(value), field, details::INVALID_PROJECT)
}

/// A UTC time from FFI callers (`...Z`, with or without a fraction of a second);
/// `E_VALIDATION` with `invalid_time` when it is not one. The core's times
/// have exactly three fraction digits, so a missing fraction is filled and a
/// longer one is cut.
pub(super) fn utc_time(field: &str, value: &str) -> Result<UtcTime, FfiError> {
    let canonical = match value
        .strip_suffix('Z')
        .map(|body| (body, body.split_once('.')))
    {
        Some((body, None)) => format!("{body}.000Z"),
        Some((_, Some((seconds, fraction))))
            if !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let millis: String = fraction.chars().chain("00".chars()).take(3).collect();
            format!("{seconds}.{millis}Z")
        }
        _ => value.to_owned(),
    };
    named(UtcTime::parse(&canonical), field, details::INVALID_TIME)
}

/// The title of a step, or its node path when the plan no longer has it.
pub(super) fn step_title(project: &ProjectState, node: &NodePath) -> String {
    project
        .features
        .get(node)
        .map_or_else(|| node.as_str().to_owned(), |feature| feature.title.clone())
}

/// A count as the contract carries it.
pub(super) fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The harness as the core spells it on the wire (`claude_code`, ...).
pub(super) fn harness_name(harness: Harness) -> String {
    serde_json::to_value(harness)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
