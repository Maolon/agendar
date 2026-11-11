//! The owner's snoozes (`need.snoozed`): which Needs-you rows are hidden for
//! now, on every host.
//!
//! A snooze is display only. Its events are read here and nowhere else in the
//! reducer: they never touch a step, a session, a host's last word or a
//! project's last event, so everything an agent reads is the same with and
//! without them. The only thing they produce is `NeedsYouItem.snoozed_until`.
//!
//! For one row the snooze in effect is the latest `need.snoozed` naming it in
//! the canonical order `O`, which is a function of the event set alone; two
//! hosts that snooze at once therefore agree as soon as they hold the same
//! events, in whatever order those arrived. That snooze hides the row while
//! its `until` is still ahead and its `revision` is still the row's revision.
//!
//! A row whose question has a default time is hidden at most until
//! [`BEFORE_DEFAULT_S`] before that default ([`limit`]): it comes back then,
//! so the agent's default never runs out behind a snooze. The cut is made
//! here, at read time, so an event with a later `until` (an older build's) is
//! cut as well; `snoozed_until` is always the time the row really returns.

use std::collections::BTreeMap;

use crate::guard::need_revision;
use crate::model::event::{Event, EventPayload, NeedSnoozed};
use crate::model::ids::{ProjectSlug, UtcTime};
use crate::model::state::{
    DecisionLifecycle, DecisionState, NeedsKind, NeedsYouItem, ProjectState,
};

/// A snoozed row with a default time comes back this long before the default.
pub const BEFORE_DEFAULT_S: i64 = 15 * 60;

/// The latest time a snooze may hide this row: [`BEFORE_DEFAULT_S`] before
/// its default. None for a row with no default time: only a question that is
/// still open, in a project that lets questions time out, has one.
pub fn limit(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
    allow_timeouts: bool,
) -> Option<UtcTime> {
    decision
        .filter(|decision| {
            need.kind == NeedsKind::Decide
                && allow_timeouts
                && decision.lifecycle == DecisionLifecycle::Open
        })
        .and_then(|decision| decision.default_at)
        .map(|default_at| default_at.add_seconds(-BEFORE_DEFAULT_S))
}

/// [`limit`] for a row of `project`.
pub fn limit_in(project: &ProjectState, need: &NeedsYouItem) -> Option<UtcTime> {
    limit(
        need,
        project.decisions.get(&need.source),
        project.policy.value.allow_timeouts,
    )
}

/// When a snooze until `until` really ends for a row with this [`limit`].
pub fn effective(until: UtcTime, limit: Option<UtcTime>) -> UtcTime {
    limit.map_or(until, |limit| until.min(limit))
}

/// The latest snooze of every row, by project and row id.
pub type Snoozes<'a> = BTreeMap<&'a ProjectSlug, BTreeMap<&'a str, &'a NeedSnoozed>>;

/// Whether the reducer's fold skips this event (see the module text).
pub const fn is_snooze(event: &Event) -> bool {
    matches!(event.payload, EventPayload::NeedSnoozed(_))
}

/// Collect the snooze in effect for every row from the events in `O`: a
/// later one replaces an earlier one, whatever either says.
pub fn latest(ordered: &[Event]) -> Snoozes<'_> {
    let mut snoozes: Snoozes<'_> = BTreeMap::new();
    for event in ordered {
        if let EventPayload::NeedSnoozed(body) = &event.payload {
            snoozes
                .entry(&event.project)
                .or_default()
                .insert(body.need.as_str(), body);
        }
    }
    snoozes
}

/// Mark the rows of one project that are snoozed at `as_of`.
pub fn apply(project: &mut ProjectState, snoozes: &Snoozes<'_>, as_of: UtcTime) {
    let Some(snoozes) = snoozes.get(&project.summary.slug) else {
        return;
    };
    let ProjectState {
        needs_you,
        decisions,
        policy,
        ..
    } = project;
    for need in needs_you {
        let Some(snooze) = snoozes.get(need.id.as_str()) else {
            continue;
        };
        let decision = decisions.get(&need.source);
        let until = effective(
            snooze.until,
            limit(need, decision, policy.value.allow_timeouts),
        );
        if until > as_of && snooze.revision == need_revision(need, decision, &policy.revision) {
            need.snoozed_until = Some(until);
        }
    }
}
