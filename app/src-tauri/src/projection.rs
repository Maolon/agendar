//! Projection from canonical core state to the frontend wire contract.
//!
//! The core state types already serialize most of the DTO. The app adds the
//! mutation revision tokens, the diagnostics code fallback
//! and the minimal graph/brief projections the app renders.

use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{
    DecisionState, FeatureState, NeedsYouItem, ProjectSlug, ProjectState, Sha256, State, UtcTime,
};
use agendar_core::reduce::activity::ActivityItem;
use agendar_core::reduce::stats::ProjectStats;

/// The revisions answers, rescues and reviews are bound to are shared with
/// every host.
pub use agendar_core::guard::{decision_revision, delivery_revision, need_revision, revision};

fn graph_revision(project: &ProjectSlug, feature: &FeatureState) -> Sha256 {
    let source = feature.field_sources.get("depends_on");
    agendar_core::model::graph_revision(project, &feature.node, source)
}

fn feature_value(project: &ProjectSlug, feature: &FeatureState, value: &mut Value) {
    if let Value::Object(object) = value {
        // Per-field SHA256 tokens, not raw event IDs: `expected_fields` and
        // `expected_graph_revision` are the mutation guards.
        let revisions = agendar_core::model::feature_field_revisions(project, feature);
        object.insert(
            "field_revisions".to_owned(),
            serde_json::to_value(&revisions).unwrap_or_else(|_| json!({})),
        );
        object.insert(
            "graph_revision".to_owned(),
            json!(graph_revision(project, feature)),
        );
    }
}

fn decision_value(
    project: &ProjectSlug,
    decision: &DecisionState,
    policy: &Sha256,
    value: &mut Value,
) {
    if let Value::Object(object) = value {
        object.insert(
            "revision".to_owned(),
            json!(decision_revision(project, decision, policy)),
        );
    }
}

/// The row's revision, and for a row with a default time `snooze_limit`: the
/// latest time a snooze may hide it (the core's rule).
fn need_value(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
    project: &ProjectState,
    value: &mut Value,
) {
    if let Value::Object(object) = value {
        object.insert(
            "revision".to_owned(),
            json!(need_revision(need, decision, &project.policy.revision)),
        );
        if let Some(limit) = agendar_core::reduce::snooze::limit_in(project, need) {
            object.insert("snooze_limit".to_owned(), json!(limit));
        }
    }
}

/// One project state plus every revision the UI must echo back.
pub fn project_value(project: &ProjectState) -> Result<Value> {
    let mut value = serde_json::to_value(project).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the project state: {error}."),
        )
        .with_detail("project_serialize")
    })?;
    let slug = project.summary.slug.clone();
    let policy = project.policy.revision.clone();

    let mut features = serde_json::Map::new();
    for (node, feature) in &project.features {
        let mut item = serde_json::to_value(feature).unwrap_or(Value::Null);
        feature_value(&slug, feature, &mut item);
        features.insert(node.as_str().to_owned(), item);
    }
    let mut decisions = serde_json::Map::new();
    for (id, decision) in &project.decisions {
        let mut item = serde_json::to_value(decision).unwrap_or(Value::Null);
        decision_value(&slug, decision, &policy, &mut item);
        decisions.insert(id.as_str().to_owned(), item);
    }
    let mut deliveries = serde_json::Map::new();
    for (id, delivery) in &project.deliveries {
        let current_receipt = agendar_core::guard::current_receipt(project, delivery);
        let mut item = serde_json::to_value(delivery).unwrap_or(Value::Null);
        if let Value::Object(object) = &mut item {
            object.insert(
                "revision".to_owned(),
                json!(delivery_revision(&slug, delivery, current_receipt)),
            );
        }
        deliveries.insert(id.as_str().to_owned(), item);
    }
    let mut needs = Vec::new();
    for need in &project.needs_you {
        let decision = project.decisions.get(&need.source);
        let mut item = serde_json::to_value(need).unwrap_or(Value::Null);
        need_value(need, decision, project, &mut item);
        needs.push(item);
    }
    if let Value::Object(object) = &mut value {
        let project_fields = agendar_core::model::project_field_revisions(&slug, &project.summary);
        object.insert(
            "field_revisions".to_owned(),
            serde_json::to_value(&project_fields).unwrap_or_else(|_| json!({})),
        );
        object.insert("features".to_owned(), Value::Object(features));
        object.insert("decisions".to_owned(), Value::Object(decisions));
        object.insert("deliveries".to_owned(), Value::Object(deliveries));
        object.insert("needs_you".to_owned(), Value::Array(needs));
    }
    Ok(value)
}

/// Attach this host's local check proposals to each projected delivery.
/// Proposals are local metadata (a cwd on this host); they are never Vault
/// truth and are not visible on another host.
pub fn attach_check_proposals(
    state_value: &mut Value,
    proposals: &std::collections::BTreeMap<String, Vec<Value>>,
) {
    let Some(projects) = state_value
        .get_mut("projects")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for project in projects.values_mut() {
        let Some(deliveries) = project.get_mut("deliveries").and_then(Value::as_object_mut) else {
            continue;
        };
        for (id, delivery) in deliveries.iter_mut() {
            if let Some(rows) = proposals.get(id)
                && let Value::Object(object) = delivery
            {
                object.insert("check_proposals".to_owned(), json!(rows));
            }
        }
    }
}

/// Attach proposals to one project JSON (the `get_project.project` shape).
pub fn attach_project_check_proposals(
    project_value: &mut Value,
    proposals: &std::collections::BTreeMap<String, Vec<Value>>,
) {
    let Some(deliveries) = project_value
        .get_mut("deliveries")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for (id, delivery) in deliveries.iter_mut() {
        if let Some(rows) = proposals.get(id)
            && let Value::Object(object) = delivery
        {
            object.insert("check_proposals".to_owned(), json!(rows));
        }
    }
}

