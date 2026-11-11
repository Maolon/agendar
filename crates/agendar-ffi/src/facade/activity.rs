//! The project page's activity, header numbers and deliveries, as the desktop
//! project view gets them: the core's activity feed and statistics over the
//! project's recent events, and the reduced deliveries. Nothing is derived
//! here; the rows only rename what the core computed.
#![forbid(unsafe_code)]

use agendar_core::model::{
    DeliveryState, Harness, NeedsKind, NeedsYouItem, ProjectState, ReviewStatus,
};
use agendar_core::reduce::activity::{ActivityItem, ActorKind, harness_label};
use agendar_core::reduce::stats::ProjectStats as CoreStats;

use super::shared::{count, harness_name, step_title};
use super::{ActivityKind, ActivityRow, CalibrationRow, DeliveryRow, ProjectStats, ReviewState};

/// Every harness, to read one back from the name an activity line gives its
/// actor (`harness_label`). A harness missing here has no `harness` on its
/// lines; `actor` still names it.
const HARNESSES: [Harness; 10] = [
    Harness::ClaudeCode,
    Harness::Codex,
    Harness::Generic,
    Harness::Pi,
    Harness::Herdr,
    Harness::OpenCode,
    Harness::Cursor,
    Harness::Dsh,
    Harness::Agy,
    Harness::Gemini,
];

/// The kind of a line, from the icon the core chose for it. Two kinds share
/// the cross; the catalog key tells a withdrawn question from a dropped step.
fn kind(item: &ActivityItem) -> ActivityKind {
    let key = item.msg.as_ref().map(|msg| msg.key.as_str());
    match item.icon {
        "bot" => ActivityKind::SessionStarted,
        "refresh-cw" => ActivityKind::SessionBack,
        "pause-circle" => ActivityKind::SessionEnded,
        "target" => ActivityKind::ProjectDescribed,
        "git-branch" => ActivityKind::PlanUpdated,
        "clock" => ActivityKind::Started,
        "check" => ActivityKind::Finished,
        "x" if key == Some("act.withdrew") => ActivityKind::Withdrawn,
        "x" => ActivityKind::Dropped,
        "radio" => ActivityKind::Update,
        "help-circle" => ActivityKind::Asked,
        "user" => ActivityKind::Answered,
        "badge-check" => ActivityKind::Applied,
        "file-text" => ActivityKind::Report,
        _ => ActivityKind::Other,
    }
}

pub(super) fn activity_row(project: &ProjectState, item: &ActivityItem) -> ActivityRow {
    let harness = (item.actor_kind == ActorKind::Agent)
        .then(|| {
            HARNESSES
                .into_iter()
                .find(|harness| harness_label(*harness) == item.actor)
        })
        .flatten();
    ActivityRow {
        at: item.ts.to_string(),
        kind: kind(item),
        text: item.text.clone(),
        msg_json: item
            .msg
            .as_ref()
            .and_then(|msg| serde_json::to_string(msg).ok()),
        detail: item.detail.clone(),
        node: item.node.as_ref().map(|node| node.as_str().to_owned()),
        step_title: item.node.as_ref().map(|node| step_title(project, node)),
        actor: item.actor.clone(),
        harness: harness.map(harness_name),
        by_owner: item.actor_kind == ActorKind::Human,
        events: item.count,
        has_report: item.report.is_some(),
    }
}

pub(super) fn project_stats(stats: &CoreStats) -> ProjectStats {
    ProjectStats {
        agents_running: stats.agents_running,
        agents_idle: stats.agents_idle,
        agents_stalled: stats.agents_stalled,
        finished_24h: stats.finished_24h,
        agent_ms_24h: stats.agent_ms_24h,
        median_step_ms: stats.median_step_ms,
        timed_steps: stats.timed_steps,
        instant_excluded: stats.instant_excluded,
    }
}

pub(super) fn calibration(project: &ProjectState) -> Vec<CalibrationRow> {
    project
        .summary
        .calibration
        .iter()
        .map(|row| CalibrationRow {
            harness: harness_name(row.harness),
            samples: row.samples,
            ratio_milli: row.ratio_milli,
        })
        .collect()
}

pub(super) const fn review_state(status: ReviewStatus) -> ReviewState {
    match status {
        ReviewStatus::Pending => ReviewState::Pending,
        ReviewStatus::Accepted => ReviewState::Accepted,
        ReviewStatus::SentBack => ReviewState::SentBack,
    }
}

/// The Needs-you row that waits on a delivery, while one does.
pub(super) fn review_need<'a>(
    project: &'a ProjectState,
    delivery: &DeliveryState,
) -> Option<&'a NeedsYouItem> {
    project
        .needs_you
        .iter()
        .find(|need| need.kind == NeedsKind::Review && need.source == delivery.id)
}

/// The deliveries the desktop's evidence cards list: those no newer delivery
/// replaced, newest first (ids sort by time).
pub(super) fn delivery_rows(project: &ProjectState) -> Vec<DeliveryRow> {
    project
        .deliveries
        .values()
        .rev()
        .filter(|delivery| delivery.superseded_by.is_none())
        .map(|delivery| DeliveryRow {
            delivery_id: delivery.id.as_str().to_owned(),
            name: delivery.name.clone(),
            node: delivery.node.as_str().to_owned(),
            step_title: step_title(project, &delivery.node),
            review: review_state(delivery.review),
            need_id: review_need(project, delivery).map(|need| need.id.clone()),
            demos: count(delivery.demos.len()),
        })
        .collect()
}
