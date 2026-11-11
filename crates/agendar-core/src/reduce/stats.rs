//! Project pace for the project header: how many agents run now and how fast
//! steps get finished in wall time. A host-local projection over recent events,
//! like the activity feed; never part of canonical `State` or `state export`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::event::{Event, EventPayload, WorkStatus};
use crate::model::ids::{EventId, UtcTime};
use crate::model::state::ProjectState;
use crate::reduce::calibration::MIN_SAMPLE_MS;

const DAY_MS: i64 = 86_400_000;

/// Header statistics of one project at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectStats {
    /// Sessions working now: a live agent with a step in progress.
    pub agents_running: u32,
    /// Connected sessions with nothing in progress (idle at their prompt).
    pub agents_idle: u32,
    /// Running sessions that have nothing they can do until you answer.
    pub agents_stalled: u32,
    /// Steps finished in the last 24 hours (timed completions only).
    pub finished_24h: u32,
    /// Wall time of the steps finished in the last 24 hours, start to finish.
    pub agent_ms_24h: u64,
    /// Median wall time of a finished step in the window, when any was timed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_step_ms: Option<u64>,
    /// Finished steps the median rests on.
    pub timed_steps: u32,
    /// Completions under a minute left out (imports, after-the-fact reports).
    pub instant_excluded: u32,
    /// Tokens the project's agents used in the last 24 hours, read from their
    /// local logs by the host; absent when no log matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<crate::runtime::usage::TokenUsage>,
    /// When the whole project finishes, from the open steps and recent pace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast: Option<crate::reduce::forecast::Forecast>,
}

/// Build the header statistics from the project's recent events (oldest
/// first). Each attempt counts once, at its first completion; wall time is
/// start to completion and includes waits, because that is what the human
/// experienced.
pub fn build(project: &ProjectState, events: &[Event], now: UtcTime) -> ProjectStats {
    let mut started: BTreeMap<EventId, i64> = BTreeMap::new();
    let mut durations: Vec<u64> = Vec::new();
    let mut instant = 0_u32;
    let mut finished_24h = 0_u32;
    let mut agent_ms_24h = 0_u64;
    let mut record = |start: i64, at: UtcTime| {
        let elapsed = u64::try_from(at.millis() - start).unwrap_or(0);
        if elapsed < MIN_SAMPLE_MS {
            instant += 1;
            return;
        }
        durations.push(elapsed);
        if now.millis() - at.millis() <= DAY_MS {
            finished_24h += 1;
            agent_ms_24h = agent_ms_24h.saturating_add(elapsed);
        }
    };
    // An attempt the owner dropped is not finished work, whatever its agent
    // reported, also once the step was started again. The reducer's rule
    // (`FeatureFold::settle_owner_drops`), read from the events at hand: a
    // start that does not cite a drop of its step is closed by it, unless a
    // start that does not cite that drop either had replaced it. The service
    // writes every drop it has seen into a start's `causes`.
    let starts: Vec<&Event> = events
        .iter()
        .filter(|event| {
            matches!(&event.payload, EventPayload::WorkProgress(body) if body.status == WorkStatus::Started)
        })
        .collect();
    let owner_dropped: std::collections::BTreeSet<&EventId> = events
        .iter()
        .filter(|event| matches!(event.payload, EventPayload::PlanDropped(_)))
        .flat_map(|drop| {
            let closed = |start: &&&Event| {
                start.project == drop.project
                    && start.refs.node == drop.refs.node
                    && !start.refs.causes.contains(&drop.id)
            };
            starts
                .iter()
                .filter(closed)
                .filter(|start| {
                    !starts
                        .iter()
                        .any(|later| closed(&later) && later.refs.causes.contains(&start.id))
                })
                .map(|start| &start.id)
                .collect::<Vec<_>>()
        })
        .collect();
    for event in events {
        let EventPayload::WorkProgress(body) = &event.payload else {
            continue;
        };
        match body.status {
            WorkStatus::Started => {
                started.insert(event.id.clone(), event.ts.millis());
            }
            WorkStatus::Done | WorkStatus::Verified => {
                let Some(attempt) = event.refs.attempt.as_ref() else {
                    continue;
                };
                if owner_dropped.contains(attempt) {
                    continue;
                }
                // First completion only: a later "verified" is not new work.
                let Some(start) = started.remove(attempt) else {
                    continue;
                };
                record(start, event.ts);
            }
            WorkStatus::Dropped => {
                started.remove(event.refs.attempt.as_ref().unwrap_or(&event.id));
            }
        }
    }
    // A step the owner's accept finished, with no done from its agent, ends at
    // the delivery that accept rests on.
    for feature in project.features.values() {
        if feature.progress != crate::model::state::Progress::Done {
            continue;
        }
        let Some(attempt) = feature.attempt.as_ref() else {
            continue;
        };
        if !crate::reduce::settle::accepted_by_owner(project, attempt) {
            continue;
        }
        let Some(start) = started.remove(attempt) else {
            continue;
        };
        let delivered = events
            .iter()
            .filter(|event| matches!(event.payload, EventPayload::Delivery(_)))
            .filter(|event| {
                project.deliveries.get(&event.id).is_some_and(|delivery| {
                    &delivery.attempt == attempt && delivery.superseded_by.is_none()
                })
            })
            .map(|event| event.ts)
            .max();
        if let Some(at) = delivered {
            record(start, at);
        }
    }
    durations.sort_unstable();
    let median_step_ms = match durations.len() {
        0 => None,
        n if n % 2 == 1 => Some(durations[n / 2]),
        n => Some((durations[n / 2 - 1] + durations[n / 2]) / 2),
    };
    let working: std::collections::BTreeSet<_> = project
        .features
        .values()
        .filter(|feature| {
            feature.progress == crate::model::state::Progress::Doing
                && feature.liveness == crate::model::state::Liveness::Live
                && feature.phase != Some(crate::model::state::Phase::InReview)
        })
        .filter_map(|feature| feature.session.as_ref())
        .collect();
    let agents_running = u32::try_from(working.len()).unwrap_or(u32::MAX);
    ProjectStats {
        agents_running,
        agents_idle: project.summary.active_agents.saturating_sub(agents_running),
        agents_stalled: project.summary.stalled_agents,
        finished_24h,
        agent_ms_24h,
        median_step_ms,
        timed_steps: u32::try_from(durations.len()).unwrap_or(u32::MAX),
        instant_excluded: instant,
        tokens: None,
        forecast: crate::reduce::forecast::forecast(project, &durations, now),
    }
}
