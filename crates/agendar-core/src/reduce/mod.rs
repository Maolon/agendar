//! Deterministic reducer orchestration: event-set resolution, the causal fold,
//! and the final derived projections.
//!
//! `reduce` is pure: no clock reads, RNG, filesystem, database, host-specific pid
//! facts or writes. The same event multiset and the same `as_of` produce
//! byte-identical canonical state.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::event::{BudgetUsage, Event, EventPayload, PolicySet, RuleSet, WorkStatus};
use crate::model::ids::{EventId, HostId, NodePath, ProjectSlug, SessionId, UtcTime};
use crate::model::state::{
    Basis, Calibration, DecisionState, Diagnostic, FeatureState, Liveness, NeedsYouItem, Progress,
    ProjectState, ProjectSummary, Receipt, RuleState, SessionState, State, Variance,
    policy_revision, rule_revision,
};

pub mod activity;
pub mod assumptions;
pub mod attention;
pub mod brief;
pub mod budget;
pub mod calibration;
pub mod decisions;
pub mod eta;
pub mod evidence;
pub mod forecast;
pub mod graph;
pub mod nudges;
pub mod order;
pub mod ordering;
pub mod plan;
pub mod policy;
pub mod stats;

pub use decisions::{AckFold, AskFold};
pub use order::{Resolution, VariantState, VariantStatus, resolve, resolve_with_quarantine};

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
    pub ancestors: &'a BTreeMap<EventId, BTreeSet<EventId>>,
}

impl CausalView<'_> {
    pub fn order_of(&self, id: &EventId) -> usize {
        self.order_index.get(id).copied().unwrap_or(usize::MAX)
    }

    pub fn before(&self, earlier: &EventId, later: &EventId) -> bool {
        self.ancestors
            .get(later)
            .is_some_and(|set| set.contains(earlier))
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
    ReduceOutput { state, resolution }
}

/// Reduce and also return the calibration report of every project.
pub fn reduce_with_report(events: &[Event], ctx: &ReduceContext) -> (State, ReduceReport) {
    let (state, _, report) = reduce_impl(events, ctx, None);
    (state, report)
}

