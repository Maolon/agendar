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
    ClaimState, Correction, CorrectionRule, EtaState, FeatureState, Gate, GraphNodeId, Lease,
    Liveness, Phase, Progress, TakenFrom,
};
use crate::reduce::{CausalView, FoldNote};

/// What touched one step: its latest event and whether the owner wrote any.
/// "Any event" is any event of the project that refers to the node; "the
/// owner" is an operator writer. One test for the reducer's `untouched` rule
/// and for `settle`'s candidates (`owed.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub last: UtcTime,
    pub owner: bool,
}

impl Touch {
    pub fn record(touches: &mut BTreeMap<NodePath, Touch>, event: &Event) {
        let Some(node) = event.refs.node.as_ref() else {
            return;
        };
        let owner = event.writer.is_operator();
        touches
            .entry(node.clone())
            .and_modify(|touch| {
                touch.last = touch.last.max(event.ts);
                touch.owner |= owner;
            })
            .or_insert(Touch {
                last: event.ts,
                owner,
            });
    }
}

/// The steps a human answered: an answer receipt from the owner is theirs.
pub fn human_decided<'a>(
    receipts: impl Iterator<Item = &'a crate::model::state::Receipt>,
) -> BTreeSet<NodePath> {
    receipts
        .filter(|receipt| receipt.source == crate::model::state::ReceiptSource::Human)
        .map(|receipt| receipt.node.clone())
        .collect()
}

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
    /// The session that wrote the start. `session` moves to a continuation
    /// (`apply_session_continued`); this never changes.
    pub started_by: SessionId,
    /// Every session this attempt has belonged to: the one that started it
    /// and each continuation. One agent conversation.
    pub sessions: BTreeSet<SessionId>,
    /// The deliveries on this attempt.
    pub deliveries: Vec<EventId>,
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
    /// `O` index and timestamp of every pulse that set an ETA after the start:
    /// the agent's work signals besides its deliveries.
    pub eta_signals: Vec<(usize, UtcTime)>,
    /// What the fold held before a `dropped`, and the drop's `O` index: a drop
    /// ordered after the accepting review is history only (`reduce::settle`).
    pub before_drop: Option<DropUndo>,
    /// The derived settle from this attempt's deliveries and their reviews,
    /// filled in when the project is finalized.
    pub settle: crate::reduce::settle::Settle,
    /// The latest event on this attempt by its session (a pulse, a delivery,
    /// a progress event or a question on the step): the claim lease runs from
    /// it (`reduce::claim`).
    pub last_word: (UtcTime, EventId),
    /// Every word its own session wrote on this attempt after the start, so
    /// a takeover can tell whether it saw them (`contested_takeovers`).
    pub words: Vec<EventId>,
}

/// The fold state a `dropped` replaced, kept so a drop the owner's accept
/// outranks can be undone deterministically.
#[derive(Debug, Clone)]
pub struct DropUndo {
    pub order: usize,
    pub event: EventId,
    pub status: WorkStatus,
    pub status_event: Option<EventId>,
    pub status_ts: Option<UtcTime>,
}

impl AttemptFold {
    /// Record a word from the agent on this attempt; the latest one counts.
    pub fn heard(&mut self, event: &Event) {
        if event.ts >= self.last_word.0 {
            self.last_word = (event.ts, event.id.clone());
        }
        if event.id != self.id && event.writer.session() == Some(&self.session) {
            self.words.push(event.id.clone());
        }
        if matches!(event.payload, EventPayload::Delivery(_)) {
            self.deliveries.push(event.id.clone());
        }
    }

    /// The latest live delivery the owner's accept closed, when the derived
    /// settle is done (no work signal after it, every live delivery accepted).
    pub fn accepted_at(&self) -> Option<(usize, UtcTime)> {
        if self.status == WorkStatus::Dropped {
            return None;
        }
        (self.settle.phase == Some(crate::reduce::settle::SettlePhase::Accepted))
            .then_some(self.settle.delivered)
            .flatten()
    }

    /// Completion time for calibration and closing waits: the earlier of the
    /// first done/verified event and the delivery a derived done rests on.
    pub fn completion(&self) -> Option<(usize, UtcTime)> {
        match (self.first_completion, self.accepted_at()) {
            (Some(done), Some(accepted)) => Some(if accepted.1.millis() < done.1.millis() {
                accepted
            } else {
                done
            }),
            (done, accepted) => done.or(accepted),
        }
    }
}

