//! Attention rows, the single runnable predicate and stalled-agent accounting
//!.
//!
//! `decide` rows, the dependency-cycle and start-conflict rescues come from the
//! plan; lost-session and repeated-failure rescues come from the runtime and
//! evidence projections that own their facts.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::Msg;
use crate::model::ids::{EventId, Mode, NodePath, SessionId, UtcTime};
use crate::model::state::{
    DecisionLifecycle, Gate, Liveness, NeedsKind, NeedsYouItem, Progress, ProjectState,
    ReceiptSource, ReviewStatus,
};
use crate::reduce::graph::{RejectedEdge, ancestors, descendants};
use crate::reduce::policy::{risk_scan, weight_x2};

/// Largest score representable exactly by JavaScript numbers.
pub const JS_SAFE_MAX: u64 = 9_007_199_254_740_991;

/// One lost-session rescue input: a host-owned loss fact attached to the
/// feature that session was working on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostSession {
    pub session: SessionId,
    pub lost_event: EventId,
    pub node: Option<NodePath>,
    pub since: UtcTime,
}

/// Extra rescue inputs that are not part of `ProjectState`.
pub struct RescueInputs<'a> {
    pub rejected: &'a [RejectedEdge],
    pub start_conflicts: &'a [(NodePath, EventId, UtcTime)],
    pub lost_sessions: &'a [LostSession],
    pub event_ts: &'a BTreeMap<EventId, UtcTime>,
    pub as_of: UtcTime,
}

/// Accepted upstream → downstream adjacency of a finished project.
pub fn adjacency(project: &ProjectState) -> BTreeMap<NodePath, BTreeSet<NodePath>> {
    let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for feature in project.features.values() {
        for upstream in &feature.accepted_depends_on {
            adjacency
                .entry(upstream.clone())
                .or_default()
                .insert(feature.node.clone());
        }
    }
    adjacency
}

/// Did `session` apply every effective receipt that blocks `node`?
fn receipts_applied(project: &ProjectState, session: &SessionId, node: &NodePath) -> bool {
    unapplied_receipts(project, session, node).is_empty()
}

/// Current human receipts on `node` or its upstream that `session` has not
/// applied yet (by ack, or by a start or progress citing them).
pub fn unapplied_receipts(
    project: &ProjectState,
    session: &SessionId,
    node: &NodePath,
) -> Vec<EventId> {
    let adjacency = adjacency(project);
    let mut relevant: BTreeSet<NodePath> = ancestors(&adjacency, node).into_iter().collect();
    relevant.insert(node.clone());
    let mut pending = Vec::new();
    for decision in project.decisions.values() {
        let Some(receipt) = &decision.effective_receipt else {
            continue;
        };
        if !relevant.contains(&decision.node) {
            continue;
        }
        if decision.lifecycle == DecisionLifecycle::Withdrawn
            || decision.lifecycle == DecisionLifecycle::Defaulted
        {
            continue;
        }
        if project
            .receipts
            .get(receipt)
            .is_some_and(|receipt| receipt.source == ReceiptSource::Assumed)
        {
            continue;
        }
        if !decision
            .applied_sessions
            .iter()
            .any(|applied| applied == session)
        {
            pending.push(receipt.clone());
        }
    }
    pending.sort();
    pending.dedup();
    pending
}

/// The single firm-work predicate shared by asks, Stop, scheduling and ranking.
///
/// No OS or clock fact is read: everything comes from the convergent projection.
pub fn runnable_for(session: &SessionId, node: &NodePath, project: &ProjectState) -> bool {
    runnable(session, node, project, true)
}

/// [`runnable_for`] except for unapplied receipts: a start that cites them
/// applies them, so `progress started --applies` may use
/// this together with [`unapplied_receipts`].
pub fn runnable_except_receipts(
    session: &SessionId,
    node: &NodePath,
    project: &ProjectState,
) -> bool {
    runnable(session, node, project, false)
}

