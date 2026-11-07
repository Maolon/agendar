//! Nudges: derived reminders for one session, shown with its `inbox` (overdue
//! ETAs also with `board`). A nudge is not a packet: it has no id and is never
//! acked. An overdue ETA disappears once the agent re-estimates or finishes,
//! measured against the same window the app shows. A plan change made by a
//! human is shown to each session once, then a local cursor moves past it.

use serde::Serialize;

use crate::model::event::{Event, EventPayload};
use crate::model::ids::{EventId, NodePath, ProjectSlug, SessionId, UtcTime};
use crate::model::state::{Progress, ProjectState};

/// A reminder addressed to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Nudge {
    pub kind: NudgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<EventId>,
    /// The finish bound that passed (`eta_overdue`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<UtcTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub late_by_s: Option<u64>,
    /// The human edit this nudge reports (`plan_changed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventId>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NudgeKind {
    /// The step is still running past its ETA: re-estimate or finish it.
    EtaOverdue,
    /// A human changed the goal, a step, the rules or the policy.
    PlanChanged,
}

/// Human edits to `project` in `ordered` (O-order) that a session has not
/// been shown: after the event `after`, or from `since` when it has no cursor.
pub fn plan_changes(
    ordered: &[Event],
    project: &ProjectSlug,
    after: Option<&EventId>,
    since: UtcTime,
) -> Vec<Nudge> {
    // A cursor that is no longer in the log (an index rebuild) falls back to
    // the time, so old edits are never replayed.
    let cursor = after.and_then(|cursor| ordered.iter().position(|event| &event.id == cursor));
    let start = cursor.map_or(0, |index| index + 1);
    ordered[start.min(ordered.len())..]
        .iter()
        .filter(|event| &event.project == project && event.writer.is_operator())
        .filter(|event| cursor.is_some() || event.ts >= since)
        .filter_map(|event| {
            let (node, text) = describe(event)?;
            Some(Nudge {
                kind: NudgeKind::PlanChanged,
                node,
                attempt: None,
                due_at: None,
                late_by_s: None,
                event: Some(event.id.clone()),
                text,
            })
        })
        .collect()
}

fn describe(event: &Event) -> Option<(Option<NodePath>, String)> {
    match &event.payload {
        EventPayload::ProjectDeclared(body) => {
            let mut parts = Vec::new();
            if let Some(goal) = &body.goal {
                parts.push(format!("the goal is now: {goal}"));
            }
            if let Some(brief) = &body.brief {
                parts.push(format!("the brief is now: {brief}"));
            }
            if let Some(title) = &body.title {
                parts.push(format!("the project is now called {title}"));
            }
            match body.archived {
                Some(true) => parts.push("the project was archived".to_owned()),
                Some(false) => parts.push("the project was restored".to_owned()),
                None => {}
            }
            (!parts.is_empty()).then(|| {
                (
                    None,
                    format!("A human changed the project: {}.", parts.join("; ")),
                )
            })
        }
        EventPayload::PlanFeature(body) => {
            let node = event.refs.node.clone()?;
            let mut fields = Vec::new();
            for (present, name) in [
                (body.title.is_some(), "title"),
                (body.depends_on.is_some(), "depends_on"),
                (body.milestone.is_some(), "milestone"),
                (body.deadline.is_some(), "deadline"),
                (body.priority.is_some(), "priority"),
                (body.slice.is_some(), "slice"),
                (body.demo_kind.is_some(), "demo_kind"),
            ] {
                if present {
                    fields.push(name);
                }
            }
            let mut text = if fields.is_empty() {
                String::new()
            } else {
                format!("A human changed {node}: {}.", fields.join(", "))
            };
            match body.paused {
                Some(true) => text.push_str(&format!(" {node} is paused.")),
                Some(false) => text.push_str(&format!(" {node} is resumed.")),
                None => {}
            }
            if body.handoff == Some(true) {
                text.push_str(&format!(" {node} was handed off to another agent."));
            }
            let text = text.trim().to_owned();
            (!text.is_empty()).then(|| (Some(node), format!("{text} Read board before going on.")))
        }
        EventPayload::RuleSet(_) => Some((
            None,
            "A human changed the house rules; read rules before going on.".to_owned(),
        )),
        EventPayload::PolicySet(_) => Some((
            None,
            "A human changed the decision policy; it applies to your next ask.".to_owned(),
        )),
        _ => None,
    }
}

/// Nudges for the steps `session` currently runs in `project`, by node.
/// A step on hold (waiting for a human) is never overdue.
pub fn for_session(project: &ProjectState, session: &SessionId) -> Vec<Nudge> {
    project
        .features
        .values()
        .filter(|feature| feature.progress == Progress::Doing)
        .filter(|feature| feature.session.as_ref() == Some(session))
        .filter_map(|feature| {
            let attempt = feature.attempt.clone()?;
            let eta = feature.eta.as_ref()?;
            if eta.late_by_s == 0 || eta.paused {
                return None;
            }
            let due_at = eta
                .likely_finish
                .as_ref()
                .map_or(eta.agent_finish.latest, |range| range.latest);
            Some(Nudge {
                kind: NudgeKind::EtaOverdue,
                text: format!(
                    "Your ETA for {} passed {} ago. Still working: pulse with --eta \
                     to re-estimate. Finished: report done.",
                    feature.node,
                    short_duration(eta.late_by_s)
                ),
                node: Some(feature.node.clone()),
                attempt: Some(attempt),
                due_at: Some(due_at),
                late_by_s: Some(eta.late_by_s),
                event: None,
            })
        })
        .collect()
}

/// `45s`, `12m`, `1h05m`, `2d3h`.
pub fn short_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if seconds < 60 {
        format!("{seconds}s")
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if hours < 24 {
        format!("{hours}h{:02}m", minutes % 60)
    } else {
        format!("{days}d{}h", hours % 24)
    }
}

#[cfg(test)]
mod tests {
    use super::short_duration;

    #[test]
    fn durations_stay_short() {
        assert_eq!(short_duration(45), "45s");
        assert_eq!(short_duration(12 * 60 + 30), "12m");
        assert_eq!(short_duration(65 * 60), "1h05m");
        assert_eq!(short_duration(51 * 3600), "2d3h");
    }
}
