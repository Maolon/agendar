//! Contract v1 reads: `refresh`, `now`, `projects`, `project`.
//!
//! Only `refresh` reads the Vault. The other calls reduce the cached index at
//! the current time, so they show what the last refresh read. Order and
//! filters are the desktop's: active needs by the core's cost-of-delay key,
//! projects by the core's project key, archived projects left out.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use agendar_core::guard::{guard_for, need_revision};
use agendar_core::index::ingest::SOURCE_WAITING;
use agendar_core::index::query::{Query, QueryResult, query, recent_project_events};
use agendar_core::model::{
    DecisionLifecycle, FeatureState, Gate, HostId, Lease, Msg, NeedsKind, NeedsYouItem, NodePath,
    Phase, Progress, ProjectSlug, ProjectState, State, TimeRange, UtcTime,
};
use agendar_core::reduce::activity::{self, ACTIVITY_EVENT_WINDOW};
use agendar_core::reduce::attention::is_active_need;
use agendar_core::reduce::forecast::Forecast;
use agendar_core::reduce::motion::{self, Motion, Stall};
use agendar_core::reduce::{brief, ordering, stats};
use agendar_core::service::Fresh;
use agendar_core::{AgendarError, ErrorCode};

use super::activity::{activity_row, calibration, delivery_rows, project_stats};
use super::shared::{count, harness_name, project_slug, step_title, utc_time};
use super::{
    AgentRow, DoneRow, FfiError, Freshness, Guard, HostSeen, NeedKind, NeedRow, NowBoard,
    ProjectForecast, ProjectPage, ProjectRow, Session, StallReason, StepGate, StepPhase,
    StepProgress, StepRow, TimeWindow,
};

impl Session {
    /// Converge the index with the Vault and report what this device has read.
    /// `deadline_ms` bounds the wait for the index locks, not the ingest;
    /// past it the call is `Refused(RefreshTimeout)`.
    pub fn refresh(&self, deadline_ms: u32) -> Result<Freshness, FfiError> {
        let fresh = self.bounded_refresh(deadline_ms)?;
        self.freshness(&fresh)
    }

    /// The Now screen, from the cached index. `looked_at` is when the owner
    /// last looked; `done_since` is empty without it.
    pub fn now(&self, looked_at: Option<String>) -> Result<NowBoard, FfiError> {
        let since = looked_at
            .as_deref()
            .map(|at| utc_time("/looked_at", at))
            .transpose()?;
        let fresh = self.cached()?;
        let projects = ordered_projects(&fresh.state);

        let mut needs: Vec<(&ProjectState, &NeedsYouItem)> = projects
            .iter()
            .flat_map(|project| active_needs(project).map(move |need| (*project, need)))
            .collect();
        needs.sort_by_key(|(_, need)| ordering::need_key(need));

        let mut moving = Vec::new();
        let mut stalled = Vec::new();
        for project in &projects {
            agent_rows(project, &mut moving, &mut stalled);
        }
        // A snoozed need leaves `needs` and is listed in `snoozed`, in the
        // same order.
        let (snoozed, needs): (Vec<NeedRow>, Vec<NeedRow>) = needs
            .into_iter()
            .map(|(project, need)| need_row(project, need))
            .partition(|row| row.snoozed_until.is_some());
        Ok(NowBoard {
            needs,
            snoozed,
            moving,
            stalled,
            done_since: since.map_or_else(Vec::new, |since| done_since(&fresh, since)),
        })
    }

    /// Every project that is not archived, most urgent first.
    pub fn projects(&self) -> Result<Vec<ProjectRow>, FfiError> {
        let fresh = self.cached()?;
        Ok(ordered_projects(&fresh.state)
            .into_iter()
            .map(project_row)
            .collect())
    }

