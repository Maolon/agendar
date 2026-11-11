//! Deterministic reducer orchestration: event-set resolution, the causal fold,
//! and the final derived projections.
//!
//! `reduce` is pure: no clock reads, RNG, filesystem, database, host-specific pid
//! facts or writes. The same event multiset and the same `as_of` produce
//! byte-identical canonical state.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::event::{BudgetUsage, Event, EventPayload, PolicySet, RuleSet, WorkStatus};
use crate::model::ids::{EventId, HostId, NodePath, ProjectSlug, SessionId, UtcTime};
use crate::model::state::{
    Basis, Calibration, DecisionState, Diagnostic, FeatureState, Liveness, NeedsYouItem, Progress,
    ProjectState, ProjectSummary, Receipt, RuleState, SessionState, State, UNTOUCHED_AFTER_S,
    Variance, policy_revision, rule_revision,
};

pub mod activity;
pub mod assumptions;
pub mod attention;
pub mod brief;
pub mod budget;
pub mod calibration;
pub mod claim;
pub mod decisions;
pub mod eta;
pub mod evidence;
pub mod forecast;
pub mod graph;
pub mod moot;
pub mod motion;
pub mod nudges;
pub mod order;
pub mod ordering;
pub mod plan;
pub mod policy;
pub mod settle;
pub mod snooze;
pub mod stats;
pub mod usage;
pub mod waits;

pub use decisions::{AckFold, AskFold};
pub use order::{
    CausalIndex, Resolution, VariantState, VariantStatus, resolve, resolve_with_quarantine,
};

/// Outcome of one replicated semantic check performed while folding.
#[derive(Debug, Clone)]
pub enum FoldNote {
    /// The event is rejected: it must not fold and its descendants stay pending.
    Invalid(String),
    /// The event is a legitimate historical fact without projection effect.
    History(String),
}

impl FoldNote {
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid(detail.into())
    }

    pub fn history(detail: impl Into<String>) -> Self {
        Self::History(detail.into())
    }
}

/// Evaluation context: the only source of "now" for the reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReduceContext {
    pub as_of: UtcTime,
}

/// Read-only causal view used by fold helpers.
pub struct CausalView<'a> {
    pub order_index: &'a BTreeMap<EventId, usize>,
    pub ancestors: &'a CausalIndex,
}

impl CausalView<'_> {
    pub fn order_of(&self, id: &EventId) -> usize {
        self.order_index.get(id).copied().unwrap_or(usize::MAX)
    }

    pub fn before(&self, earlier: &EventId, later: &EventId) -> bool {
        self.ancestors.before(earlier, later)
    }
}

/// Fold state of one project while events are applied in `O` order.
#[derive(Debug)]
pub struct ProjectFold {
    pub slug: ProjectSlug,
    pub declaration: plan::Declaration,
    pub features: BTreeMap<NodePath, plan::FeatureFold>,
    pub asks: BTreeMap<EventId, AskFold>,
    pub receipts: BTreeMap<EventId, Receipt>,
    pub acks: BTreeMap<EventId, decisions::AckFold>,
    pub sessions: BTreeMap<SessionId, plan::SessionFold>,
    pub rules: BTreeMap<crate::model::ids::RuleId, (RuleSet, EventId)>,
    pub policy: Option<(PolicySet, EventId)>,
    pub evidence: evidence::EvidenceFold,
    pub diagnostics: Vec<Diagnostic>,
    pub last_event_ts: Option<UtcTime>,
    pub host_as_of: BTreeMap<HostId, UtcTime>,
    pub declaration_event: Option<EventId>,
    /// Assumption receipts consumed by a choice that cited the attempt.
    pub assumed_by_attempt: BTreeMap<EventId, BTreeSet<EventId>>,
    /// Published reports in O-order.
    pub reports: Vec<crate::model::state::ReportState>,
    /// The latest event on each step and whether the owner wrote any.
    pub touches: BTreeMap<NodePath, plan::Touch>,
}

impl ProjectFold {
    fn new(slug: ProjectSlug) -> Self {
        Self {
            slug,
            declaration: plan::Declaration::default(),
            features: BTreeMap::new(),
            asks: BTreeMap::new(),
            receipts: BTreeMap::new(),
            acks: BTreeMap::new(),
            sessions: BTreeMap::new(),
            rules: BTreeMap::new(),
            policy: None,
            evidence: evidence::EvidenceFold::default(),
            diagnostics: Vec::new(),
            last_event_ts: None,
            host_as_of: BTreeMap::new(),
            declaration_event: None,
            assumed_by_attempt: BTreeMap::new(),
            reports: Vec::new(),
            touches: BTreeMap::new(),
        }
    }

    fn policy_value(&self) -> PolicySet {
        self.policy
            .as_ref()
            .map(|(value, _)| value.clone())
            .unwrap_or_else(policy::default_policy)
    }

    fn policy_source(&self) -> Option<EventId> {
        self.policy.as_ref().map(|(_, source)| source.clone())
    }

    fn feature(&mut self, node: &NodePath) -> &mut plan::FeatureFold {
        self.features
            .entry(node.clone())
            .or_insert_with(|| plan::FeatureFold::new(node.clone()))
    }

    fn diagnostic(&mut self, event: &Event, code: &str, message: String) {
        self.diagnostics.push(Diagnostic::new(
            format!("{code}:{}", event.id.as_str()),
            code,
            Some(self.slug.clone()),
            Some(event.id.clone()),
            message,
        ));
    }

    /// Record a replicated semantic outcome. Invalid events are reported to the
    /// caller so eligibility can be recomputed; historical-only events keep
    /// their diagnostic without changing projections.
    fn note(
        &mut self,
        event: &Event,
        note: Option<FoldNote>,
        rejected: &mut BTreeMap<EventId, String>,
    ) {
        match note {
            None => {}
            Some(FoldNote::Invalid(detail)) => {
                rejected.insert(event.id.clone(), detail.clone());
                self.diagnostic(event, "semantic_invalid", detail);
            }
            Some(FoldNote::History(detail)) => {
                self.diagnostic(event, "semantic_history", detail);
            }
        }
    }
}

