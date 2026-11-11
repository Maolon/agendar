//! Attention rows, the single runnable predicate and stalled-agent accounting
//!.
//!
//! `decide` rows, the dependency-cycle and start-conflict rescues come from the
//! plan; lost-session and repeated-failure rescues come from the runtime and
//! evidence projections that own their facts.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::Msg;
use crate::model::ids::{EventId, Mode, NodePath, SessionId, UtcTime};
use crate::model::state::{
    DecisionLifecycle, Gate, Lease, Liveness, NeedsKind, NeedsYouItem, Progress, ProjectState,
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
    /// Attempts whose fold status is dropped.
    pub dropped_attempts: &'a BTreeSet<EventId>,
    /// When each closed step closed (`reduce::moot`).
    pub closed_steps: &'a BTreeMap<NodePath, UtcTime>,
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
    let takeover = free_for(session, feature);
    let granted = granted_to(session, feature).is_some();
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
    // A free step (its claim ended) is open to any agent without a hand-off,
    // but never past a question on it: that is the owner's hold, not the
    // agent's claim.
    if (takeover || granted)
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
        && !takeover
        && !granted
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

/// Whether `session` may take over `feature` without a hand-off: another
/// session's attempt is in progress and its claim is free (the session was
/// lost, or no word came for a day after its ETA). A held or stalled claim,
/// and a step the owner holds (in review, paused, handed off: no claim), are
/// never taken this way. The start still cites the attempt it replaces.
pub fn free_for(session: &SessionId, feature: &crate::model::state::FeatureState) -> bool {
    feature.progress == Progress::Doing
        && !feature.handoff_pending
        && !feature.paused
        && feature.phase != Some(crate::model::state::Phase::InReview)
        && feature
            .session
            .as_ref()
            .is_some_and(|owner| owner != session)
        && feature
            .claim
            .as_ref()
            .is_some_and(|claim| claim.state == Lease::Free)
}

/// The owner's answer that lets `session` continue `feature` while another
/// session's attempt on it is in progress: the session asked on the step and
/// the owner's current answer chose the `takeover` option. It passes the
/// other session's claim and nothing else: as for a free step, a pause, an
/// old hand-off and a delivery in review are the owner's, and a finished or
/// dropped step is not in progress. (`runnable` also keeps an open question
/// and an unresolved upstream in its way.)
pub fn granted_to<'a>(
    session: &SessionId,
    feature: &'a crate::model::state::FeatureState,
) -> Option<&'a crate::model::state::TakeoverGrant> {
    if feature.progress != Progress::Doing
        || feature.handoff_pending
        || feature.paused
        || feature.phase == Some(crate::model::state::Phase::InReview)
        || feature
            .session
            .as_ref()
            .is_none_or(|owner| owner == session)
    {
        return None;
    }
    feature
        .takeover_grants
        .iter()
        .find(|grant| &grant.session == session)
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

/// Steps `session` may start only by applying answers it has not applied
/// yet, with those answers: `progress started --applies <answer>` admits them
/// (the start cites the answer, which is what applying it means). This is how
/// a step the owner handed off after its question was answered, or a free
/// step in that state, reaches the next agent: whoever continues applies the
/// answer. Sorted by node.
pub fn runnable_applying(
    session: &SessionId,
    project: &ProjectState,
) -> Vec<(NodePath, Vec<EventId>)> {
    project
        .features
        .keys()
        .filter(|node| runnable_except_receipts(session, node, project))
        .filter_map(|node| {
            let unapplied = unapplied_receipts(project, session, node);
            (!unapplied.is_empty()).then(|| (node.clone(), unapplied))
        })
        .collect()
}

/// Row id prefix of the step a lost session was on.
pub const LOST_ROW: &str = "rescue:lost:";
/// Row id prefix of a step of a lost session other than the one it was on.
pub const LOST_STEP_ROW: &str = "rescue:lost-step:";
/// Row id prefix of a claim that expired by silence.
pub const SILENT_ROW: &str = "rescue:silent:";

/// A row the board raised by correcting itself: a free step, open to any
/// agent. It is information, not a request: it stays addressable (the owner
/// may still pause it) but is never counted
/// in the active Needs-you list and never announced.
pub fn is_correction_row(item: &NeedsYouItem) -> bool {
    item.kind == NeedsKind::Rescue
        && (item.id.starts_with(LOST_ROW)
            || item.id.starts_with(LOST_STEP_ROW)
            || item.id.starts_with(SILENT_ROW))
}