    /// One project's page.
    pub fn project(&self, slug: String) -> Result<ProjectPage, FfiError> {
        let slug = project_slug("/slug", &slug)?;
        let fresh = self.cached()?;
        let project = self.service.project(&fresh, &slug)?;
        // The desktop's forecast: the statistics of the same recent events.
        let events =
            recent_project_events(&fresh.index, &slug, ACTIVITY_EVENT_WINDOW).unwrap_or_default();
        let stats = stats::build(project, &events, fresh.now);
        let mut needs: Vec<&NeedsYouItem> = active_needs(project).collect();
        needs.sort_by_key(|need| ordering::need_key(need));
        Ok(ProjectPage {
            row: project_row(project),
            goal: project.summary.goal.clone(),
            brief: project.summary.brief.clone(),
            steps: plan_order(&project.features)
                .into_iter()
                .map(step_row)
                .collect(),
            forecast: stats.forecast.as_ref().map(project_forecast),
            needs: needs
                .into_iter()
                .map(|need| need_row(project, need))
                .collect(),
            // The desktop's feed: the same events, the same builder.
            activity: activity::build(project, &events)
                .iter()
                .map(|item| activity_row(project, item))
                .collect(),
            stats: project_stats(&stats),
            calibration: calibration(project),
            deliveries: delivery_rows(project),
        })
    }

    /// What one read holds: its digest and size, the newest event read from
    /// each host, and the writer files the core knows are not on this device.
    fn freshness(&self, fresh: &Fresh) -> Result<Freshness, FfiError> {
        let QueryResult::Health {
            event_set_digest,
            diagnostics,
            ..
        } = query(&fresh.index, Query::Health, fresh.now)?
        else {
            return Err(AgendarError::new(
                ErrorCode::Index,
                "The health query returned no health.",
            )
            .into());
        };
        let this_host = self.service.host();
        let mut newest: BTreeMap<&HostId, UtcTime> = BTreeMap::new();
        for project in fresh.state.projects.values() {
            for (host, at) in &project.summary.host_as_of {
                let seen = newest.entry(host).or_insert(*at);
                *seen = (*seen).max(*at);
            }
        }
        let mut hosts: Vec<(&HostId, UtcTime)> = newest.into_iter().collect();
        hosts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        Ok(Freshness {
            evaluated_at: fresh.state.evaluated_at.to_string(),
            event_set_digest: event_set_digest.map(|digest| digest.as_str().to_owned()),
            generation: fresh.generation,
            events: count(fresh.events.len()),
            this_host: this_host.as_str().to_owned(),
            hosts: hosts
                .into_iter()
                .map(|(host, at)| HostSeen {
                    host: host.as_str().to_owned(),
                    last_event_at: at.to_string(),
                    is_self: host == this_host,
                })
                .collect(),
            waiting: diagnostics
                .into_iter()
                .filter(|note| note.code == SOURCE_WAITING)
                .map(|note| note.path)
                .collect(),
        })
    }
}

/// The rows of the active Needs-you list of one project, in stored order.
fn active_needs(project: &ProjectState) -> impl Iterator<Item = &NeedsYouItem> {
    project.needs_you.iter().filter(|need| is_active_need(need))
}

/// Projects that are not archived, in the core's project order. The earliest
/// need is the earliest active one, as in the desktop sidebar and the brief.
fn ordered_projects(state: &State) -> Vec<&ProjectState> {
    let needs: Vec<NeedsYouItem> = state
        .projects
        .values()
        .flat_map(active_needs)
        .cloned()
        .collect();
    let earliest = ordering::earliest_needs(&needs);
    let mut projects: Vec<&ProjectState> = state
        .projects
        .values()
        .filter(|project| !project.summary.archived)
        .collect();
    projects.sort_by_key(|project| {
        let summary = &project.summary;
        ordering::project_key(summary, earliest.get(&summary.slug).copied())
    });
    projects
}

fn project_row(project: &ProjectState) -> ProjectRow {
    let summary = &project.summary;
    // The same rule as the lists of `now`, not the summary's agents.
    let (moving, stalled) = motion::counts(project);
    ProjectRow {
        slug: summary.slug.as_str().to_owned(),
        title: summary.title.clone(),
        glyph: summary.glyph.clone(),
        needs: summary.needs_count,
        moving,
        stalled,
        stopped_steps: summary.stopped_steps,
        last_event: summary.last_event.as_ref().map(ToString::to_string),
    }
}

fn msg_json(msg: Option<&Msg>) -> Option<String> {
    msg.and_then(|msg| serde_json::to_string(msg).ok())
}

