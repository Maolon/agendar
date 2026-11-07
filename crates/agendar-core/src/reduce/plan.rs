//! Plan and progress projection: field ownership, attempts, handoff intent and
//! feature assembly.
//!
//! Every plan field is resolved by field ownership: a human patch outranks every
//! agent patch permanently; otherwise a later patch from the same writer wins by
//! sequence, and patches from different writers resolve by the causal order `O`.
//! Missing fields are never reset by an unrelated patch.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::event::{
    DeadlineUpdate, Event, EventPayload, PlanFeature, ProjectDeclared, WorkProgress, WorkStatus,
};
use crate::model::ids::{EtaRange, EventId, Harness, HostId, NodePath, SessionId, UtcTime};
use crate::model::state::{
    DecisionLifecycle, DecisionState, EtaState, FeatureState, Gate, GraphNodeId, Liveness, Progress,
};
use crate::reduce::{CausalView, FoldNote};

/// One winning field patch candidate.
#[derive(Debug, Clone)]
pub struct Patch<T> {
    pub value: T,
    pub source: EventId,
    pub order: usize,
    pub writer: String,
    pub seq: u64,
    pub human: bool,
}

impl<T> Patch<T> {
    /// Ownership comparison: human beats agent; same writer beats by sequence;
    /// different writers resolve by the causal order.
    pub fn outranks(&self, other: &Self) -> bool {
        match (self.human, other.human) {
            (true, false) => true,
            (false, true) => false,
            _ if self.writer == other.writer => self.seq > other.seq,
            _ => self.order > other.order,
        }
    }
}

fn offer<T>(slot: &mut Option<Patch<T>>, incoming: Patch<T>) {
    if slot
        .as_ref()
        .is_none_or(|current| incoming.outranks(current))
    {
        *slot = Some(incoming);
    }
}

fn patch<T>(value: T, event: &Event, order: usize) -> Patch<T> {
    Patch {
        value,
        source: event.id.clone(),
        order,
        writer: event.writer.to_string(),
        seq: event.seq,
        human: event.writer.is_operator(),
    }
}

/// Project-level declaration patch fields.
#[derive(Debug, Default)]
pub struct Declaration {
    pub title: Option<Patch<String>>,
    pub brief: Option<Patch<String>>,
    pub goal: Option<Patch<String>>,
    pub glyph: Option<Patch<String>>,
    pub archived: Option<Patch<bool>>,
    pub icon: Option<Patch<crate::model::BlobRef>>,
}

impl Declaration {
    pub fn apply(&mut self, event: &Event, order: usize) {
        let EventPayload::ProjectDeclared(ProjectDeclared {
            title,
            brief,
            goal,
            glyph,
            archived,
            icon,
            ..
        }) = &event.payload
        else {
            return;
        };
        if let Some(value) = title {
            offer(&mut self.title, patch(value.clone(), event, order));
        }
        if let Some(value) = brief {
            offer(&mut self.brief, patch(value.clone(), event, order));
        }
        if let Some(value) = goal {
            offer(&mut self.goal, patch(value.clone(), event, order));
        }
        if let Some(value) = glyph {
            offer(&mut self.glyph, patch(value.clone(), event, order));
        }
        if let Some(value) = archived {
            offer(&mut self.archived, patch(*value, event, order));
        }
        if let Some(value) = icon {
            offer(&mut self.icon, patch(value.clone(), event, order));
        }
    }
}