/// Attach the local activity feed to every projected project. The feed is a
/// non-canonical UI extra (never Vault truth, never in `state export`); every
/// project object carries `activity`, empty when nothing is known.
/// Host-local, non-canonical extras of one project.
#[derive(Debug, Clone)]
pub struct ProjectExtras {
    pub activity: Vec<ActivityItem>,
    pub stats: ProjectStats,
    /// Unacked questions: is an agent waiting for the answer right now?
    pub listening: std::collections::BTreeMap<
        agendar_core::model::EventId,
        agendar_core::reduce::attention::Listening,
    >,
}

pub fn attach_activity(
    state_value: &mut Value,
    activity: &std::collections::BTreeMap<String, ProjectExtras>,
) {
    let Some(projects) = state_value
        .get_mut("projects")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for (slug, project) in projects.iter_mut() {
        if let Some(extras) = activity.get(slug) {
            attach_project_activity(project, extras);
        } else if let Value::Object(object) = project {
            object.insert("activity".to_owned(), json!([]));
        }
    }
}

/// Attach the activity feed to one project JSON (the `get_project.project` shape).
pub fn attach_project_activity(project_value: &mut Value, extras: &ProjectExtras) {
    if let Value::Object(object) = project_value {
        object.insert(
            "activity".to_owned(),
            serde_json::to_value(&extras.activity).unwrap_or_else(|_| json!([])),
        );
        if let Ok(value) = serde_json::to_value(&extras.stats) {
            object.insert("stats".to_owned(), value);
        }
        object.insert(
            "listening".to_owned(),
            serde_json::to_value(&extras.listening).unwrap_or_else(|_| json!({})),
        );
    }
}

/// The whole canonical state in frontend shape.
pub fn state_value(state: &State) -> Result<Value> {
    let mut value = serde_json::to_value(state).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the canonical state: {error}."),
        )
        .with_detail("state_serialize")
    })?;
    if let Value::Object(object) = &mut value {
        if let Some(Value::Array(diagnostics)) = object.get_mut("diagnostics") {
            for diagnostic in diagnostics {
                if let Value::Object(item) = diagnostic
                    && !item.contains_key("code")
                {
                    item.insert("code".to_owned(), json!("E_INDEX"));
                }
            }
        }
        if let Some(Value::Object(projects)) = object.get_mut("projects") {
            for (slug, project_json) in projects.iter_mut() {
                let project = state
                    .projects
                    .iter()
                    .find(|(key, _)| key.as_str() == slug)
                    .map(|(_, project)| project);
                if let Some(project) = project {
                    *project_json = project_value(project)?;
                }
            }
        }
    }
    Ok(value)
}

/// All attention rows, ranked across projects by the shared cost-of-delay order,
/// each with its revision token.
pub fn all_needs(state: &State) -> Vec<Value> {
    let mut rows: Vec<(&NeedsYouItem, Value)> = Vec::new();
    for project in state.projects.values() {
        for need in &project.needs_you {
            let decision = project.decisions.get(&need.source);
            let mut value = match serde_json::to_value(need) {
                Ok(value) => value,
                Err(_) => continue,
            };
            need_value(need, decision, project, &mut value);
            rows.push((need, value));
        }
    }
    rows.sort_by_key(|(need, _)| agendar_core::reduce::ordering::need_key(need));
    rows.into_iter().map(|(_, value)| value).collect()
}

/// The six-section brief, projected from converged state and the "last looked"
/// watermark. Rows carry the same ranking as the sidebar.
pub fn brief(
    state: &State,
    events: &[agendar_core::model::Event],
    since: Option<UtcTime>,
    _as_of: UtcTime,
) -> Value {
    match serde_json::to_value(agendar_core::reduce::brief::build(state, events, since)) {
        Ok(value) => value,
        Err(_) => json!({
            "delivered": [],
            "coming_up": [],
            "needs_you": [],
            "if_you_wait": [],
            "by_default": [],
            "at_risk": [],
        }),
    }
}

/// The semantic graph DTO, produced by the reducer so the app
/// and the CLI can never disagree about nodes, accepted edges or need ids.
pub fn graph(project: &ProjectState) -> Value {
    serde_json::to_value(agendar_core::reduce::graph::build_graph(project))
        .unwrap_or_else(|_| json!({"nodes": [], "edges": []}))
}

/// The frontend project summary map for search, in the shared project order.
pub fn summaries(state: &State) -> Vec<Value> {
    let needs: Vec<NeedsYouItem> = state
        .projects
        .values()
        .flat_map(|project| project.needs_you.iter().cloned())
        .collect();
    let earliest = agendar_core::reduce::ordering::earliest_needs(&needs);
    let mut values: Vec<(agendar_core::model::ProjectSummary, Value)> = state
        .projects
        .values()
        .filter_map(|project| {
            let value = serde_json::to_value(&project.summary).ok()?;
            Some((project.summary.clone(), value))
        })
        .collect();
    values.sort_by_key(|(summary, _)| {
        agendar_core::reduce::ordering::project_key(summary, earliest.get(&summary.slug).copied())
    });
    values.into_iter().map(|(_, value)| value).collect()
}

/// Lowercased scalar-safe haystack for search.
pub fn matches(text: &str, haystacks: &[&str]) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let needle = text.to_lowercase();
    haystacks
        .iter()
        .any(|value| value.to_lowercase().contains(&needle))
}