fn reduce_impl(
    events: &[Event],
    ctx: &ReduceContext,
    initial: Option<&Resolution>,
) -> (State, Resolution, ReduceReport) {
    let mut quarantine: BTreeMap<EventId, String> = BTreeMap::new();
    let mut resolution = match initial {
        Some(resolution) => resolution.clone(),
        None => resolve(events),
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
        resolution = resolve_with_quarantine(events, &quarantine);
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
    reduce_with_resolution(all_events, ctx, Some(resolution)).state
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
    for event in all_events {
        folds
            .entry(event.project.clone())
            .or_insert_with(|| ProjectFold::new(event.project.clone()));
    }

    for (order, event) in resolution.ordered.iter().enumerate() {
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
            EventPayload::Delivery(_)
            | EventPayload::EvidenceObserved(_)
            | EventPayload::ReviewResult(_) => fold.evidence.apply(event),
            EventPayload::Delivered(_) => {}
        }
    }

    let mut projects: BTreeMap<ProjectSlug, ProjectState> = BTreeMap::new();
    let mut diagnostics = resolution.diagnostics.clone();
    let mut report = ReduceReport::default();
    for (slug, fold) in &mut folds {
        let (project, project_diagnostics, calibration) =
            finalize_project(fold, ctx.as_of, &event_ts, &resolution.order_index, &causal);
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
    for (ask, state) in &mut decision_states {
        state.applied_sessions = applied_sessions(ask, state, fold);
    }

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
    for (ask, state) in &mut decision_states {
        state.built_on = assumptions::built_on(ask, &consumption_by_node, &fold.receipts);
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

    // 4. Feature progress, then gates (upstream progress is known after step 4a).
    let mut progress_map: BTreeMap<NodePath, Progress> = BTreeMap::new();
    for (node, feature) in &fold.features {
        progress_map.insert(
            node.clone(),
            plan::progress(feature, policy_value.require_human_review),
        );
    }
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
    let accepted_adjacency = {
        let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
        for edge in &plan_edges.accepted {
            adjacency
                .entry(edge.upstream.clone())
                .or_default()
                .insert(edge.downstream.clone());
        }
        adjacency
    };
    // Human waits are replayed from causal transitions: accepted dependency
    // edges, upstream restarts and completions, and ask answers decide when a
    // hold opens and closes. A later agenda edit therefore cannot rewrite a
    // wait that was already spent, and calibration measures
    // active duration against the ETA the agent declared at start.
    let dependency_steps = eta::dependency_steps(&fold.features, &known);
    let waits_by_attempt = waits_by_attempt(fold, &dependency_steps, &mut diagnostics);
    let calibration_report = calibration::report(&fold.features, &waits_by_attempt, as_of);
    let mut feature_states: BTreeMap<NodePath, FeatureState> = BTreeMap::new();
    for (node, feature) in &fold.features {
        let fields = field_views
            .get(node)
            .cloned()
            .expect("every folded feature has resolved fields");
        let progress = progress_map.get(node).copied().unwrap_or(Progress::Planned);
        let pending_decisions = plan::open_decisions(&decision_states, node);
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
        let accepted_depends_on: Vec<NodePath> = plan_edges
            .accepted
            .iter()
            .filter(|edge| &edge.downstream == node)
            .map(|edge| edge.upstream.clone())
            .collect();
        let unblocks = graph::descendants(&accepted_adjacency, node);
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
        let consumption = consumption_by_node.get(node).cloned().unwrap_or_default();
        let assumptions = consumption.unconfirmed.clone();
        let taints = consumption.tainted.clone();
        let state = plan::build_feature(
            feature,
            fields,
            accepted_depends_on,
            progress,
            gate,
            waits_on,
            unblocks,
            liveness,
            eta,
            assumptions,
            taints,
            policy_value.require_human_review,
        );
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
    let (demo_active, earliest_active_start) =
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

    let start_conflicts = start_conflicts(fold);
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
    let needs = {
        let inputs = attention::RescueInputs {
            rejected: &plan_edges.rejected,
            start_conflicts: &start_conflicts,
            lost_sessions: &lost_sessions,
            event_ts,
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

fn applied_sessions(ask: &EventId, state: &DecisionState, fold: &ProjectFold) -> Vec<SessionId> {
    let Some(effective) = &state.effective_receipt else {
        return Vec::new();
    };
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    for ack in fold.acks.values() {
        if &ack.ask == ask && &ack.answer == effective {
            sessions.insert(ack.session.clone());
        }
    }
    // Application may be proven by any eligible own attempt or progress event
    // that carries the current receipt, not only an attempt on the ask's target
    //: a descendant start with the receipt in basis counts.
    for feature in fold.features.values() {
        for attempt in feature.attempts.values() {
            if attempt.applied.contains(effective) {
                sessions.insert(attempt.session.clone());
            }
        }
    }
    sessions.into_iter().collect()
}

fn start_conflicts(fold: &ProjectFold) -> Vec<(NodePath, EventId, UtcTime)> {
    let mut conflicts = Vec::new();
    for (node, feature) in &fold.features {
        for attempt_id in &feature.start_conflicts {
            if let Some(attempt) = feature.attempts.get(attempt_id) {
                conflicts.push((node.clone(), attempt_id.clone(), attempt.started_at));
            }
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
    dependency_steps: &BTreeMap<NodePath, Vec<eta::DependencyStep>>,
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
    let mut waits: BTreeMap<EventId, Vec<(UtcTime, Option<UtcTime>)>> = BTreeMap::new();
    let no_steps: Vec<eta::DependencyStep> = Vec::new();
    for (node, feature) in &fold.features {
        let steps = dependency_steps.get(node).unwrap_or(&no_steps);
        let intervals = eta::waits(node, steps, &asks, &fold.features);
        for attempt in feature.attempts.values() {
            waits.insert(attempt.id.clone(), intervals.clone());
        }
    }
    waits
}

/// Whether the project has current doing or done-awaiting-review work, and the
/// earliest start of that work for the demo-gap clock.
fn demo_activity(
    fold: &ProjectFold,
    features: &BTreeMap<NodePath, FeatureState>,
    deliveries: &BTreeMap<EventId, crate::model::state::DeliveryState>,
) -> (bool, Option<UtcTime>) {
    let mut active = false;
    let mut earliest: Option<UtcTime> = None;
    for (node, feature_state) in features {
        if !budget::in_flight(feature_state, deliveries) {
            continue;
        }
        active = true;
        if let Some(attempt) = fold.features.get(node).and_then(plan::FeatureFold::current) {
            earliest = Some(match earliest {
                Some(current) => current.min(attempt.started_at),
                None => attempt.started_at,
            });
        }
    }
    (active, earliest)
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
    if features
        .values()
        .any(|feature| !feature.progress.is_terminal() && feature.liveness == Liveness::Lost)
    {
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
        needs_count: needs.len() as u32,
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