/// One attempt (`work.progress` with status `started`).
#[derive(Debug, Clone)]
pub struct AttemptFold {
    pub id: EventId,
    pub session: SessionId,
    pub harness: Harness,
    pub started_at: UtcTime,
    pub order: usize,
    /// The ETA the agent declared when it started. Pulses update `eta` but never
    /// rewrite this original claim; calibration samples and receipt history use
    /// it.
    pub start_eta: EtaRange,
    pub eta: EtaRange,
    pub eta_ts: UtcTime,
    pub eta_order: usize,
    pub status: WorkStatus,
    pub status_event: Option<EventId>,
    pub status_ts: Option<UtcTime>,
    /// `O` index and timestamp of the first `done`/`verified` completion. A
    /// later `verified` after `done` never moves it, so one attempt yields one
    /// calibration sample.
    pub first_completion: Option<(usize, UtcTime)>,
    pub causes: BTreeSet<EventId>,
    /// Receipts this attempt reported as applied through `causes`/`basis`.
    pub applied: BTreeSet<EventId>,
    /// The start event's own `refs.basis`, kept immutable: only the explicit
    /// restart can repair a carried taint, never a later completion that cites
    /// a receipt.
    pub start_basis: BTreeSet<EventId>,
}

/// One winning `depends_on` transition, in causal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyChange {
    pub order: usize,
    pub ts: UtcTime,
    pub depends_on: Vec<NodePath>,
}

/// Per-feature fold state.
#[derive(Debug)]
pub struct FeatureFold {
    pub node: NodePath,
    pub title: Option<Patch<String>>,
    pub slice: Option<Patch<String>>,
    pub demo_kind: Option<Patch<crate::model::cards::DemoKind>>,
    pub milestone: Option<Patch<String>>,
    pub deadline: Option<Patch<Option<UtcTime>>>,
    pub priority: Option<Patch<u8>>,
    pub paused: Option<Patch<bool>>,
    pub handoff: Option<Patch<bool>>,
    pub depends_on: Option<Patch<Vec<NodePath>>>,
    /// Every time a `depends_on` patch became the winning value. Human waits
    /// are replayed against this history so a later edit cannot rewrite a wait
    /// that was already spent.
    pub depends_on_history: Vec<DependencyChange>,
    pub attempts: BTreeMap<EventId, AttemptFold>,
    pub current_attempt: Option<EventId>,
    pub start_conflicts: Vec<EventId>,
    pub last_pulse: Option<(usize, EventId)>,
    /// A `dropped` without an attempt: the step left the plan before anyone
    /// started it. A later start brings it back.
    pub unstarted_drop: Option<EventId>,
}

impl Default for FeatureFold {
    fn default() -> Self {
        Self {
            node: NodePath::parse("unset").expect("fixed placeholder node"),
            title: None,
            slice: None,
            demo_kind: None,
            milestone: None,
            deadline: None,
            priority: None,
            paused: None,
            handoff: None,
            depends_on: None,
            depends_on_history: Vec::new(),
            attempts: BTreeMap::new(),
            current_attempt: None,
            start_conflicts: Vec::new(),
            last_pulse: None,
            unstarted_drop: None,
        }
    }
}

impl FeatureFold {
    pub fn new(node: NodePath) -> Self {
        Self {
            node,
            ..Self::default()
        }
    }

    pub fn apply(&mut self, event: &Event, order: usize) {
        let EventPayload::PlanFeature(body) = &event.payload else {
            return;
        };
        let PlanFeature {
            title,
            depends_on,
            slice,
            demo_kind,
            milestone,
            deadline,
            priority,
            paused,
            handoff,
            ..
        } = body;
        if let Some(value) = title {
            offer(&mut self.title, patch(value.clone(), event, order));
        }
        if let Some(value) = slice {
            offer(&mut self.slice, patch(value.clone(), event, order));
        }
        if let Some(value) = demo_kind {
            offer(&mut self.demo_kind, patch(*value, event, order));
        }
        if let Some(value) = milestone {
            offer(&mut self.milestone, patch(value.clone(), event, order));
        }
        if let Some(value) = deadline {
            let value = match value {
                DeadlineUpdate::Set { value } => Some(*value),
                DeadlineUpdate::Clear => None,
            };
            offer(&mut self.deadline, patch(value, event, order));
        }
        if let Some(value) = priority {
            offer(&mut self.priority, patch(*value, event, order));
        }
        if let Some(value) = paused {
            offer(&mut self.paused, patch(*value, event, order));
        }
        if let Some(value) = handoff {
            offer(&mut self.handoff, patch(*value, event, order));
        }
        if let Some(value) = depends_on {
            let incoming = patch(value.clone(), event, order);
            let wins = self
                .depends_on
                .as_ref()
                .is_none_or(|current| incoming.outranks(current));
            offer(&mut self.depends_on, incoming);
            if wins {
                self.depends_on_history.push(DependencyChange {
                    order,
                    ts: event.ts,
                    depends_on: value.clone(),
                });
            }
        }
    }