/// End `attempt` by the owner's drop.
fn close_by_owner(attempt: &mut AttemptFold, drop: &OwnerDropFold) {
    attempt.status = WorkStatus::Dropped;
    attempt.status_event = Some(drop.event.clone());
    attempt.status_ts = Some(drop.ts);
    attempt.before_drop = None;
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
    /// Every plan patch that asked for a hand-off: a start citing one is the
    /// owner's choice, never a contested takeover.
    pub handoff_events: BTreeSet<EventId>,
    pub last_pulse: Option<(usize, EventId)>,
    /// A `dropped` without an attempt: the step left the plan before anyone
    /// started it. A later start brings it back.
    pub unstarted_drop: Option<EventId>,
    /// The owner's drops of this step (`plan.dropped`), in `O`.
    pub owner_drops: Vec<OwnerDropFold>,
}

/// One `plan.dropped` on a step.
#[derive(Debug, Clone)]
pub struct OwnerDropFold {
    pub event: EventId,
    pub ts: UtcTime,
    pub note: Option<String>,
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
            handoff_events: BTreeSet::new(),
            last_pulse: None,
            unstarted_drop: None,
            owner_drops: Vec::new(),
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
            if *value {
                self.handoff_events.insert(event.id.clone());
            }
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
                // An attempt the owner's drop closes need not be cited: the
                // board shows another attempt there, and the service cites
                // the one it shows.
                if let Some(current) = self.current_attempt.clone()
                    && causal.before(&current, &event.id)
                    && !event.refs.causes.contains(&current)
                    && self.closed_by(&current, causal).is_none()
                {
                    return Some(FoldNote::invalid(
                        "a restart must cite the current attempt in refs.causes",
                    ));
                }
                let attempt = AttemptFold {
                    id: event.id.clone(),
                    session: agent.session.clone(),
                    started_by: agent.session.clone(),
                    sessions: BTreeSet::from([agent.session.clone()]),
                    deliveries: Vec::new(),
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
                    eta_signals: Vec::new(),
                    before_drop: None,
                    settle: crate::reduce::settle::Settle::default(),
                    last_word: (event.ts, event.id.clone()),
                    words: Vec::new(),
                };
                // A start the owner's drop closes (`settle_owner_drops`) is
                // no start conflict: the owner settled it.
                let closed = self.closed_by(&event.id, causal).is_some();
                if let Some(current) = self.current_attempt.clone()
                    && !closed
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
                attempt.heard(event);
                attempt.applied.extend(
                    event
                        .refs
                        .causes
                        .iter()
                        .chain(event.refs.basis.iter())
                        .cloned(),
                );
                if *status == WorkStatus::Dropped {
                    attempt.before_drop = Some(DropUndo {
                        order,
                        event: event.id.clone(),
                        status: attempt.status,
                        status_event: attempt.status_event.clone(),
                        status_ts: attempt.status_ts,
                    });
                }
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

    /// Record the owner's `plan.dropped`. Every drop of the project is
    /// recorded before the fold starts, and none changes the fold: an agent's
    /// event folds exactly as it would without the drop, so the drop never
    /// makes it invalid and its place in `O` beside the drop never matters.
    pub fn record_owner_drop(&mut self, event: &Event) {
        let EventPayload::PlanDropped(body) = &event.payload else {
            return;
        };
        self.owner_drops.push(OwnerDropFold {
            event: event.id.clone(),
            ts: event.ts,
            note: body.note.clone(),
        });
    }

    /// The owner's drop that closes the attempt `start` began: the first one
    /// in `O` that is not in the start's causal past. Owner > agent: only a
    /// start that follows every drop is open.
    pub fn closed_by(&self, start: &EventId, causal: &CausalView<'_>) -> Option<&OwnerDropFold> {
        self.owner_drops
            .iter()
            .find(|drop| !causal.before(&drop.event, start))
    }

