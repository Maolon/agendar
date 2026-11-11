//! Contract v1: snooze a need on every host.
//!
//! The same path as the other writes: one write at a time, a refresh inside
//! [`WRITE_REFRESH_BOUND`](super::write::WRITE_REFRESH_BOUND), the checks
//! against that read, `not_synced` while a file of the project has not
//! downloaded, and only then the owner's `need.snoozed` event.
#![forbid(unsafe_code)]

use std::sync::PoisonError;

use agendar_core::{AgendarError, ErrorCode};

use super::shared::utc_time;
use super::write::WRITES;
use super::{FfiError, RefusalCode, Session};

impl Session {
    /// Hide a waiting need until `until` (at most seven days ahead); a time
    /// in the past ends the snooze. Returns the event id.
    ///
    /// Refused, with nothing written: `refresh_timeout` and `not_synced` as
    /// for an answer, and `stale_revision` when the need is no longer
    /// waiting. An `until` that is not a time or is more than seven days
    /// ahead is `E_VALIDATION` (`invalid_time`, `snooze_too_long`). A repeat
    /// of the snooze in effect returns its event and writes nothing.
    ///
    /// A need with a default time (`NeedRow.default_at`) is hidden at most
    /// until 15 minutes before it: a later `until` is cut to that time, and
    /// the next read gives the cut time in `snoozed_until`. When that time is
    /// not ahead any more the snooze is `E_VALIDATION`
    /// (`snooze_default_near`, field `/until`).
    pub fn snooze(&self, need_id: String, until: String) -> Result<String, FfiError> {
        let until = utc_time("/until", &until)?;
        let _writes = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = self.write_refresh()?;
        let project = fresh
            .state
            .projects
            .values()
            .find(|project| project.needs_you.iter().any(|need| need.id == need_id))
            .map(|project| project.summary.slug.clone())
            .ok_or_else(gone)?;
        self.refuse_while_waiting(&fresh, &project)?;
        let event = self
            .service
            .snooze_need(&fresh, &need_id, until)
            .map_err(refused)?;
        Ok(event.event.as_str().to_owned())
    }
}

fn gone() -> FfiError {
    FfiError::refused(
        RefusalCode::StaleRevision,
        "This item is no longer waiting for you.",
    )
}

/// A need that left the list between the check and the write is the same
/// refusal as one that had left before.
fn refused(error: AgendarError) -> FfiError {
    match (error.code(), error.detail()) {
        (ErrorCode::NotFound, Some("need_not_waiting")) => gone(),
        _ => error.into(),
    }
}