    pub fn apply_progress(
        &mut self,
        event: &Event,
        order: usize,
        causal: &CausalView<'_>,
    ) -> Option<FoldNote> {
        let EventPayload::WorkProgress(body) = &event.payload else {
            return None;
        };
        let WorkProgress {
            agent, status, eta, ..
        } = body;
        match status {
            WorkStatus::Started => {
                let Some(eta) = eta else {
                    return Some(FoldNote::invalid("a started attempt requires an ETA range"));
                };
                // A restart supersedes old work only when it explicitly cites the
                // attempt it replaces. A causally later start that
                // silently ignores the current attempt is rejected.
                if let Some(current) = self.current_attempt.clone()
                    && causal.before(&current, &event.id)
                    && !event.refs.causes.contains(&current)
                {
                    return Some(FoldNote::invalid(
                        "a restart must cite the current attempt in refs.causes",
                    ));
                }
                let attempt = AttemptFold {
                    id: event.id.clone(),
                    session: agent.session.clone(),
                    harness: agent.harness,
                    started_at: event.ts,
                    order,
                    start_eta: *eta,
                    eta: *eta,
                    eta_ts: event.ts,
                    eta_order: order,
                    status: WorkStatus::Started,
                    status_event: None,
                    status_ts: None,
                    first_completion: None,
                    causes: event.refs.causes.iter().cloned().collect(),
                    applied: event
                        .refs
                        .causes
                        .iter()
                        .chain(event.refs.basis.iter())
                        .cloned()
                        .collect(),
                    start_basis: event.refs.basis.iter().cloned().collect(),
                };
                if let Some(current) = self.current_attempt.clone()
                    && !attempt.causes.contains(&current)
                    && !self.start_conflicts.contains(&current)
                {
                    self.start_conflicts.push(current);
                }
                self.attempts.insert(event.id.clone(), attempt);
                self.current_attempt = Some(event.id.clone());
                None
            }
            WorkStatus::Dropped if event.refs.attempt.is_none() => {
                // Only a step nobody has started can leave the plan this way;
                // running or finished work is dropped by its attempt.
                if !self.attempts.is_empty() {
                    return Some(FoldNote::invalid(
                        "this step has an attempt; drop it by attempt reference",
                    ));
                }
                if self.unstarted_drop.is_some() {
                    return Some(FoldNote::history("the step was already dropped"));
                }
                self.unstarted_drop = Some(event.id.clone());
                None
            }
            WorkStatus::Done | WorkStatus::Verified | WorkStatus::Dropped => {
                let Some(attempt_id) = event.refs.attempt.clone() else {
                    return Some(FoldNote::invalid(
                        "a non-started progress event requires an attempt reference",
                    ));
                };
                let Some(attempt) = self.attempts.get_mut(&attempt_id) else {
                    return Some(FoldNote::invalid("the referenced attempt is unknown"));
                };
                if attempt.status == WorkStatus::Dropped {
                    return Some(FoldNote::history(
                        "dropped is terminal until a new start; the status is history only",
                    ));
                }
                attempt.applied.extend(
                    event
                        .refs
                        .causes
                        .iter()
                        .chain(event.refs.basis.iter())
                        .cloned(),
                );
                if matches!(status, WorkStatus::Done | WorkStatus::Verified)
                    && attempt.first_completion.is_none()
                {
                    attempt.first_completion = Some((order, event.ts));
                }
                attempt.status = *status;
                attempt.status_event = Some(event.id.clone());
                attempt.status_ts = Some(event.ts);
                None
            }
        }
    }