fn runnable(
    session: &SessionId,
    node: &NodePath,
    project: &ProjectState,
    check_receipts: bool,
) -> bool {
    if project.summary.archived {
        return false;
    }
    let Some(session_state) = project.sessions.get(session) else {
        return false;
    };
    if session_state.liveness == Liveness::Lost {
        return false;
    }
    let Some(feature) = project.features.get(node) else {
        return false;
    };
    if !feature.complete {
        return false;
    }
    let terminal = matches!(
        feature.progress,
        Progress::Done | Progress::Verified | Progress::Dropped
    );
    if terminal && !feature.handoff_pending {
        return false;
    }
    // A pending handoff is the *only* gate an eligible replacement may bypass:
    // an independent open ask, a pause or an unresolved upstream still blocks
    // it. Every `waits_on` entry is a real blocker; a
    // handoff gate contributes none of them.
    if feature.paused || !feature.waits_on.is_empty() {
        return false;
    }
    let replacement = feature.handoff_pending
        && feature
            .session
            .as_ref()
            .is_none_or(|owner| owner != session);
    if feature.gate == Gate::AwaitingUpstream {
        return false;
    }
    if feature.gate == Gate::AwaitingDecision && !replacement {
        return false;
    }
    // A handoff bypasses only its own gate. An independent open ask on the same
    // feature still needs a human answer.
    if replacement
        && project.decisions.values().any(|decision| {
            &decision.node == node
                && decision.lifecycle == DecisionLifecycle::Open
                && decision.mode == Mode::Ask
        })
    {
        return false;
    }
    if feature.progress == Progress::Doing
        && !replacement
        && feature.session.as_ref() != Some(session)
    {
        return false;
    }
    if check_receipts && !receipts_applied(project, session, node) {
        return false;
    }
    if !feature.assumptions.is_empty() || !feature.taints.is_empty() {
        return false;
    }
    true
}

/// Lexically sorted runnable nodes for one session.
pub fn runnable_nodes(session: &SessionId, project: &ProjectState) -> Vec<NodePath> {
    let mut nodes: Vec<NodePath> = project
        .features
        .keys()
        .filter(|node| runnable_for(session, node, project))
        .cloned()
        .collect();
    nodes.sort();
    nodes
}

/// Workers classification: decide rows are actionable with stalled agents; every
/// rescue row whose evidence exists is actionable.
pub fn notification_eligible(item: &NeedsYouItem) -> bool {
    match item.kind {
        NeedsKind::Decide => item.stalled_agents > 0,
        NeedsKind::Rescue => true,
        NeedsKind::Confirm | NeedsKind::Review => false,
    }
}

/// Human-facing magnitude of an age in whole minutes.
pub fn age_label(minutes: u64) -> String {
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / (24 * 60))
    }
}

fn unblocks_for(project: &ProjectState, node: &NodePath) -> Vec<NodePath> {
    let adjacency = adjacency(project);
    descendants(&adjacency, node)
        .into_iter()
        .filter(|candidate| {
            project
                .features
                .get(candidate)
                .is_some_and(|feature| feature.progress != Progress::Verified)
        })
        .collect()
}

/// Sessions stalled on one attention row.
fn stalled_on(project: &ProjectState, node: &NodePath) -> Vec<SessionId> {
    let adjacency = adjacency(project);
    let mut sessions = Vec::new();
    for (session, session_state) in &project.sessions {
        if session_state.liveness == Liveness::Lost {
            continue;
        }
        let Some(focus) = &session_state.current_node else {
            continue;
        };
        let Some(feature) = project.features.get(focus) else {
            continue;
        };
        if feature.progress.is_terminal() {
            continue;
        }
        let gated_by_row = focus == node || ancestors(&adjacency, focus).contains(node);
        if !gated_by_row {
            continue;
        }
        if !runnable_nodes(session, project).is_empty() {
            continue;
        }
        sessions.push(session.clone());
    }
    sessions
}

fn stack_score(stalled: u32, waiting_minutes: u64, weight_x2: u8) -> u64 {
    (stalled as u64)
        .saturating_mul(waiting_minutes)
        .saturating_mul(u64::from(weight_x2))
        .min(JS_SAFE_MAX)
}

fn waiting_minutes(since: UtcTime, as_of: UtcTime) -> u64 {
    let elapsed = as_of.millis() - since.millis();
    if elapsed <= 0 {
        0
    } else {
        u64::try_from(elapsed / 60_000).unwrap_or(u64::MAX)
    }
}

