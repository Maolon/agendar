//! The owner snoozes a Needs-you row (`need.snoozed`, written by the
//! operator), for every host that reads the Vault.

use crate::error::{AgendarError, ErrorCode, Result};
use crate::guard::need_revision;
use crate::model::event::{EventPayload, MAX_SNOOZE_MS, NeedSnoozed, Refs};
use crate::model::ids::{EventId, UtcTime};
use crate::model::state::{NeedsYouItem, ProjectState};
use crate::reduce::snooze::{effective, limit_in};
use crate::vault::EventDraft as VaultDraft;

use super::{Fresh, Service};

/// A snooze as written (or as already in effect).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snoozed {
    pub event: EventId,
    /// When the row really returns: the requested time, cut to the row's
    /// limit before its default. This is the `until` the event stores.
    pub until: UtcTime,
}

impl Service {
    /// Hide the row `need_id` until `until`, or end its snooze with a time
    /// that has passed. `fresh` is the read the caller checked the request
    /// against. Refused without writing: an `until` more than seven days
    /// after `fresh.now` (`snooze_too_long`), a row that is not waiting in
    /// that read (`E_NOT_FOUND`, `need_not_waiting`), and a row whose default
    /// is so near that it could not be hidden at all (`snooze_default_near`).
    ///
    /// A row with a default time is hidden at most until
    /// [`BEFORE_DEFAULT_S`](crate::reduce::snooze::BEFORE_DEFAULT_S) before
    /// it: a later `until` is cut to that time, and the cut time is what the
    /// event stores and what is returned.
    ///
    /// The event carries the row's revision in that read, so the snooze stops
    /// hiding the row once it changes. A repeat of the snooze in effect
    /// returns that event and writes nothing.
    pub fn snooze_need(&self, fresh: &Fresh, need_id: &str, until: UtcTime) -> Result<Snoozed> {
        if until.millis() - fresh.now.millis() > MAX_SNOOZE_MS {
            return Err(AgendarError::validation_at(
                "/until",
                "snooze_too_long",
                "A snooze can last at most seven days.",
            ));
        }
        let Some((project, need)) = find_need(fresh, need_id) else {
            return Err(AgendarError::new(
                ErrorCode::NotFound,
                "This item is no longer waiting for you.",
            )
            .with_detail("need_not_waiting"));
        };
        // A time that has passed ends the snooze and is stored as given.
        let until = if until > fresh.now {
            let cut = effective(until, limit_in(project, need));
            if cut <= fresh.now {
                return Err(AgendarError::validation_at(
                    "/until",
                    "snooze_default_near",
                    "The default is too near to snooze this item.",
                ));
            }
            cut
        } else {
            until
        };
        let body = NeedSnoozed {
            need: need.id.clone(),
            revision: need_revision(
                need,
                project.decisions.get(&need.source),
                &project.policy.revision,
            ),
            until,
        };
        let slug = &project.summary.slug;
        let in_effect = fresh.resolution.ordered.iter().rev().find(|event| {
            &event.project == slug
                && matches!(&event.payload, EventPayload::NeedSnoozed(last) if last.need == body.need)
        });
        if let Some(event) = in_effect
            && event.payload == EventPayload::NeedSnoozed(body.clone())
        {
            return Ok(Snoozed {
                event: event.id.clone(),
                until,
            });
        }
        let draft = VaultDraft {
            refs: Refs::default(),
            payload: EventPayload::NeedSnoozed(body),
        };
        let event = self.append(&self.operator_key(slug), draft, None)?.event.id;
        Ok(Snoozed { event, until })
    }
}

/// The row with this id and its project, in any project of the read.
fn find_need<'a>(fresh: &'a Fresh, need_id: &str) -> Option<(&'a ProjectState, &'a NeedsYouItem)> {
    fresh.state.projects.values().find_map(|project| {
        project
            .needs_you
            .iter()
            .find(|need| need.id == need_id)
            .map(|need| (project, need))
    })
}