    pub fn apply_pulse(&mut self, event: &Event, order: usize) {
        let EventPayload::Pulse(body) = &event.payload else {
            return;
        };
        if event.refs.node.as_ref() != Some(&self.node) {
            return;
        }
        if body.eta.is_some()
            && let Some(attempt_id) = event.refs.attempt.clone()
            && let Some(attempt) = self.attempts.get_mut(&attempt_id)
            && attempt.status == WorkStatus::Started
            && let Some(eta) = body.eta
        {
            attempt.eta = eta;
            attempt.eta_ts = event.ts;
            attempt.eta_order = order;
        }
        if self
            .last_pulse
            .as_ref()
            .is_none_or(|(previous, _)| order > *previous)
        {
            self.last_pulse = Some((order, event.id.clone()));
        }
    }

    pub fn current(&self) -> Option<&AttemptFold> {
        self.current_attempt
            .as_ref()
            .and_then(|id| self.attempts.get(id))
    }
}

/// Resolved plan fields for one feature.
#[derive(Debug, Clone)]
pub struct FeatureFields {
    pub title: String,
    pub slice: String,
    pub demo_kind: crate::model::cards::DemoKind,
    pub milestone: Option<String>,
    pub deadline: Option<UtcTime>,
    pub priority: u8,
    pub paused: bool,
    pub handoff: bool,
    pub depends_on: Vec<NodePath>,
    pub field_sources: BTreeMap<String, EventId>,
    pub complete: bool,
    pub handoff_event: Option<EventId>,
    pub handoff_consumed_by: Option<EventId>,
    pub handoff_pending: bool,
}