/// Project every attention row for a finished project. Scores are derived for
/// every row; the ordering is age-based.
pub fn build_needs(project: &ProjectState, inputs: &RescueInputs<'_>) -> Vec<NeedsYouItem> {
    let mut rows: Vec<NeedsYouItem> = Vec::new();

    for (ask, decision) in &project.decisions {
        if decision.lifecycle != DecisionLifecycle::Open || decision.mode != Mode::Ask {
            continue;
        }
        let (risk, external, _) = risk_scan(&decision.card);
        let weight = weight_x2(risk, external);
        let stalled = stalled_on(project, &decision.node);
        let since = inputs.event_ts.get(ask).copied().unwrap_or(inputs.as_of);
        let minutes = waiting_minutes(since, inputs.as_of);
        let (reason, reason_msg) = reason_text(stalled.len() as u32, minutes);
        rows.push(NeedsYouItem {
            id: format!("decide:{}", ask.as_str()),
            kind: NeedsKind::Decide,
            project: project.summary.slug.clone(),
            node: Some(decision.node.clone()),
            source: ask.clone(),
            question: decision.card.question.clone(),
            question_msg: None,
            since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: unblocks_for(project, &decision.node),
        });
    }

    // An unconfirmed assumption becomes a confirm row once enough features are
    // built on it or it is old enough. A tainted
    // choice is rework, not a confirmation, and an answered one is history.
    for (ask, decision) in &project.decisions {
        if decision.lifecycle != DecisionLifecycle::Open || decision.mode != Mode::Assume {
            continue;
        }
        let Some(effective) = &decision.effective_receipt else {
            continue;
        };
        if project
            .receipts
            .get(effective)
            .is_none_or(|receipt| receipt.source != ReceiptSource::Assumed)
        {
            continue;
        }
        let assumed_at = decision
            .assumed_at
            .or_else(|| inputs.event_ts.get(ask).copied())
            .unwrap_or(inputs.as_of);
        let age_s = (inputs.as_of.millis() - assumed_at.millis()).max(0) / 1000;
        let built = decision.built_on.len();
        if built < crate::reduce::assumptions::CONFIRM_BUILT_ON
            && age_s < crate::reduce::assumptions::CONFIRM_AGE_S
        {
            continue;
        }
        let (risk, external, _) = risk_scan(&decision.card);
        let weight = weight_x2(risk, external);
        let stalled = stalled_on(project, &decision.node);
        let minutes = waiting_minutes(assumed_at, inputs.as_of);
        let (reason, reason_msg) = if stalled.is_empty() {
            if built == 0 {
                (
                    format!("Assumed {}", age_label(minutes)),
                    Msg::new("need.reason.assumed").minutes("age", minutes),
                )
            } else {
                (
                    format!("{built} steps built on it"),
                    Msg::new("need.reason.built_on").count("n", built),
                )
            }
        } else {
            reason_text(stalled.len() as u32, minutes)
        };
        rows.push(NeedsYouItem {
            id: format!("confirm:{}", ask.as_str()),
            kind: NeedsKind::Confirm,
            project: project.summary.slug.clone(),
            node: Some(decision.node.clone()),
            source: ask.clone(),
            question: decision.card.question.clone(),
            question_msg: None,
            since: assumed_at,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: unblocks_for(project, &decision.node),
        });
    }

    for edge in inputs.rejected {
        let source_ts = inputs
            .event_ts
            .get(&edge.source_event)
            .copied()
            .unwrap_or(inputs.as_of);
        let weight = 2_u8;
        let stalled = stalled_on(project, &edge.downstream);
        let minutes = waiting_minutes(source_ts, inputs.as_of);
        let (reason, reason_msg) = reason_text(stalled.len() as u32, minutes);
        rows.push(NeedsYouItem {
            id: format!(
                "rescue:cycle:{}:{}:{}",
                edge.source_event.as_str(),
                edge.upstream.as_str(),
                edge.downstream.as_str()
            ),
            kind: NeedsKind::Rescue,
            project: project.summary.slug.clone(),
            node: Some(edge.downstream.clone()),
            source: edge.source_event.clone(),
            question: format!(
                "Remove {} → {} to repair the plan?",
                edge.upstream.as_str(),
                edge.downstream.as_str()
            ),
            question_msg: Some(
                Msg::new("need.q.cycle")
                    .text("from", edge.upstream.as_str())
                    .text("to", edge.downstream.as_str()),
            ),
            since: source_ts,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: unblocks_for(project, &edge.downstream),
        });
    }

    for lost in inputs.lost_sessions {
        let Some(node) = &lost.node else {
            // A node-less idle loss has no actionable rescue.
            continue;
        };
        let Some(feature) = project.features.get(node) else {
            continue;
        };
        if feature.liveness != Liveness::Lost
            || feature.progress.is_terminal()
            || feature.handoff_pending
            || feature.handoff_consumed_by.is_some()
            || feature.paused
            || feature.session.as_ref() != Some(&lost.session)
        {
            continue;
        }
        let weight = 2_u8;
        let stalled = stalled_on(project, node);
        let minutes = waiting_minutes(lost.since, inputs.as_of);
        rows.push(NeedsYouItem {
            id: format!("rescue:lost:{}", lost.lost_event.as_str()),
            kind: NeedsKind::Rescue,
            project: project.summary.slug.clone(),
            node: Some(node.clone()),
            source: lost.lost_event.clone(),
            question: format!("Hand {} to the next session?", node.as_str()),
            question_msg: Some(Msg::new("need.q.handoff").text("node", node.as_str())),
            since: lost.since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason: format!("Agent stopped · {}", age_label(minutes)),
            reason_msg: Some(Msg::new("need.reason.stopped").minutes("age", minutes)),
            unblocks: unblocks_for(project, node),
        });
    }

    for (node, attempt, since) in inputs.start_conflicts {
        let weight = 2_u8;
        let stalled = stalled_on(project, node);
        let minutes = waiting_minutes(*since, inputs.as_of);
        let (reason, reason_msg) = reason_text(stalled.len() as u32, minutes);
        rows.push(NeedsYouItem {
            id: format!("rescue:start-conflict:{}", attempt.as_str()),
            kind: NeedsKind::Rescue,
            project: project.summary.slug.clone(),
            node: Some(node.clone()),
            source: attempt.clone(),
            question: format!("Who should continue {}?", node.as_str()),
            question_msg: Some(Msg::new("need.q.conflict").text("node", node.as_str())),
            since: *since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: unblocks_for(project, node),
        });
    }

    // A delivery whose review is still pending is a human review row. It is
    // visible even with zero stalled agents and never creates
    // a notification (`notification_eligible` returns false for Review).
    for delivery in project.deliveries.values() {
        if delivery.superseded_by.is_some() || delivery.review != ReviewStatus::Pending {
            continue;
        }
        let weight = 2_u8;
        let stalled = stalled_on(project, &delivery.node);
        let since = inputs
            .event_ts
            .get(&delivery.id)
            .copied()
            .unwrap_or(inputs.as_of);
        let minutes = waiting_minutes(since, inputs.as_of);
        let (reason, reason_msg) = if stalled.is_empty() {
            (
                format!("Ready for review · {}", age_label(minutes)),
                Msg::new("need.reason.review").minutes("age", minutes),
            )
        } else {
            reason_text(stalled.len() as u32, minutes)
        };
        rows.push(NeedsYouItem {
            id: format!("review:{}", delivery.id.as_str()),
            kind: NeedsKind::Review,
            project: project.summary.slug.clone(),
            node: Some(delivery.node.clone()),
            source: delivery.id.clone(),
            question: format!("Review {}: does it do what you asked?", delivery.name),
            question_msg: Some(Msg::new("need.q.review").text("name", delivery.name.clone())),
            since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: unblocks_for(project, &delivery.node),
        });
    }

    // Full cost-of-delay ranking: score desc, older first, then
    // the stable item ID.
    crate::reduce::ordering::sort_needs(&mut rows);
    rows
}