/// Reduce one event multiset at one evaluation time.
pub fn reduce(events: &[Event], ctx: &ReduceContext) -> State {
    reduce_with_resolution(events, ctx, None).state
}

/// A converged reduction plus the resolution that produced it.
#[derive(Debug, Clone)]
pub struct ReduceOutput {
    pub state: State,
    pub resolution: Resolution,
}

/// Calibration evidence gathered while folding one event set. It is not part of
/// the convergent `State`; the reducer exposes it for tests and analytics that
/// must see attempt samples even while a project is still ineligible.
#[derive(Debug, Clone, Default)]
pub struct ReduceReport {
    pub calibration: BTreeMap<ProjectSlug, calibration::Report>,
}

/// Reduce and also return the final resolution (the index persists its variant
/// statuses, so it must observe the same semantic quarantine the state used).
pub fn reduce_with_resolution(
    events: &[Event],
    ctx: &ReduceContext,
    initial: Option<&Resolution>,
) -> ReduceOutput {
    let (state, resolution, _) = reduce_impl(events, ctx, initial);
    ReduceOutput {
        state,
        resolution: resolution.into_owned(),
    }
}

/// One reduction that keeps both resolutions of its event set.
#[derive(Debug, Clone)]
pub struct ReduceKept {
    pub state: State,
    /// `resolve(events)`: what [`reduce_resolved`] starts from when a reader
    /// reloads the same event set.
    pub resolved: Resolution,
    /// The final resolution after semantic quarantine, when it differs from
    /// `resolved` (the index persists its variant statuses).
    pub quarantined: Option<Resolution>,
}

impl ReduceKept {
    /// The resolution the state was folded with.
    pub fn last(&self) -> &Resolution {
        self.quarantined.as_ref().unwrap_or(&self.resolved)
    }
}

/// [`reduce_with_resolution`] with no initial resolution, also keeping the
/// plain resolution: the state equals `reduce_resolved(&resolved, events, ctx)`
/// because that is exactly the call made here.
pub fn reduce_keeping_resolution(events: &[Event], ctx: &ReduceContext) -> ReduceKept {
    let resolved = resolve(events);
    let (state, last, _) = reduce_impl(events, ctx, Some(&resolved));
    let quarantined = match last {
        Cow::Borrowed(_) => None,
        Cow::Owned(resolution) => Some(resolution),
    };
    ReduceKept {
        state,
        resolved,
        quarantined,
    }
}

/// Reduce and also return the calibration report of every project.
pub fn reduce_with_report(events: &[Event], ctx: &ReduceContext) -> (State, ReduceReport) {
    let (state, _, report) = reduce_impl(events, ctx, None);
    (state, report)
}

fn reduce_impl<'a>(
    events: &[Event],
    ctx: &ReduceContext,
    initial: Option<&'a Resolution>,
) -> (State, Cow<'a, Resolution>, ReduceReport) {
    let mut quarantine: BTreeMap<EventId, String> = BTreeMap::new();
    // A caller's resolution is only borrowed: it is copied only if a semantic
    // rejection forces a new one or the caller asks for it back.
    let mut resolution = match initial {
        Some(resolution) => Cow::Borrowed(resolution),
        None => Cow::Owned(resolve(events)),
    };
    // Replicated semantic rejections feed back into eligibility: a rejected fact
    // must not fold, and its causal descendants stay pending. Each pass is a
    // deterministic function of the event set, and the seed set only grows.
    for _ in 0..=events.len() {
        let (state, rejected, report) = fold(&resolution, events, ctx);
        let mut changed = false;
        for (id, reason) in rejected {
            if let std::collections::btree_map::Entry::Vacant(entry) = quarantine.entry(id) {
                entry.insert(reason);
                changed = true;
            }
        }
        if !changed {
            return (state, resolution, report);
        }
        resolution = Cow::Owned(resolve_with_quarantine(events, &quarantine));
    }
    let (state, _, report) = fold(&resolution, events, ctx);
    (state, resolution, report)
}

/// Reduce an already resolved event set (the index reuses its resolution).
pub fn reduce_resolved(
    resolution: &Resolution,
    all_events: &[Event],
    ctx: &ReduceContext,
) -> State {
    reduce_impl(all_events, ctx, Some(resolution)).0
}