/// One Needs-you row with the revision an answer or a rescue passes back.
pub(super) fn need_row(project: &ProjectState, need: &NeedsYouItem) -> NeedRow {
    let decision = project.decisions.get(&need.source);
    let question = decision.filter(|_| need.kind == NeedsKind::Decide);
    // As on the desktop: a default is shown only while the question is open
    // and the project lets questions time out.
    let default_at = question
        .filter(|decision| {
            decision.lifecycle == DecisionLifecycle::Open && project.policy.value.allow_timeouts
        })
        .and_then(|decision| decision.default_at);
    NeedRow {
        id: need.id.clone(),
        kind: match need.kind {
            NeedsKind::Decide => NeedKind::Decide,
            NeedsKind::Rescue => NeedKind::Rescue,
            NeedsKind::Confirm => NeedKind::Confirm,
            NeedsKind::Review => NeedKind::Review,
        },
        project: need.project.as_str().to_owned(),
        project_title: project.summary.title.clone(),
        node: need.node.as_ref().map(|node| node.as_str().to_owned()),
        step_title: need.node.as_ref().map(|node| step_title(project, node)),
        source: need.source.as_str().to_owned(),
        question: need.question.clone(),
        question_msg_json: msg_json(need.question_msg.as_ref()),
        reason: need.reason.clone(),
        reason_msg_json: msg_json(need.reason_msg.as_ref()),
        since: need.since.to_string(),
        waiting_minutes: need.waiting_minutes,
        stalled_agents: need.stalled_agents,
        default_at: default_at.as_ref().map(ToString::to_string),
        revision: need_revision(need, decision, &project.policy.revision)
            .as_str()
            .to_owned(),
        guard: question.map(|decision| match guard_for(&decision.card) {
            agendar_core::guard::Guard::Easy => Guard::Easy,
            agendar_core::guard::Guard::Choose => Guard::Choose,
            agendar_core::guard::Guard::Confirm => Guard::Confirm,
        }),
        snoozed_until: need.snoozed_until.as_ref().map(ToString::to_string),
    }
}

/// The sessions of one project, each in at most one list, by the core's
/// display rule (`reduce::motion`). Stalled: a Needs-you row stalls the
/// session, since the oldest such row; or the owner sent its step back and
/// nobody is on it, since the send-back. Moving: a live session on a step in
/// progress it holds, since that step's start (since the send-back while it
/// is sent back). A session whose steps are all finished or with the owner
/// for review is in neither.
fn agent_rows(project: &ProjectState, moving: &mut Vec<AgentRow>, stalled: &mut Vec<AgentRow>) {
    for session in project.sessions.values() {
        let row = |step: Option<&FeatureState>, since: UtcTime, stall| AgentRow {
            session: session.id.as_str().to_owned(),
            project: project.summary.slug.as_str().to_owned(),
            step_title: step.map(|feature| feature.title.clone()),
            harness: harness_name(session.harness),
            since: since.to_string(),
            stall,
        };
        match motion::of_session(project, session) {
            Motion::Stalled { step, since, why } => {
                let why = match why {
                    Stall::NeedsYou => StallReason::NeedsYou,
                    Stall::SentBack => StallReason::SentBack,
                };
                stalled.push(row(step, since, Some(why)));
            }
            Motion::Moving { step, since } => moving.push(row(Some(step), since, None)),
            Motion::Neither => {}
        }
    }
}

/// Accepted deliveries after `since`, newest first (the brief's Delivered
/// section), without those of archived projects.
fn done_since(fresh: &Fresh, since: UtcTime) -> Vec<DoneRow> {
    let delivered = brief::build(&fresh.state, &fresh.events, Some(since)).delivered;
    delivered
        .into_iter()
        .filter_map(|row| {
            let slug = ProjectSlug::parse(&row.target.id).ok()?;
            let project = fresh.state.projects.get(&slug)?;
            if project.summary.archived {
                return None;
            }
            let delivery = row.source_ids.first()?;
            let at = fresh.events.iter().find(|event| &event.id == delivery)?.ts;
            Some(DoneRow {
                id: row.id,
                text: row.text,
                project: row.target.id,
                at: at.to_string(),
            })
        })
        .collect()
}