fn reason_text(stalled: u32, minutes: u64) -> (String, Msg) {
    let age = age_label(minutes);
    if stalled > 0 {
        let suffix = if stalled == 1 { "" } else { "s" };
        (
            format!("{stalled} agent{suffix} waiting · {age}"),
            Msg::new("need.reason.agents")
                .count("n", stalled)
                .minutes("age", minutes),
        )
    } else {
        (
            format!("Waiting for you · {age}"),
            Msg::new("need.reason.you").minutes("age", minutes),
        )
    }
}

impl crate::model::state::DecisionState {
    // No impl body: the projection deliberately holds no local time facts.
}

/// Distinct non-lost sessions stalled across all rows, for fleet-level counts.
pub fn fleet_stalled(rows: &[NeedsYouItem], project: &ProjectState) -> u32 {
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    for row in rows {
        if let Some(node) = &row.node {
            sessions.extend(stalled_on(project, node));
        }
    }
    sessions.len() as u32
}

/// Sessions stalled on each row, for `SessionState.stalled_on`.
pub fn stalled_by_row(
    rows: &[NeedsYouItem],
    project: &ProjectState,
) -> BTreeMap<EventId, Vec<SessionId>> {
    let mut map = BTreeMap::new();
    for row in rows {
        if let Some(node) = &row.node {
            map.insert(row.source.clone(), stalled_on(project, node));
        }
    }
    map
}

/// Whether anyone will hear an answer to an unacked question right away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Listening {
    /// A local `agendar wait` is blocked on this question now.
    Waiting,
    /// An agent is connected; it reads the answer at its next inbox or wait.
    Connected,
    /// No agent is connected; the answer waits in the inbox.
    Away,
}

/// Listening state of every question in `project` that is not acked yet.
/// `waiting` holds the asks a fresh local wait lease covers.
pub fn listening(
    project: &ProjectState,
    waiting: &BTreeSet<EventId>,
) -> BTreeMap<EventId, Listening> {
    let live = |session: &SessionId| {
        project
            .sessions
            .get(session)
            .is_some_and(|state| state.liveness == Liveness::Live)
    };
    let any_live = project.sessions.keys().any(live);
    project
        .decisions
        .values()
        .filter(|decision| {
            matches!(
                decision.lifecycle,
                DecisionLifecycle::Open | DecisionLifecycle::Answered
            ) && decision.withdrawn_by.is_none()
                && decision.superseded_by.is_none()
        })
        .map(|decision| {
            let state = if waiting.contains(&decision.id) {
                Listening::Waiting
            } else if project
                .features
                .get(&decision.node)
                .and_then(|feature| feature.session.as_ref())
                .is_some_and(live)
                || any_live
            {
                Listening::Connected
            } else {
                Listening::Away
            };
            (decision.id.clone(), state)
        })
        .collect()
}