fn fold(
    resolution: &Resolution,
    all_events: &[Event],
    ctx: &ReduceContext,
) -> (State, BTreeMap<EventId, String>, ReduceReport) {
    let mut by_id: BTreeMap<EventId, Event> = BTreeMap::new();
    for event in &resolution.ordered {
        by_id.insert(event.id.clone(), event.clone());
    }
    let mut rejected: BTreeMap<EventId, String> = BTreeMap::new();
    let mut event_ts: BTreeMap<EventId, UtcTime> = BTreeMap::new();
    for event in &resolution.ordered {
        event_ts.insert(event.id.clone(), event.ts);
    }
    let causal = CausalView {
        order_index: &resolution.order_index,
        ancestors: &resolution.ancestors,
    };

    let mut folds: BTreeMap<ProjectSlug, ProjectFold> = BTreeMap::new();
    // A snooze is display only: it is read once, here, and skipped by the
    // fold below (see `snooze`).
    let snoozes = snooze::latest(&resolution.ordered);
    for event in all_events.iter().filter(|event| !snooze::is_snooze(event)) {
        folds
            .entry(event.project.clone())
            .or_insert_with(|| ProjectFold::new(event.project.clone()));
    }

    // The owner's drops are known before any event folds, so whether a drop
    // closes an attempt never depends on where the two fall in `O`.
    for event in &resolution.ordered {
        if let (EventPayload::PlanDropped(_), Some(node)) = (&event.payload, &event.refs.node) {
            folds
                .entry(event.project.clone())
                .or_insert_with(|| ProjectFold::new(event.project.clone()))
                .feature(node)
                .record_owner_drop(event);
        }
    }

    for (order, event) in resolution.ordered.iter().enumerate() {
        if snooze::is_snooze(event) {
            continue;
        }
        let fold = folds
            .entry(event.project.clone())
            .or_insert_with(|| ProjectFold::new(event.project.clone()));
        fold.last_event_ts = Some(match fold.last_event_ts {
            Some(current) => current.max(event.ts),
            None => event.ts,
        });
        let host_entry = fold
            .host_as_of
            .entry(event.writer.host().clone())
            .or_insert(event.ts);
        *host_entry = (*host_entry).max(event.ts);
        plan::touch_session(&mut fold.sessions, event, order);
        plan::Touch::record(&mut fold.touches, event);

        match &event.payload {
            EventPayload::ProjectDeclared(_) => {
                if fold.declaration_event.is_none() {
                    fold.declaration_event = Some(event.id.clone());
                }
                fold.declaration.apply(event, order);
            }
            EventPayload::PlanFeature(_) => {
                let node = event.refs.node.clone();
                if let Some(node) = node {
                    fold.feature(&node).apply(event, order);
                }
            }
            // Recorded before the fold and applied after it.
            EventPayload::PlanDropped(_) => {}
            // Skipped above.
            EventPayload::NeedSnoozed(_) => {}
            EventPayload::WorkProgress(_) => {
                let node = event.refs.node.clone();
                if let Some(node) = node {
                    let note = fold.feature(&node).apply_progress(event, order, &causal);
                    fold.note(event, note, &mut rejected);
                }
            }
            EventPayload::Pulse(_) => {
                let node = event.refs.node.clone();
                if let Some(node) = node {
                    fold.feature(&node).apply_pulse(event, order);
                }
            }
            EventPayload::DecisionAsk(body) => {
                // A question on the step is a word on its attempt.
                if let Some(feature) = event
                    .refs
                    .node
                    .as_ref()
                    .and_then(|node| fold.features.get_mut(node))
                {
                    feature.heard(event);
                }
                // Revalidate against the declared causal policy snapshot (that
                // event, or balanced when absent), never a concurrent policy the
                // author could not have observed.
                let declared = declared_policy(&by_id, body.policy.policy_event.as_ref());
                let note = match declared {
                    Ok((value, source)) => {
                        let evaluated = policy::evaluate_with_policy(
                            &body.card,
                            body.policy.requested,
                            &value,
                            source,
                        );
                        decisions::apply_ask(&mut fold.asks, event, order, &evaluated, &value)
                    }
                    Err(detail) => Some(FoldNote::invalid(detail)),
                };
                fold.note(event, note, &mut rejected);
            }
            EventPayload::DecisionAnswer(_) => {
                let note =
                    decisions::apply_answer(&mut fold.asks, &mut fold.receipts, event, order);
                fold.note(event, note, &mut rejected);
            }
            EventPayload::DecisionAssumed(body) | EventPayload::DecisionAuto(body) => {
                let declared = declared_policy(&by_id, body.policy.policy_event.as_ref());
                let note = match declared {
                    Ok((value, source)) => {
                        let current_policy = fold.policy_value();
                        decisions::apply_choice(
                            &mut fold.asks,
                            &mut fold.receipts,
                            &mut fold.features,
                            &mut fold.assumed_by_attempt,
                            event,
                            order,
                            &value,
                            source,
                            &current_policy,
                            &causal,
                        )
                    }
                    Err(detail) => Some(FoldNote::invalid(detail)),
                };
                fold.note(event, note, &mut rejected);
            }
            EventPayload::DecisionWithdrawn(_) => {
                let note = decisions::apply_withdrawn(&mut fold.asks, event, order, &causal);
                fold.note(event, note, &mut rejected);
            }
            EventPayload::ReportPublished(body) => {
                fold.reports.push(crate::model::state::ReportState {
                    id: event.id.clone(),
                    node: event.refs.node.clone(),
                    title: body.title.clone(),
                    summary: body.summary.clone(),
                    report: body.report.clone(),
                    kind: body.kind,
                    domain: body.domain,
                    ts: event.ts,
                    session: body.agent.session.clone(),
                    harness: body.agent.harness,
                });
            }
            EventPayload::DecisionAck(_) => {
                let note = decisions::apply_ack(
                    &mut fold.asks,
                    &mut fold.acks,
                    &fold.receipts,
                    event,
                    order,
                );
                fold.note(event, note, &mut rejected);
            }
            EventPayload::RuleSet(body) => {
                fold.rules
                    .insert(body.rule_id.clone(), (body.clone(), event.id.clone()));
            }
            EventPayload::PolicySet(body) => match policy::validate_policy(body) {
                Ok(()) => {
                    fold.policy = Some((body.clone(), event.id.clone()));
                }
                Err(error) => {
                    fold.note(
                        event,
                        Some(FoldNote::invalid(error.to_string())),
                        &mut rejected,
                    );
                }
            },
            EventPayload::SessionLost(_) => {
                plan::apply_session_lost(&mut fold.sessions, event, order);
            }
            EventPayload::SessionContinued(_) => {
                plan::apply_session_continued(&fold.sessions, &mut fold.features, event);
            }
            EventPayload::Delivery(_) => {
                if let Some(feature) = event
                    .refs
                    .node
                    .as_ref()
                    .and_then(|node| fold.features.get_mut(node))
                {
                    feature.heard(event);
                }
                fold.evidence.apply(event);
            }
            EventPayload::EvidenceObserved(_) | EventPayload::ReviewResult(_) => {
                fold.evidence.apply(event)
            }
            EventPayload::Delivered(_) => {}
        }
    }

    for fold in folds.values_mut() {
        for feature in fold.features.values_mut() {
            feature.settle_owner_drops(&causal);
        }
    }

    let mut projects: BTreeMap<ProjectSlug, ProjectState> = BTreeMap::new();
    let mut diagnostics = resolution.diagnostics.clone();
    let mut report = ReduceReport::default();
    for (slug, fold) in &mut folds {
        let (mut project, project_diagnostics, calibration) =
            finalize_project(fold, ctx.as_of, &event_ts, &resolution.order_index, &causal);
        snooze::apply(&mut project, &snoozes, ctx.as_of);
        diagnostics.extend(project_diagnostics);
        projects.insert(slug.clone(), project);
        report.calibration.insert(slug.clone(), calibration);
    }
    diagnostics.sort_by(|left, right| left.id.cmp(&right.id));
    diagnostics.dedup_by(|left, right| left.id == right.id);

    (
        State {
            projects,
            diagnostics,
            event_set_digest: resolution.event_set_digest.clone(),
            evaluated_at: ctx.as_of,
        },
        rejected,
        report,
    )
}