/// A question on a step closed a day ago: folded "On closed steps".
pub fn is_folded(item: &NeedsYouItem) -> bool {
    item.closed_step
        .as_ref()
        .is_some_and(|closed| closed.folded)
}

/// A row in the active Needs-you list: everything but information rows and
/// folded questions.
pub fn is_active_need(item: &NeedsYouItem) -> bool {
    !is_correction_row(item) && !is_folded(item)
}

/// Workers classification: decide rows are actionable with stalled agents; every
/// rescue row whose evidence exists is actionable.
pub fn notification_eligible(item: &NeedsYouItem) -> bool {
    match item.kind {
        NeedsKind::Decide => item.stalled_agents > 0 && !is_folded(item),
        NeedsKind::Rescue => !is_correction_row(item),
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

/// Per-project view for stalled-agent and unblocks accounting.
///
/// Built once per projection pass: the accepted adjacency, the sessions that
/// can be stalled (not lost, focused on a non-terminal feature) with the
/// upstreams of their focus, and each such session's "nothing runnable" fact,
/// computed at most once and only when a row actually gates that session.
struct Stalls<'a> {
    project: &'a ProjectState,
    adjacency: BTreeMap<NodePath, BTreeSet<NodePath>>,
    candidates: Vec<(&'a SessionId, &'a NodePath, BTreeSet<NodePath>)>,
    idle: Vec<OnceCell<bool>>,
}

impl<'a> Stalls<'a> {
    fn new(project: &'a ProjectState) -> Self {
        let adjacency = adjacency(project);
        let mut candidates = Vec::new();
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
            let upstreams = ancestors(&adjacency, focus).into_iter().collect();
            candidates.push((session, focus, upstreams));
        }
        let idle = candidates.iter().map(|_| OnceCell::new()).collect();
        Self {
            project,
            adjacency,
            candidates,
            idle,
        }
    }

    fn unblocks_for(&self, node: &NodePath) -> Vec<NodePath> {
        descendants(&self.adjacency, node)
            .into_iter()
            .filter(|candidate| {
                self.project
                    .features
                    .get(candidate)
                    .is_some_and(|feature| feature.progress != Progress::Verified)
            })
            .collect()
    }

    /// Sessions stalled on one attention row.
    fn stalled_on(&self, node: &NodePath) -> Vec<SessionId> {
        let mut sessions = Vec::new();
        for (index, (session, focus, upstreams)) in self.candidates.iter().enumerate() {
            let gated_by_row = *focus == node || upstreams.contains(node);
            if !gated_by_row {
                continue;
            }
            let idle =
                *self.idle[index].get_or_init(|| runnable_nodes(session, self.project).is_empty());
            if !idle {
                continue;
            }
            sessions.push((*session).clone());
        }
        sessions
    }
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
    let stalls = Stalls::new(project);

    for (ask, decision) in &project.decisions {
        if decision.lifecycle != DecisionLifecycle::Open || decision.mode != Mode::Ask {
            continue;
        }
        let (risk, external, _) = risk_scan(&decision.card);
        let weight = weight_x2(risk, external);
        let stalled = stalls.stalled_on(&decision.node);
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
            unblocks: stalls.unblocks_for(&decision.node),
            closed_step: crate::reduce::moot::closed_step(
                decision,
                inputs.closed_steps,
                inputs.as_of,
            ),
            snoozed_until: None,
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
        let stalled = stalls.stalled_on(&decision.node);
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
            unblocks: stalls.unblocks_for(&decision.node),
            closed_step: None,
            snoozed_until: None,
        });
    }

    for edge in inputs.rejected {
        let source_ts = inputs
            .event_ts
            .get(&edge.source_event)
            .copied()
            .unwrap_or(inputs.as_of);
        let weight = 2_u8;
        let stalled = stalls.stalled_on(&edge.downstream);
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
            unblocks: stalls.unblocks_for(&edge.downstream),
            closed_step: None,
            snoozed_until: None,
        });
    }

    // Every claim that ended gets an information row: each step of a lost
    // session (the one it was on keeps its first row id) and each claim that
    // expired by silence. Any agent may pick the step up; the row lets the
    // owner pause it, and asks nothing.
    for (node, feature) in &project.features {
        let Some(claim) = feature
            .claim
            .as_ref()
            .filter(|claim| claim.state == Lease::Free)
        else {
            continue;
        };
        if feature.progress.is_terminal()
            || feature.handoff_pending
            || feature.handoff_consumed_by.is_some()
            || feature.paused
        {
            continue;
        }
        let Some(session) = &feature.session else {
            continue;
        };
        let lost = inputs
            .lost_sessions
            .iter()
            .find(|lost| &lost.session == session && feature.liveness == Liveness::Lost);
        let (id, source) = match lost {
            Some(lost) if lost.node.as_ref() == Some(node) => (
                format!("rescue:lost:{}", lost.lost_event.as_str()),
                lost.lost_event.clone(),
            ),
            Some(lost) => (
                format!(
                    "{LOST_STEP_ROW}{}:{}",
                    lost.lost_event.as_str(),
                    node.as_str()
                ),
                lost.lost_event.clone(),
            ),
            None => {
                let Some(attempt) = &feature.attempt else {
                    continue;
                };
                (format!("{SILENT_ROW}{}", attempt.as_str()), attempt.clone())
            }
        };
        let weight = 2_u8;
        let stalled = stalls.stalled_on(node);
        let minutes = waiting_minutes(claim.since, inputs.as_of);
        let quiet = waiting_minutes(claim.last_word, inputs.as_of);
        let (reason, reason_msg) = if lost.is_some() {
            (
                format!("Agent stopped · {}", age_label(minutes)),
                Msg::new("need.reason.stopped").minutes("age", minutes),
            )
        } else {
            (
                format!("No word for {}", age_label(quiet)),
                Msg::new("need.reason.silent").minutes("age", quiet),
            )
        };
        rows.push(NeedsYouItem {
            id,
            kind: NeedsKind::Rescue,
            project: project.summary.slug.clone(),
            node: Some(node.clone()),
            source,
            question: format!("{}: abandoned · open to any agent", node.as_str()),
            question_msg: Some(Msg::new("need.q.abandoned").text("node", node.as_str())),
            since: claim.since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: stalls.unblocks_for(node),
            closed_step: None,
            snoozed_until: None,
        });
    }

    for (node, attempt, since) in inputs.start_conflicts {
        let weight = 2_u8;
        let stalled = stalls.stalled_on(node);
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
            unblocks: stalls.unblocks_for(node),
            closed_step: None,
            snoozed_until: None,
        });
    }

    // A delivery whose review is still pending is a human review row. It is
    // visible even with zero stalled agents and never creates
    // a notification (`notification_eligible` returns false for Review).
    for delivery in project.deliveries.values() {
        if delivery.superseded_by.is_some() || delivery.review != ReviewStatus::Pending {
            continue;
        }
        // The agent dropped the attempt that delivered it: nothing to review.
        if inputs.dropped_attempts.contains(&delivery.attempt) {
            continue;
        }
        let weight = 2_u8;
        let stalled = stalls.stalled_on(&delivery.node);
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
            // Only a delivery that answers a decision was asked for.
            question: if delivery.receipt.is_some() {
                format!("Review {}: does it do what you asked?", delivery.name)
            } else {
                format!("Review {}", delivery.name)
            },
            question_msg: Some(
                Msg::new(if delivery.receipt.is_some() {
                    "need.q.review"
                } else {
                    "need.q.review_ready"
                })
                .text("name", delivery.name.clone()),
            ),
            since,
            stalled_agents: stalled.len() as u32,
            waiting_minutes: minutes,
            weight_x2: weight,
            score_x2: stack_score(stalled.len() as u32, minutes, weight),
            reason,
            reason_msg: Some(reason_msg),
            unblocks: stalls.unblocks_for(&delivery.node),
            closed_step: None,
            snoozed_until: None,
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
    let stalls = Stalls::new(project);
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    for row in rows {
        if let Some(node) = &row.node {
            sessions.extend(stalls.stalled_on(node));
        }
    }
    sessions.len() as u32
}

/// Sessions stalled on each row, for `SessionState.stalled_on`.
pub fn stalled_by_row(
    rows: &[NeedsYouItem],
    project: &ProjectState,
) -> BTreeMap<EventId, Vec<SessionId>> {
    let stalls = Stalls::new(project);
    let mut map = BTreeMap::new();
    for row in rows {
        if let Some(node) = &row.node {
            map.insert(row.source.clone(), stalls.stalled_on(node));
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