/// Steps in the desktop's plan order (`planSteps` in `app/src/state/project.ts`):
/// upstream dependencies first, node order inside a depth, a cycle kept at
/// the depth where it was met instead of recursing forever.
fn plan_order(features: &BTreeMap<NodePath, FeatureState>) -> Vec<&FeatureState> {
    fn depth_of<'a>(
        node: &'a NodePath,
        features: &'a BTreeMap<NodePath, FeatureState>,
        depths: &mut BTreeMap<&'a NodePath, usize>,
        visiting: &mut BTreeSet<&'a NodePath>,
    ) -> usize {
        if let Some(depth) = depths.get(node) {
            return *depth;
        }
        let Some(feature) = features.get(node) else {
            return 0;
        };
        if !visiting.insert(node) {
            return 0;
        }
        let mut depth = 0;
        for dependency in &feature.depends_on {
            if dependency != node && features.contains_key(dependency) {
                depth = depth.max(depth_of(dependency, features, depths, visiting) + 1);
            }
        }
        visiting.remove(node);
        depths.insert(node, depth);
        depth
    }

    let mut depths = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    let mut steps: Vec<(usize, &FeatureState)> = features
        .iter()
        .map(|(node, feature)| {
            (
                depth_of(node, features, &mut depths, &mut visiting),
                feature,
            )
        })
        .collect();
    // Stable: node order inside a depth.
    steps.sort_by_key(|(depth, _)| *depth);
    steps.into_iter().map(|(_, feature)| feature).collect()
}

fn time_window(range: &TimeRange) -> TimeWindow {
    TimeWindow {
        earliest: range.earliest.to_string(),
        latest: range.latest.to_string(),
    }
}

fn step_row(feature: &FeatureState) -> StepRow {
    // A claim that ended has no holder; without a claim (a step in review,
    // paused or handed off) only a step in progress keeps its session.
    let held = match &feature.claim {
        Some(claim) => claim.state != Lease::Free,
        None => feature.progress == Progress::Doing,
    };
    StepRow {
        node: feature.node.as_str().to_owned(),
        title: feature.title.clone(),
        progress: match feature.progress {
            Progress::Planned => StepProgress::Planned,
            Progress::Doing => StepProgress::Doing,
            Progress::Done => StepProgress::Done,
            Progress::Verified => StepProgress::Verified,
            Progress::Dropped => StepProgress::Dropped,
        },
        milestone: feature.milestone.clone(),
        paused: feature.paused,
        dropped_by_owner: feature.owner_drop.is_some(),
        holder: feature
            .session
            .as_ref()
            .filter(|_| held)
            .map(|session| session.as_str().to_owned()),
        // The same window as the desktop: history-adjusted when calibrated.
        expected_finish: feature
            .eta
            .as_ref()
            .map(|eta| time_window(eta.likely_finish.as_ref().unwrap_or(&eta.agent_finish))),
        phase: feature.phase.map(|phase| match phase {
            Phase::InReview => StepPhase::InReview,
            Phase::SentBack => StepPhase::SentBack,
        }),
        gate: match feature.gate {
            Gate::Open => StepGate::Open,
            Gate::AwaitingDecision => StepGate::AwaitingDecision,
            Gate::AwaitingUpstream => StepGate::AwaitingUpstream,
        },
        deadline: feature.deadline.as_ref().map(ToString::to_string),
        priority: feature.priority,
        depends_on: feature
            .depends_on
            .iter()
            .map(|node| node.as_str().to_owned())
            .collect(),
        late_by_s: feature.eta.as_ref().map_or(0, |eta| eta.late_by_s),
    }
}

fn project_forecast(forecast: &Forecast) -> ProjectForecast {
    ProjectForecast {
        open_steps: forecast.open_steps,
        in_progress: forecast.in_progress,
        abandoned: forecast.abandoned,
        not_started: forecast.not_started,
        agents: forecast.agents,
        work_low_ms: forecast.work_ms[0],
        work_high_ms: forecast.work_ms[1],
        finish: forecast.finish.as_ref().map(time_window),
        basis_steps: forecast.basis_steps,
    }
}