/// Resolve the policy a replicated event declares as its causal snapshot.
///
/// `None` means the initial balanced default; a present source must be an
/// eligible, valid `policy.set` the author could actually have observed.
fn declared_policy(
    eligible: &BTreeMap<EventId, Event>,
    source: Option<&EventId>,
) -> std::result::Result<(PolicySet, Option<EventId>), String> {
    match source {
        None => Ok((policy::default_policy(), None)),
        Some(id) => {
            let Some(event) = eligible.get(id) else {
                return Err("the declared policy source is not in the causal view".to_owned());
            };
            let EventPayload::PolicySet(value) = &event.payload else {
                return Err("the declared policy source is not a `policy.set`".to_owned());
            };
            policy::validate_policy(value).map_err(|error| error.to_string())?;
            Ok((value.clone(), Some(id.clone())))
        }
    }
}

fn finalize_project(
    fold: &mut ProjectFold,
    as_of: UtcTime,
    event_ts: &BTreeMap<EventId, UtcTime>,
    order_index: &BTreeMap<EventId, usize>,
    causal: &CausalView<'_>,
) -> (ProjectState, Vec<Diagnostic>, calibration::Report) {
    let mut diagnostics = fold.diagnostics.clone();
    let policy_value = fold.policy_value();
    let policy_source = fold.policy_source();
    let policy_state = crate::model::state::PolicyState {
        value: policy_value.clone(),
        revision: policy_revision(&fold.slug, policy_source.as_ref()),
    };

    // 1. Resolve plan fields and declared dependencies.
    let mut field_views: BTreeMap<NodePath, plan::FeatureFields> = BTreeMap::new();
    let mut declared_deps: Vec<graph::DeclaredDeps> = Vec::new();
    for (node, feature) in &fold.features {
        let fields = plan::fields(feature);
        if !fields.complete {
            diagnostics.push(Diagnostic::new(
                format!("feature_incomplete:{}", node.as_str()),
                "feature_incomplete",
                Some(fold.slug.clone()),
                None,
                format!(
                    "Feature {} has no complete first declaration (title, slice and demo kind are required).",
                    node.as_str()
                ),
            ));
        }
        if let (Some(source), true) = (
            fields.field_sources.get("depends_on").cloned(),
            !fields.depends_on.is_empty(),
        ) {
            declared_deps.push(graph::DeclaredDeps {
                node: node.clone(),
                depends_on: fields.depends_on.clone(),
                source,
            });
        }
        field_views.insert(node.clone(), fields);
    }
    let known: BTreeSet<NodePath> = fold.features.keys().cloned().collect();
    let plan_edges = graph::plan_edges(&declared_deps, &known, order_index);
    for edge in &plan_edges.missing {
        diagnostics.push(Diagnostic::new(
            format!(
                "missing_upstream:{}:{}",
                edge.downstream.as_str(),
                edge.upstream.as_str()
            ),
            "missing_upstream",
            Some(fold.slug.clone()),
            Some(edge.source_event.clone()),
            format!(
                "Feature {} depends on undeclared feature {}; dependent work stays blocked.",
                edge.downstream.as_str(),
                edge.upstream.as_str()
            ),
        ));
    }

    // 2. Decisions: receipts, effective answers and lifecycles.
    let mut decision_states: BTreeMap<EventId, DecisionState> =
        decisions::finalize(&mut fold.asks, &fold.acks, &fold.receipts, causal);
    applied_sessions(fold, &mut decision_states);

    // 2a. Assumption consumption per current attempt, then `built_on`.
    let effective_by_ask: BTreeMap<EventId, Option<EventId>> = decision_states
        .iter()
        .map(|(ask, state)| (ask.clone(), state.effective_receipt.clone()))
        .collect();
    let mut consumption_by_node: BTreeMap<NodePath, assumptions::Consumption> = BTreeMap::new();
    for (node, feature) in &fold.features {
        consumption_by_node.insert(
            node.clone(),
            assumptions::feature_consumption(
                feature,
                &fold.assumed_by_attempt,
                &fold.asks,
                &effective_by_ask,
                &fold.receipts,
            ),
        );
    }
    let mut built_on = assumptions::built_on_by_ask(&consumption_by_node, &fold.receipts);
    for (ask, state) in &mut decision_states {
        state.built_on = built_on.remove(ask).unwrap_or_default();
    }

    // 2b. Deliveries, Seen observations and reviews.
    let delivery_states = evidence::finalize(
        &fold.evidence,
        &decision_states,
        &fold.receipts,
        causal,
        order_index,
    );
    let (review_pending, review_accepted) = evidence::review_gates(&delivery_states);

    // 2c. Derived settle per attempt: in review, sent back or done from its own
    // live deliveries' reviews. A drop ordered after the accept is history.
    for (node, dropped) in
        settle::apply(&mut fold.features, &delivery_states, order_index, event_ts)
    {
        diagnostics.push(Diagnostic::new(
            format!("semantic_history:{}", dropped.as_str()),
            "semantic_history",
            Some(fold.slug.clone()),
            Some(dropped.clone()),
            format!(
                "The owner accepted {} before this drop; the drop is history only.",
                node.as_str()
            ),
        ));
    }

    // 3. Session liveness.
    let mut session_states: BTreeMap<SessionId, SessionState> = BTreeMap::new();
    for (session, session_fold) in &fold.sessions {
        let liveness = plan::session_liveness(
            session_fold.lost_event.is_some(),
            session_fold.last_event_ts,
            as_of,
        );
        session_states.insert(
            session.clone(),
            SessionState {
                id: session.clone(),
                host: session_fold.host.clone(),
                harness: session_fold.harness,
                last_event: session_fold.last_event.clone(),
                last_event_ts: session_fold.last_event_ts,
                current_node: session_fold.current_node.clone(),
                liveness,
                lost_event: session_fold.lost_event.clone(),
                stalled_on: Vec::new(),
            },
        );
    }

    // 4. ETAs and claims, then the shown progress, then gates (upstream
    // progress is known once every step is shown).
    let waits_by_attempt = waits_by_attempt(fold, &mut diagnostics);
    let calibration_report = calibration::report(&fold.features, &waits_by_attempt, as_of);
    // Open mandatory decisions per feature, in decision order.
    let mut open_by_node: BTreeMap<NodePath, Vec<EventId>> = BTreeMap::new();
    for (ask, decision) in &decision_states {
        if decision.lifecycle == crate::model::state::DecisionLifecycle::Open
            && decision.mode == crate::model::ids::Mode::Ask
        {
            open_by_node
                .entry(decision.node.clone())
                .or_default()
                .push(ask.clone());
        }
    }
    // Questions on steps dropped or accepted a day ago fold and stop holding.
    let closed_steps = moot::closed_steps(&fold.features, event_ts);
    let folded = moot::folded(&decision_states, &closed_steps, as_of);
    let holds = claim::SessionHolds::build(&fold.asks, &decision_states, &folded);
    let mut upstream_adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for edge in &plan_edges.accepted {
        upstream_adjacency
            .entry(edge.upstream.clone())
            .or_default()
            .insert(edge.downstream.clone());
    }
    // An attempt with a delivery waiting for review is the owner's.
    let reviewing: BTreeSet<&EventId> = delivery_states
        .values()
        .filter(|delivery| {
            delivery.superseded_by.is_none()
                && delivery.review == crate::model::state::ReviewStatus::Pending
        })
        .map(|delivery| &delivery.attempt)
        .collect();
    let mut etas: BTreeMap<NodePath, crate::model::state::EtaState> = BTreeMap::new();
    let mut shown_map: BTreeMap<NodePath, plan::Shown> = BTreeMap::new();
    for (node, feature) in &fold.features {
        let eta = feature
            .current()
            .filter(|attempt| attempt.status != WorkStatus::Dropped)
            .map(|attempt| {
                let pauses = waits_by_attempt
                    .get(&attempt.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let ratio = calibration_report.ratio_for(attempt.harness);
                plan::attempt_eta(attempt, pauses, as_of, ratio)
            });
        let fields = &field_views[node];
        let outcome = feature
            .current()
            .filter(|attempt| attempt.status == WorkStatus::Started)
            .filter(|attempt| attempt.accepted_at().is_none())
            .filter(|attempt| attempt.settle.phase != Some(settle::SettlePhase::InReview))
            .filter(|attempt| !reviewing.contains(&attempt.id))
            .filter(|_| !fields.paused && !fields.handoff_pending)
            .map(|attempt| {
                let lost = session_states
                    .get(&attempt.session)
                    .and_then(|session| session.lost_event.as_ref())
                    .map(|lost| (lost, event_ts.get(lost).copied().unwrap_or(as_of)));
                let decision = open_by_node
                    .get(node)
                    .and_then(|asks| asks.first().cloned())
                    .or_else(|| {
                        // Only an ETA held by a human wait can rest on an
                        // upstream question.
                        eta.as_ref().filter(|eta| eta.paused)?;
                        graph::ancestors(&upstream_adjacency, node)
                            .iter()
                            .find_map(|upstream| {
                                open_by_node
                                    .get(upstream)?
                                    .iter()
                                    .find(|ask| !folded.contains(*ask))
                                    .cloned()
                            })
                    });
                claim::claim(
                    attempt,
                    &claim::ClaimInputs {
                        lost,
                        eta: eta.as_ref(),
                        decision,
                        holds: &holds,
                        as_of,
                    },
                )
            });
        shown_map.insert(
            node.clone(),
            plan::shown(feature, policy_value.require_human_review, outcome),
        );
        if let Some(eta) = eta {
            etas.insert(node.clone(), eta);
        }
    }
    let progress_map: BTreeMap<NodePath, Progress> = shown_map
        .iter()
        .map(|(node, shown)| (node.clone(), shown.progress))
        .collect();
    let mut dependency_view = plan::DependencyView::default();
    for edge in &plan_edges.accepted {
        dependency_view
            .accepted_upstreams
            .entry(edge.downstream.clone())
            .or_default()
            .push(edge.upstream.clone());
    }
    for edge in &plan_edges.rejected {
        dependency_view
            .rejected_upstreams
            .entry(edge.downstream.clone())
            .or_default()
            .insert(edge.upstream.clone());
    }
    for edge in &plan_edges.missing {
        dependency_view
            .missing_upstreams
            .entry(edge.downstream.clone())
            .or_default()
            .insert(edge.upstream.clone());
    }
    let mut unblocks_by_node = graph::all_descendants(&known, &plan_edges.accepted);
    // Human waits are replayed from causal transitions: accepted dependency
    // edges, upstream restarts and completions, and ask answers decide when a
    // hold opens and closes. A later agenda edit therefore cannot rewrite a
    // wait that was already spent, and calibration measures
    // active duration against the ETA the agent declared at start.
    // Accepted upstreams per feature, in accepted-edge order.
    let mut accepted_by_downstream: BTreeMap<NodePath, Vec<NodePath>> = BTreeMap::new();
    for edge in &plan_edges.accepted {
        accepted_by_downstream
            .entry(edge.downstream.clone())
            .or_default()
            .push(edge.upstream.clone());
    }
    let human_decided = plan::human_decided(fold.receipts.values());
    let takeover_answers = plan::takeover_answers(&fold.asks, &fold.receipts, &decision_states);
    let mut feature_states: BTreeMap<NodePath, FeatureState> = BTreeMap::new();
    for (node, feature) in &fold.features {
        let fields = field_views
            .get(node)
            .cloned()
            .expect("every folded feature has resolved fields");
        let shown = shown_map
            .remove(node)
            .expect("every folded feature has a shown state");
        let progress = shown.progress;
        let pending_decisions = open_by_node.remove(node).unwrap_or_default();
        let mut context = plan::FeatureContext::default();
        for upstream in &fields.depends_on {
            if let Some(value) = progress_map.get(upstream) {
                context.upstream_progress.insert(upstream.clone(), *value);
            }
            if review_pending.contains(upstream) {
                context.upstream_pending_review.insert(upstream.clone());
            }
            if review_accepted.contains(upstream) {
                context.upstream_review_accepted.insert(upstream.clone());
            }
            if consumption_by_node
                .get(upstream)
                .is_some_and(|consumption| !consumption.tainted.is_empty())
            {
                context.upstream_tainted.insert(upstream.clone());
            }
        }
        let (gate, waits_on) = plan::gate_and_waits(
            node,
            &fields,
            progress,
            &dependency_view,
            &context,
            &pending_decisions,
            policy_value.require_human_review,
        );
        let liveness = feature
            .current()
            .and_then(|attempt| session_states.get(&attempt.session))
            .map(|session| session.liveness)
            .unwrap_or(Liveness::Quiet);
        let accepted_depends_on = accepted_by_downstream.remove(node).unwrap_or_default();
        let unblocks = unblocks_by_node.remove(node).unwrap_or_default();
        let eta = etas.remove(node);
        let consumption = consumption_by_node.get(node).cloned().unwrap_or_default();
        let assumptions = consumption.unconfirmed.clone();
        let taints = consumption.tainted.clone();
        // Untouched: only agents wrote it, nobody started it, no question on
        // it, no event on it for UNTOUCHED_AFTER_S. Event time only.
        let untouched = progress == Progress::Planned
            && pending_decisions.is_empty()
            && !human_decided.contains(node)
            && fold.touches.get(node).is_some_and(|touch| {
                !touch.owner && touch.last.add_seconds(UNTOUCHED_AFTER_S) <= as_of
            });
        let state = plan::build_feature(
            feature,
            fields,
            accepted_depends_on,
            shown,
            gate,
            waits_on,
            unblocks,
            liveness,
            eta,
            assumptions,
            taints,
            policy_value.require_human_review,
            untouched,
        );
        let mut state = state;
        // The owner's takeover answers on this step: the one the current
        // start applied, and the current ones that let another agent start.
        if let Some(answers) = takeover_answers.get(node)
            && let Some(current) = feature.current()
        {
            if let Some(from) = state.taken_from.as_mut()
                && let Some(replaced) = feature.attempts.get(&from.attempt)
            {
                from.granted_by = answers
                    .iter()
                    .find(|answer| answer.lets(current, replaced, causal))
                    .map(|answer| answer.answer.clone());
            }
            // Shown only while the step can be taken by it: in progress and
            // under no hold of the owner (a pause, an open question, an old
            // hand-off, a delivery in review).
            if current.status == WorkStatus::Started
                && state.progress == Progress::Doing
                && state.gate != crate::model::state::Gate::AwaitingDecision
                && state.phase != Some(crate::model::state::Phase::InReview)
            {
                state.takeover_grants = answers
                    .iter()
                    .filter(|answer| answer.covers(current, causal))
                    .map(|answer| crate::model::state::TakeoverGrant {
                        session: answer.session.clone(),
                        ask: answer.ask.clone(),
                        answer: answer.answer.clone(),
                    })
                    .collect();
            }
        }
        feature_states.insert(node.clone(), state);
    }

    // 5. Rules, policy wrappers and the speculation budget.
    let mut rules: Vec<RuleState> = fold
        .rules
        .iter()
        .map(|(rule_id, (value, source))| RuleState {
            value: value.clone(),
            revision: rule_revision(&fold.slug, rule_id, Some(source)),
        })
        .collect();
    rules.sort_by(|left, right| left.value.rule_id.cmp(&right.value.rule_id));

    let speculative_features =
        budget::speculative_features(&feature_states, &decision_states, &fold.receipts);
    let pending_slices = budget::pending_slices(&delivery_states, &feature_states);
    let (demo_active, earliest_active_start, demo_gap_node) =
        demo_activity(fold, &feature_states, &delivery_states);
    let last_demo = fold
        .evidence
        .deliveries
        .values()
        .map(|event| event.ts)
        .max();
    let demo_gap_s = budget::demo_gap(as_of, last_demo, earliest_active_start, demo_active);
    let activity = budget::Activity {
        speculative_features,
        pending_slices,
        demo_gap_s,
        demo_step: budget::DemoStep::of(demo_gap_node, &feature_states),
    };
    let budget_usage = budget::usage(&policy_value.budget, activity);

    // 6. Summary placeholders, then attention rows, then final summary.
    let undeclared = fold.declaration.title.is_none();
    if undeclared {
        diagnostics.push(Diagnostic::new(
            format!("project_undeclared:{}", fold.slug.as_str()),
            "project_undeclared",
            Some(fold.slug.clone()),
            None,
            "Project has not been declared.".to_owned(),
        ));
    }
    let summary_inputs = SummaryInputs {
        budget: &budget_usage,
        calibration: &calibration_report.calibration,
        demo_active,
        demo_limit_s: policy_value.budget.max_demo_gap_s,
    };
    let summary = build_summary(
        fold,
        as_of,
        &feature_states,
        &session_states,
        &[],
        &decision_states,
        &delivery_states,
        &summary_inputs,
    );
    let mut provisional = ProjectState {
        summary,
        features: feature_states,
        decisions: decision_states,
        receipts: fold.receipts.clone(),
        deliveries: delivery_states,
        rules,
        policy: policy_state,
        sessions: session_states,
        needs_you: Vec::new(),
        reports: fold
            .reports
            .iter()
            .rev()
            .take(crate::model::state::MAX_REPORTS)
            .cloned()
            .collect(),
    };

    let start_conflicts = start_conflicts(fold, causal, &provisional.decisions);
    let lost_sessions: Vec<attention::LostSession> = provisional
        .sessions
        .values()
        .filter_map(|session| {
            session
                .lost_event
                .as_ref()
                .map(|lost| attention::LostSession {
                    session: session.id.clone(),
                    lost_event: lost.clone(),
                    node: session.current_node.clone(),
                    since: event_ts.get(lost).copied().unwrap_or(as_of),
                })
        })
        .collect();
    let dropped_attempts: BTreeSet<EventId> = fold
        .features
        .values()
        .flat_map(|feature| feature.attempts.values())
        .filter(|attempt| attempt.status == WorkStatus::Dropped)
        .map(|attempt| attempt.id.clone())
        .collect();
    let needs = {
        let inputs = attention::RescueInputs {
            rejected: &plan_edges.rejected,
            start_conflicts: &start_conflicts,
            lost_sessions: &lost_sessions,
            event_ts,
            dropped_attempts: &dropped_attempts,
            closed_steps: &closed_steps,
            as_of,
        };
        attention::build_needs(&provisional, &inputs)
    };
    let stalled = attention::stalled_by_row(&needs, &provisional);
    for session in provisional.sessions.values_mut() {
        let mut rows: Vec<String> = Vec::new();
        for (source, sessions) in &stalled {
            if sessions.contains(&session.id) {
                rows.push(
                    needs
                        .iter()
                        .find(|row| &row.source == source)
                        .map(|row| row.id.clone())
                        .unwrap_or_default(),
                );
            }
        }
        rows.sort();
        rows.dedup();
        session.stalled_on = rows;
    }
    provisional.summary = build_summary(
        fold,
        as_of,
        &provisional.features,
        &provisional.sessions,
        &needs,
        &provisional.decisions,
        &provisional.deliveries,
        &summary_inputs,
    );
    provisional.needs_you = needs;

    diagnostics.sort_by(|left, right| left.id.cmp(&right.id));
    diagnostics.dedup_by(|left, right| left.id == right.id);
    (provisional, diagnostics, calibration_report)
}

/// Sessions that applied each decision's effective receipt: by an ack of that
/// answer, or by any eligible own attempt or progress event that carries it,
/// not only an attempt on the ask's target: a descendant start with the
/// receipt in basis counts.
fn applied_sessions(fold: &ProjectFold, decisions: &mut BTreeMap<EventId, DecisionState>) {
    let mut by_ack: BTreeMap<(&EventId, &EventId), BTreeSet<&SessionId>> = BTreeMap::new();
    for ack in fold.acks.values() {
        by_ack
            .entry((&ack.ask, &ack.answer))
            .or_default()
            .insert(&ack.session);
    }
    let mut by_receipt: BTreeMap<&EventId, BTreeSet<&SessionId>> = BTreeMap::new();
    for feature in fold.features.values() {
        for attempt in feature.attempts.values() {
            for receipt in &attempt.applied {
                by_receipt
                    .entry(receipt)
                    .or_default()
                    .insert(&attempt.session);
            }
        }
    }
    for (ask, state) in decisions.iter_mut() {
        let Some(effective) = &state.effective_receipt else {
            state.applied_sessions = Vec::new();
            continue;
        };
        let mut sessions: BTreeSet<&SessionId> = BTreeSet::new();
        if let Some(acked) = by_ack.get(&(ask, effective)) {
            sessions.extend(acked);
        }
        if let Some(carried) = by_receipt.get(effective) {
            sessions.extend(carried);
        }
        state.applied_sessions = sessions.into_iter().cloned().collect();
    }
}

/// "Who should continue?" rows: concurrent starts, and contested takeovers
/// (a start that replaced another session's attempt it had not heard the last
/// of). One row per step: a contested takeover on a step that already has a
/// start conflict adds nothing.
fn start_conflicts(
    fold: &ProjectFold,
    causal: &CausalView<'_>,
    decisions: &BTreeMap<EventId, DecisionState>,
) -> Vec<(NodePath, EventId, UtcTime)> {
    let mut conflicts = Vec::new();
    let takeover_answers = plan::takeover_answers(&fold.asks, &fold.receipts, decisions);
    for (node, feature) in &fold.features {
        let before = conflicts.len();
        // The owner's drop settles who continues: no row on a step it
        // closed, nor for an attempt it ended.
        if feature.owner_drop().is_some() {
            continue;
        }
        let owners = |id: &EventId| {
            feature
                .attempts
                .get(id)
                .is_some_and(|attempt| feature.dropped_by_owner(attempt))
        };
        for attempt_id in &feature.start_conflicts {
            if owners(attempt_id) {
                continue;
            }
            if let Some(attempt) = feature.attempts.get(attempt_id) {
                conflicts.push((node.clone(), attempt_id.clone(), attempt.started_at));
            }
        }
        if conflicts.len() > before {
            continue;
        }
        let answers = takeover_answers.get(node).map_or(&[][..], Vec::as_slice);
        let mut contested = plan::contested_takeovers(feature, causal, answers);
        contested.retain(|(replaced, start)| !owners(replaced) && !owners(start));
        contested.sort_by_key(|(_, start)| causal.order_of(start));
        if let Some((replaced, start)) = contested.first()
            && let Some(start) = feature.attempts.get(start)
        {
            conflicts.push((node.clone(), replaced.clone(), start.started_at));
        }
    }
    conflicts.sort_by(|left, right| {
        (left.0.as_str(), left.1.as_str()).cmp(&(right.0.as_str(), right.1.as_str()))
    });
    conflicts
}

/// Reconstruct the human-wait intervals of every attempt in this project.
///
/// Each ask contributes `[ask.ts, first clearing event)`; withdrawal, a human
/// answer or supersession closes it, so a later amendment never extends an ended
/// wait. A prerequisite's ask also holds dependent attempts, but only while the
/// accepted dependency edge was live and that prerequisite attempt was still
/// pending: a restart reopens the hold and a later agenda edit cannot erase it.
/// Negative (clock-skewed) intervals clamp to zero and are
/// diagnosed once per ask.
fn waits_by_attempt(
    fold: &ProjectFold,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<EventId, Vec<(UtcTime, Option<UtcTime>)>> {
    let mut asks: Vec<eta::WaitAsk> = Vec::new();
    for ask in fold.asks.values() {
        let Some((start, end)) = ask.human_wait() else {
            continue;
        };
        let end = match end {
            Some(end) if end.millis() < start.millis() => {
                diagnostics.push(Diagnostic::new(
                    format!("clock_skew:{}", ask.id.as_str()),
                    "clock_skew",
                    Some(fold.slug.clone()),
                    Some(ask.id.clone()),
                    "A human-wait interval ended before it began; the negative interval is clamped to zero."
                        .to_owned(),
                ));
                Some(start)
            }
            other => other,
        };
        asks.push(eta::WaitAsk {
            node: ask.node.clone(),
            source: ask.id.clone(),
            start,
            end,
        });
    }
    let by_node = waits::by_node(&fold.features, &asks);
    let mut waits: BTreeMap<EventId, Vec<(UtcTime, Option<UtcTime>)>> = BTreeMap::new();
    for (node, feature) in &fold.features {
        let intervals = &by_node[node];
        for attempt in feature.attempts.values() {
            // A pending review is a human wait on the attempt that delivered.
            let mut own = intervals.clone();
            own.extend(attempt.settle.waits.iter().copied());
            waits.insert(attempt.id.clone(), own);
        }
    }
    waits
}

/// Whether the project has current doing or done-awaiting-review work, the
/// earliest start of that work for the demo-gap clock, and the step the clock
/// runs for: the doing step started first (the first node on a tie). A step
/// that waits for a review is delivered, so it is never that step.
fn demo_activity<'a>(
    fold: &ProjectFold,
    features: &'a BTreeMap<NodePath, FeatureState>,
    deliveries: &BTreeMap<EventId, crate::model::state::DeliveryState>,
) -> (bool, Option<UtcTime>, Option<&'a NodePath>) {
    let review_pending = budget::review_pending_nodes(deliveries);
    let mut active = false;
    let mut earliest: Option<UtcTime> = None;
    let mut undelivered: Option<(UtcTime, &NodePath)> = None;
    for (node, feature_state) in features {
        if !budget::in_flight(feature_state, &review_pending) {
            continue;
        }
        active = true;
        if let Some(attempt) = fold.features.get(node).and_then(plan::FeatureFold::current) {
            earliest = Some(match earliest {
                Some(current) => current.min(attempt.started_at),
                None => attempt.started_at,
            });
            let doing = feature_state.progress == Progress::Doing;
            if doing && undelivered.is_none_or(|(start, _)| attempt.started_at < start) {
                undelivered = Some((attempt.started_at, node));
            }
        }
    }
    (active, earliest, undelivered.map(|(_, node)| node))
}

/// Values the summary needs that are derived outside it.
struct SummaryInputs<'a> {
    budget: &'a BudgetUsage,
    calibration: &'a [Calibration],
    demo_active: bool,
    demo_limit_s: u32,
}