    /// Apply the owner's drops once every event is folded. The step ends as
    /// after an agent's drop, by the causal relation alone:
    ///
    /// - a step nobody started leaves the plan;
    /// - an attempt a drop closes ends dropped by it, unless it had finished
    ///   and was replaced before the drop (old history stays as it was). The
    ///   owner's drop is never undone by an accept and outranks the agent's
    ///   own drop;
    /// - the step shows an open attempt when there is one (the fold's
    ///   current attempt, else the latest open one), else the earliest
    ///   closed attempt that no other attempt replaced.
    pub fn settle_owner_drops(&mut self, causal: &CausalView<'_>) {
        let Some(first) = self.owner_drops.first().cloned() else {
            return;
        };
        if self.attempts.is_empty() {
            self.unstarted_drop = Some(first.event);
            return;
        }
        let replaced: BTreeSet<EventId> = self
            .attempts
            .values()
            .flat_map(|attempt| attempt.causes.iter().cloned())
            .filter(|cause| self.attempts.contains_key(cause))
            .collect();
        let closing: BTreeMap<EventId, OwnerDropFold> = self
            .attempts
            .keys()
            .filter_map(|id| Some((id.clone(), self.closed_by(id, causal)?.clone())))
            .collect();
        // Finished and replaced before the drop: by a start the same drop
        // does not precede.
        let history: BTreeSet<EventId> = closing
            .iter()
            .filter(|(id, drop)| {
                self.attempts.values().any(|later| {
                    later.causes.contains(*id) && !causal.before(&drop.event, &later.id)
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for attempt in self.attempts.values_mut() {
            if let Some(drop) = closing.get(&attempt.id)
                && (attempt.status == WorkStatus::Started || !history.contains(&attempt.id))
            {
                close_by_owner(attempt, drop);
            }
        }
        let open = |id: &EventId| !closing.contains_key(id);
        if self.current_attempt.as_ref().is_some_and(open) {
            return;
        }
        let shown = self
            .attempts
            .values()
            .filter(|attempt| open(&attempt.id))
            .max_by_key(|attempt| attempt.order)
            .or_else(|| {
                self.attempts
                    .values()
                    .filter(|attempt| !replaced.contains(&attempt.id))
                    .min_by_key(|attempt| attempt.order)
            })
            .map(|attempt| attempt.id.clone());
        if shown.is_some() {
            self.current_attempt = shown;
        }
    }

    /// Whether the owner's drop is what ended `attempt`.
    pub fn dropped_by_owner(&self, attempt: &AttemptFold) -> bool {
        attempt.status == WorkStatus::Dropped
            && attempt
                .status_event
                .as_ref()
                .is_some_and(|id| self.owner_drops.iter().any(|drop| &drop.event == id))
    }

    /// The owner's drop that closed this step, while it is what the step
    /// shows.
    pub fn owner_drop(&self) -> Option<&OwnerDropFold> {
        let closing = match self.current() {
            None => self.unstarted_drop.as_ref(),
            Some(attempt) if attempt.status == WorkStatus::Dropped => attempt.status_event.as_ref(),
            Some(_) => None,
        }?;
        self.owner_drops.iter().find(|drop| &drop.event == closing)
    }

    pub fn apply_pulse(&mut self, event: &Event, order: usize) {
        let EventPayload::Pulse(body) = &event.payload else {
            return;
        };
        if event.refs.node.as_ref() != Some(&self.node) {
            return;
        }
        self.heard(event);
        if body.eta.is_some()
            && let Some(attempt_id) = event.refs.attempt.clone()
            && let Some(attempt) = self.attempts.get_mut(&attempt_id)
            && attempt.status == WorkStatus::Started
            && let Some(eta) = body.eta
        {
            attempt.eta = eta;
            attempt.eta_ts = event.ts;
            attempt.eta_order = order;
            attempt.eta_signals.push((order, event.ts));
        }
        if self
            .last_pulse
            .as_ref()
            .is_none_or(|(previous, _)| order > *previous)
        {
            self.last_pulse = Some((order, event.id.clone()));
        }
    }

    /// Credit an agent event on this step to the attempt it is on: the one it
    /// names, else the current attempt when the writer is its session.
    pub fn heard(&mut self, event: &Event) {
        if event.writer.is_operator() {
            return;
        }
        let named = event
            .refs
            .attempt
            .clone()
            .filter(|attempt| self.attempts.contains_key(attempt));
        let target = named.or_else(|| {
            let current = self.current()?;
            (event.writer.session() == Some(&current.session)).then(|| current.id.clone())
        });
        if let Some(attempt) = target.and_then(|id| self.attempts.get_mut(&id)) {
            attempt.heard(event);
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
///
/// A started attempt whose live deliveries are all accepted, with no work
/// signal after the latest one, is done: derived, never recorded.
pub fn progress(fold: &FeatureFold, require_human_review: bool) -> Progress {
    let Some(current) = fold.current() else {
        return if fold.unstarted_drop.is_some() {
            Progress::Dropped
        } else {
            Progress::Planned
        };
    };
    match current.status {
        WorkStatus::Started if current.accepted_at().is_some() => Progress::Done,
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

/// What the board shows for one step: progress, phase, claim and the
/// correction when that differs from the agent's last self-report. The one
/// place a reason is attached to a shown status.
#[derive(Debug, Clone)]
pub struct Shown {
    pub progress: Progress,
    pub phase: Option<Phase>,
    pub claim: Option<ClaimState>,
    pub correction: Option<Correction>,
}

/// Resolve the shown state of a step from its settle and its claim (`claim`
/// is the outcome for a started attempt the agent still holds open).
///
/// Rule 1b: when the claim on a step whose live deliveries are all accepted
/// ends (lost or silent), the owner's verdict is the last fact: done.
pub fn shown(
    fold: &FeatureFold,
    require_human_review: bool,
    claim: Option<crate::reduce::claim::Outcome>,
) -> Shown {
    let progress = progress(fold, require_human_review);
    let current = fold.current();
    let started = current.filter(|attempt| attempt.status == WorkStatus::Started);
    let phase = started
        .and_then(|attempt| attempt.settle.phase)
        .and_then(crate::reduce::settle::SettlePhase::open_phase);
    let settled = current.and_then(|attempt| attempt.settle.correction.clone());
    if let (Some(attempt), Some(outcome), Progress::Doing) = (started, &claim, progress)
        && outcome.claim.state == Lease::Free
        && let Some(verdict) = &attempt.settle.accepted
    {
        return Shown {
            progress: Progress::Done,
            phase: None,
            claim: None,
            correction: Some(Correction {
                rule: CorrectionRule::ClosedAfterAccept,
                since: outcome.claim.since,
                evidence: vec![verdict.review.clone(), attempt.last_word.1.clone()],
            }),
        };
    }
    let (claim, claimed) = match claim {
        Some(outcome) => (Some(outcome.claim), outcome.correction),
        None => (None, None),
    };
    Shown {
        progress,
        phase,
        claim,
        correction: settled.or(claimed),
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
                attempt.sessions.insert(body.session.clone());
                attempt.heard(event);
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

/// The owner's answer that lets an agent continue a step another agent
/// holds: the current answer of the owner to that agent's question on the
/// step, choosing the `takeover` option. An answer the owner changed, and an
/// answer to a withdrawn or superseded question, is none.
#[derive(Debug, Clone)]
pub struct TakeoverAnswer {
    pub ask: EventId,
    /// The session that asked. The grant is its own: a continuation of that
    /// session asks again.
    pub session: SessionId,
    pub answer: EventId,
}

impl TakeoverAnswer {
    /// Whether this answer covers `held`: the question came after that
    /// attempt started, from a session the attempt never belonged to (the
    /// holder's conversation, under any of its session ids, cannot grant
    /// itself), so the owner answered about it.
    pub fn covers(&self, held: &AttemptFold, causal: &CausalView<'_>) -> bool {
        !held.sessions.contains(&self.session) && causal.before(&held.id, &self.ask)
    }

    /// Whether `start` replaced `held` by applying this answer: the session
    /// that asked wrote the start, the start event itself cites the answer
    /// (a later event of the attempt citing it changes nothing), the answer
    /// covers `held`, and the start had seen every delivery on `held` (the
    /// owner did not answer about a delivered step).
    pub fn lets(&self, start: &AttemptFold, held: &AttemptFold, causal: &CausalView<'_>) -> bool {
        self.session == start.started_by
            && (start.causes.contains(&self.answer) || start.start_basis.contains(&self.answer))
            && causal.before(&self.answer, &start.id)
            && self.covers(held, causal)
            && held
                .deliveries
                .iter()
                .all(|delivery| causal.before(delivery, &start.id))
    }
}

/// Every takeover answer of a project that is in force, by step.
pub fn takeover_answers(
    asks: &BTreeMap<EventId, crate::reduce::decisions::AskFold>,
    receipts: &BTreeMap<EventId, crate::model::state::Receipt>,
    decisions: &BTreeMap<EventId, crate::model::state::DecisionState>,
) -> BTreeMap<NodePath, Vec<TakeoverAnswer>> {
    let mut answers: BTreeMap<NodePath, Vec<TakeoverAnswer>> = BTreeMap::new();
    for decision in decisions.values() {
        if decision.withdrawn_by.is_some() || decision.superseded_by.is_some() {
            continue;
        }
        let Some(receipt) = decision
            .effective_receipt
            .as_ref()
            .and_then(|id| receipts.get(id))
        else {
            continue;
        };
        if receipt.source != crate::model::state::ReceiptSource::Human
            || receipt.chose.id.as_str() != crate::model::cards::TAKEOVER_OPTION
        {
            continue;
        }
        let Some(ask) = asks.get(&decision.id) else {
            continue;
        };
        answers
            .entry(ask.node.clone())
            .or_default()
            .push(TakeoverAnswer {
                ask: ask.id.clone(),
                session: ask.session.clone(),
                answer: receipt.id.clone(),
            });
    }
    answers
}

/// Contested takeovers on one step: a start that replaced another session's
/// attempt without a hand-off or the owner's answer, while that session wrote a word on the
/// attempt the start had not seen (not in its causal past). Two agents
/// racing, or a host whose file arrived late, both end here; the owner says
/// who continues. Each is `(replaced attempt, the start that replaced it)`.
pub fn contested_takeovers(
    fold: &FeatureFold,
    causal: &CausalView<'_>,
    answers: &[TakeoverAnswer],
) -> Vec<(EventId, EventId)> {
    let mut contested = Vec::new();
    for start in fold.attempts.values() {
        if start
            .causes
            .iter()
            .any(|cause| fold.handoff_events.contains(cause))
        {
            continue;
        }
        for replaced in fold
            .attempts
            .values()
            .filter(|previous| start.causes.contains(&previous.id))
            .filter(|previous| previous.session != start.session)
        {
            // The owner let this agent continue it: their choice, as a
            // hand-off is.
            if answers
                .iter()
                .any(|answer| answer.lets(start, replaced, causal))
            {
                continue;
            }
            if replaced
                .words
                .iter()
                .any(|word| !causal.before(word, &start.id))
            {
                contested.push((replaced.id.clone(), start.id.clone()));
            }
        }
    }
    contested
}

/// The other session's unfinished attempt the current attempt replaced (by
/// citing it), for the returning agent's "freed, now with …" nudge.
pub fn taken_from(fold: &FeatureFold) -> Option<TakenFrom> {
    let current = fold.current()?;
    fold.attempts
        .values()
        .filter(|previous| current.causes.contains(&previous.id))
        .filter(|previous| previous.session != current.session)
        .filter(|previous| previous.status == WorkStatus::Started)
        .max_by_key(|previous| previous.order)
        .map(|previous| TakenFrom {
            attempt: previous.id.clone(),
            session: previous.session.clone(),
            at: current.started_at,
            granted_by: None,
        })
}

/// Project a feature fold into its derived state.
#[allow(clippy::too_many_arguments)]
pub fn build_feature(
    fold: &FeatureFold,
    fields: FeatureFields,
    accepted_depends_on: Vec<NodePath>,
    shown: Shown,
    gate: Gate,
    waits_on: Vec<GraphNodeId>,
    unblocks: Vec<NodePath>,
    liveness: Liveness,
    eta: Option<EtaState>,
    assumptions: Vec<EventId>,
    taints: Vec<EventId>,
    require_human_review: bool,
    untouched: bool,
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
        progress: shown.progress,
        phase: shown.phase,
        claim: shown.claim,
        correction: shown.correction,
        taken_from: taken_from(fold),
        takeover_grants: Vec::new(),
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
        untouched,
        owner_drop: fold
            .owner_drop()
            .filter(|_| shown.progress == Progress::Dropped)
            .map(|drop| crate::model::state::OwnerDrop {
                event: drop.event.clone(),
                at: drop.ts,
                note: drop.note.clone(),
            }),
    }
}