/// Resolve the winning plan fields, including handoff intent consumption.
pub fn fields(fold: &FeatureFold) -> FeatureFields {
    let mut field_sources = BTreeMap::new();
    if let Some(patch) = &fold.title {
        field_sources.insert("title".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.slice {
        field_sources.insert("slice".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.demo_kind {
        field_sources.insert("demo_kind".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.milestone {
        field_sources.insert("milestone".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.deadline {
        field_sources.insert("deadline".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.priority {
        field_sources.insert("priority".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.paused {
        field_sources.insert("paused".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.handoff {
        field_sources.insert("handoff".to_owned(), patch.source.clone());
    }
    if let Some(patch) = &fold.depends_on {
        field_sources.insert("depends_on".to_owned(), patch.source.clone());
    }

    let (handoff_event, handoff_consumed_by, handoff_pending) = match &fold.handoff {
        Some(patch) if patch.value => {
            // Ownership is the attempt that preceded the human handoff intent.
            let owner_attempt = fold
                .attempts
                .values()
                .filter(|attempt| attempt.order < patch.order)
                .max_by_key(|attempt| attempt.order);
            let owner = owner_attempt.map(|attempt| attempt.session.clone());
            let owner_id = owner_attempt.map(|attempt| attempt.id.clone());
            // The winning replacement (ordinary start-conflict rules) must cite
            // the handoff, belong to another session, and prove the previous
            // attempt it replaces; the earliest historical start never wins.
            let consumer = fold
                .attempts
                .values()
                .filter(|attempt| attempt.order > patch.order)
                .filter(|attempt| attempt.causes.contains(&patch.source))
                .filter(|attempt| owner.as_ref().is_none_or(|owner| owner != &attempt.session))
                .filter(|attempt| {
                    owner_id
                        .as_ref()
                        .is_none_or(|previous| attempt.causes.contains(previous))
                })
                .max_by_key(|attempt| attempt.order)
                .map(|attempt| attempt.id.clone());
            (
                Some(patch.source.clone()),
                consumer.clone(),
                consumer.is_none(),
            )
        }
        Some(_) => (None, None, false),
        None => (None, None, false),
    };

    FeatureFields {
        title: fold
            .title
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_default(),
        slice: fold
            .slice
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_default(),
        demo_kind: fold
            .demo_kind
            .as_ref()
            .map(|patch| patch.value)
            .unwrap_or(crate::model::cards::DemoKind::CliTranscript),
        milestone: fold.milestone.as_ref().map(|patch| patch.value.clone()),
        deadline: fold.deadline.as_ref().and_then(|patch| patch.value),
        priority: fold.priority.as_ref().map(|patch| patch.value).unwrap_or(1),
        paused: fold.paused.as_ref().is_some_and(|patch| patch.value),
        handoff: fold.handoff.as_ref().is_some_and(|patch| patch.value),
        depends_on: fold
            .depends_on
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_default(),
        field_sources,
        complete: fold.title.is_some() && fold.slice.is_some() && fold.demo_kind.is_some(),
        handoff_event,
        handoff_consumed_by,
        handoff_pending,
    }
}

/// Progress contributed by the current attempt only; old attempts never
/// complete a restart.
pub fn progress(fold: &FeatureFold, require_human_review: bool) -> Progress {
    let Some(current) = fold.current() else {
        return if fold.unstarted_drop.is_some() {
            Progress::Dropped
        } else {
            Progress::Planned
        };
    };
    match current.status {
        WorkStatus::Started => Progress::Doing,
        WorkStatus::Done => Progress::Done,
        WorkStatus::Verified => {
            if require_human_review {
                Progress::Done
            } else {
                Progress::Verified
            }
        }
        WorkStatus::Dropped => Progress::Dropped,
    }
}

/// Upstream facts needed for gates and waits_on.
#[derive(Debug, Default)]
pub struct DependencyView {
    pub accepted_upstreams: BTreeMap<NodePath, Vec<NodePath>>,
    pub rejected_upstreams: BTreeMap<NodePath, BTreeSet<NodePath>>,
    pub missing_upstreams: BTreeMap<NodePath, BTreeSet<NodePath>>,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureContext {
    pub decisions: BTreeMap<NodePath, Vec<EventId>>,
    pub upstream_progress: BTreeMap<NodePath, Progress>,
    pub upstream_pending_review: BTreeSet<NodePath>,
    pub upstream_review_accepted: BTreeSet<NodePath>,
    pub upstream_tainted: BTreeSet<NodePath>,
    pub sessions: BTreeMap<SessionId, Liveness>,
}

/// Compute one feature's gate and waits-on list.
pub fn gate_and_waits(
    node: &NodePath,
    fields: &FeatureFields,
    progress: Progress,
    deps: &DependencyView,
    context: &FeatureContext,
    pending_decisions: &[EventId],
    require_human_review: bool,
) -> (Gate, Vec<GraphNodeId>) {
    let mut waits: Vec<GraphNodeId> = Vec::new();
    let mut upstream_blocked = false;

    if let Some(missing) = deps.missing_upstreams.get(node) {
        for upstream in missing {
            waits.push(GraphNodeId::feature(upstream));
            upstream_blocked = true;
        }
    }
    if let Some(rejected) = deps.rejected_upstreams.get(node) {
        for upstream in rejected {
            waits.push(GraphNodeId::feature(upstream));
            upstream_blocked = true;
        }
    }
    if let Some(accepted) = deps.accepted_upstreams.get(node) {
        for upstream in accepted {
            let upstream_state = context
                .upstream_progress
                .get(upstream)
                .copied()
                .unwrap_or(Progress::Planned);
            let pending_review = context.upstream_pending_review.contains(upstream);
            let accepted_review = context.upstream_review_accepted.contains(upstream);
            let satisfied = !pending_review
                && match upstream_state {
                    Progress::Verified => true,
                    Progress::Done => !require_human_review || accepted_review,
                    Progress::Planned | Progress::Doing | Progress::Dropped => false,
                }
                && !context.upstream_tainted.contains(upstream);
            if !satisfied {
                waits.push(GraphNodeId::feature(upstream));
                upstream_blocked = true;
            }
        }
    }
    for ask in pending_decisions {
        waits.push(GraphNodeId::decision(ask));
    }
    waits.sort();
    waits.dedup();

    let gate = if fields.paused || fields.handoff_pending || !pending_decisions.is_empty() {
        Gate::AwaitingDecision
    } else if upstream_blocked {
        Gate::AwaitingUpstream
    } else {
        Gate::Open
    };
    let _ = progress;
    (gate, waits)
}

#[derive(Debug, Clone)]
pub struct SessionFold {
    pub id: SessionId,
    pub host: HostId,
    pub harness: Harness,
    pub last_event: EventId,
    pub last_event_ts: UtcTime,
    pub last_order: usize,
    pub current_node: Option<NodePath>,
    pub lost_event: Option<EventId>,
}

/// Track an agent session from any event authored by its writer.
///
/// Execution focus follows only start/work-pulse/ask events; a
/// plan patch, done pulse or unrelated fact updates freshness but must never
/// move a blocked session's focus.
pub fn touch_session(sessions: &mut BTreeMap<SessionId, SessionFold>, event: &Event, order: usize) {
    let Some(session) = event.writer.session() else {
        return;
    };
    let harness = event
        .payload
        .agent()
        .map(|agent| agent.harness)
        .or_else(|| sessions.get(session).map(|fold| fold.harness))
        .unwrap_or(Harness::Generic);
    let focused = event.refs.node.clone().filter(|_| match &event.payload {
        EventPayload::WorkProgress(body) => body.status == WorkStatus::Started,
        EventPayload::Pulse(_) => true,
        EventPayload::DecisionAsk(_) => true,
        _ => false,
    });
    let node = focused.or_else(|| {
        sessions
            .get(session)
            .and_then(|fold| fold.current_node.clone())
    });
    let entry = sessions.entry(session.clone()).or_insert(SessionFold {
        id: session.clone(),
        host: event.writer.host().clone(),
        harness,
        last_event: event.id.clone(),
        last_event_ts: event.ts,
        last_order: order,
        current_node: node.clone(),
        lost_event: None,
    });
    if order >= entry.last_order {
        entry.last_order = order;
        entry.last_event = event.id.clone();
        entry.last_event_ts = event.ts;
        entry.current_node = node;
        entry.harness = harness;
    }
}

/// Apply a `session.lost` fact (the host/session match is resolved earlier).
pub fn apply_session_lost(
    sessions: &mut BTreeMap<SessionId, SessionFold>,
    event: &Event,
    order: usize,
) {
    let EventPayload::SessionLost(body) = &event.payload else {
        return;
    };
    if let Some(fold) = sessions.get_mut(&body.session) {
        fold.lost_event = Some(event.id.clone());
        if order >= fold.last_order {
            fold.last_order = order;
            fold.last_event = event.id.clone();
            fold.last_event_ts = event.ts;
        }
    }
}

/// Apply a `session.continued` fact (the host match is resolved earlier): the
/// lost predecessor's running attempts now belong to its continuation, so the
/// same agent conversation pulses and finishes its own steps after a restart.
/// Finished history keeps its original session.
pub fn apply_session_continued(
    sessions: &BTreeMap<SessionId, SessionFold>,
    features: &mut BTreeMap<NodePath, FeatureFold>,
    event: &Event,
) {
    let EventPayload::SessionContinued(body) = &event.payload else {
        return;
    };
    let lost = sessions
        .get(&body.previous)
        .is_some_and(|fold| fold.lost_event.is_some());
    if !lost {
        return;
    }
    for feature in features.values_mut() {
        for attempt in feature.attempts.values_mut() {
            if attempt.session == body.previous && attempt.status == WorkStatus::Started {
                attempt.session = body.session.clone();
            }
        }
    }
}

/// Liveness of one session at `as_of`: a lost fact wins; otherwise the session
/// is live while its last agent event is at most 15 minutes old.
pub fn session_liveness(lost: bool, last_event_ts: UtcTime, as_of: UtcTime) -> Liveness {
    if lost {
        return Liveness::Lost;
    }
    let elapsed_ms = as_of.millis() - last_event_ts.millis();
    if elapsed_ms <= 15 * 60 * 1000 {
        Liveness::Live
    } else {
        Liveness::Quiet
    }
}

/// ETA projection for the current attempt: original range, calibrated likely
/// range when the project × harness has history, and lateness against it.
pub fn attempt_eta(
    attempt: &AttemptFold,
    pauses: &[(UtcTime, Option<UtcTime>)],
    as_of: UtcTime,
    ratio: Option<u32>,
) -> EtaState {
    crate::reduce::eta::compute(attempt, pauses, as_of, ratio)
}

/// Decisions targeting one node, open mandatory or otherwise.
pub fn decisions_for_node(
    decisions: &BTreeMap<EventId, DecisionState>,
    node: &NodePath,
) -> Vec<EventId> {
    decisions
        .iter()
        .filter(|(_, decision)| &decision.node == node)
        .map(|(ask, _)| ask.clone())
        .collect()
}

/// Open mandatory decisions on one node.
pub fn open_decisions(
    decisions: &BTreeMap<EventId, DecisionState>,
    node: &NodePath,
) -> Vec<EventId> {
    decisions_for_node(decisions, node)
        .into_iter()
        .filter(|ask| {
            decisions.get(ask).is_some_and(|decision| {
                decision.lifecycle == DecisionLifecycle::Open
                    && decision.mode == crate::model::ids::Mode::Ask
            })
        })
        .collect()
}

/// Project a feature fold into its derived state.
#[allow(clippy::too_many_arguments)]
pub fn build_feature(
    fold: &FeatureFold,
    fields: FeatureFields,
    accepted_depends_on: Vec<NodePath>,
    progress: Progress,
    gate: Gate,
    waits_on: Vec<GraphNodeId>,
    unblocks: Vec<NodePath>,
    liveness: Liveness,
    eta: Option<EtaState>,
    assumptions: Vec<EventId>,
    taints: Vec<EventId>,
    require_human_review: bool,
) -> FeatureState {
    let current = fold.current();
    let _ = require_human_review;
    // Basis is independent of progress: an unresolved assumption
    // is assumed, an actually overridden one is tainted, and only work with no
    // unconfirmed choice at all is firm.
    let basis = if !taints.is_empty() {
        crate::model::state::Basis::Tainted
    } else if !assumptions.is_empty() {
        crate::model::state::Basis::Assumed
    } else {
        crate::model::state::Basis::Firm
    };
    FeatureState {
        node: fold.node.clone(),
        title: fields.title,
        slice: fields.slice,
        demo_kind: fields.demo_kind,
        milestone: fields.milestone,
        deadline: fields.deadline,
        priority: fields.priority,
        paused: fields.paused,
        handoff: fields.handoff,
        depends_on: fields.depends_on,
        accepted_depends_on,
        complete: fields.complete,
        field_sources: fields.field_sources,
        progress,
        gate,
        basis,
        liveness,
        attempt: current.map(|attempt| attempt.id.clone()),
        session: current.map(|attempt| attempt.session.clone()),
        assumptions,
        taints,
        waits_on,
        unblocks,
        eta,
        last_pulse: fold.last_pulse.as_ref().map(|(_, id)| id.clone()),
        handoff_event: fields.handoff_event,
        handoff_consumed_by: fields.handoff_consumed_by,
        handoff_pending: fields.handoff_pending,
    }
}