#[allow(clippy::too_many_arguments)]
fn build_summary(
    fold: &ProjectFold,
    as_of: UtcTime,
    features: &BTreeMap<NodePath, FeatureState>,
    sessions: &BTreeMap<SessionId, SessionState>,
    needs: &[NeedsYouItem],
    decisions: &BTreeMap<EventId, DecisionState>,
    deliveries: &BTreeMap<EventId, crate::model::state::DeliveryState>,
    inputs: &SummaryInputs<'_>,
) -> ProjectSummary {
    let mut feature_counts: BTreeMap<Progress, u32> = BTreeMap::new();
    for feature in features.values() {
        *feature_counts.entry(feature.progress).or_insert(0) += 1;
    }
    let mut active_agents = 0_u32;
    for session in sessions.values() {
        if session.liveness == Liveness::Live {
            active_agents += 1;
        }
    }
    let mut stalled: BTreeSet<SessionId> = BTreeSet::new();
    for row in needs {
        for session in sessions.values() {
            if session.stalled_on.contains(&row.id) {
                stalled.insert(session.id.clone());
            }
        }
    }
    let mut variance = Variance::Quiet;
    if features
        .values()
        .any(|feature| feature.basis == Basis::Tainted)
        || decisions.values().any(|decision| decision.pending_pivot)
    {
        variance = Variance::Rework;
    }
    // A stopped step marks the whole project stopped only when nothing on the
    // project is live: no step that is doing under a holder that is not lost,
    // and no live agent. With live work the project keeps its other state and
    // the summary carries the count of stopped steps.
    let stopped_steps = features
        .values()
        .filter(|feature| !feature.progress.is_terminal() && feature.liveness == Liveness::Lost)
        .count() as u32;
    let live_work = active_agents > 0
        || features.values().any(|feature| {
            feature.progress == Progress::Doing && feature.liveness != Liveness::Lost
        });
    if stopped_steps > 0 && !live_work {
        variance = Variance::Stopped;
    }
    if variance == Variance::Quiet
        && (active_agents > 0
            || features
                .values()
                .any(|feature| feature.progress == Progress::Doing))
    {
        variance = Variance::Active;
    }
    // Rework and stopped keep their precedence; otherwise a project that keeps
    // work in flight without a demo for the whole budget window rises above
    // active.
    if matches!(variance, Variance::Quiet | Variance::Active)
        && inputs.demo_active
        && inputs.budget.demo_gap_s >= u64::from(inputs.demo_limit_s)
    {
        variance = Variance::NoDemo;
    }
    let mut host_as_of = fold.host_as_of.clone();
    for value in host_as_of.values_mut() {
        if *value > as_of {
            *value = as_of;
        }
    }
    ProjectSummary {
        slug: fold.slug.clone(),
        title: fold
            .declaration
            .title
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_else(|| fold.slug.as_str().to_owned()),
        brief: fold
            .declaration
            .brief
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_default(),
        goal: fold
            .declaration
            .goal
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_default(),
        glyph: fold
            .declaration
            .glyph
            .as_ref()
            .map(|patch| patch.value.clone())
            .unwrap_or_else(|| "radar".to_owned()),
        archived: fold
            .declaration
            .archived
            .as_ref()
            .is_some_and(|patch| patch.value),
        icon: fold
            .declaration
            .icon
            .as_ref()
            .map(|patch| patch.value.clone()),
        variance,
        active_agents,
        stalled_agents: stalled.len() as u32,
        stopped_steps,
        needs_count: needs
            .iter()
            .filter(|need| attention::is_active_need(need))
            .count() as u32,
        feature_counts,
        last_demo: deliveries
            .values()
            .filter(|delivery| delivery.superseded_by.is_none())
            .filter_map(|delivery| fold.evidence.deliveries.get(&delivery.id))
            .map(|event| event.ts)
            .max(),
        last_event: fold.last_event_ts,
        host_as_of,
        budget: inputs.budget.clone(),
        calibration: inputs.calibration.to_vec(),
    }
}
