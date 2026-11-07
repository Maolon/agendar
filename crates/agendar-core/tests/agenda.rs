//! Agenda edits, policy writes and speculation admission through the service.
//!
//! Every test uses a temporary Vault and data root. Raw session facts are
//! appended through the test Vault helper only where this release has no
//! supported command yet.

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use agendar_core::error::ErrorCode;
use agendar_core::model::{
    DecisionCard, Event, Harness, Mode, NodePath, PlanFeature, ProjectDeclared, ProjectSlug,
};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::{Clock as _, SystemClock, Vault};

const PROJECT: &str = "demo";

struct Scratch {
    _root: tempfile::TempDir,
    vault: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let vault = root.path().join(format!("{name}-vault"));
        let data = root.path().join(format!("{name}-data"));
        let repo = root.path().join(format!("{name}-repo"));
        fs::create_dir_all(&repo).expect("repo");
        Self {
            _root: root,
            vault,
            data,
            repo,
        }
    }
}

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("project slug")
}

fn node(name: &str) -> NodePath {
    NodePath::parse(name).expect("node")
}

fn registration(scratch: &Scratch, id: &str) -> SessionRegistration {
    let pid = std::process::id();
    SessionRegistration {
        project: project(),
        harness: Harness::Generic,
        harness_session_id: id.to_owned(),
        pid,
        process_start: process_identity(pid).expect("process identity"),
        repo_root: scratch.repo.clone(),
    }
}

fn card() -> DecisionCard {
    serde_json::from_value(support::card()).expect("decision card")
}

/// Append one encoded event line straight into the Vault writer stream.
fn append(vault: &Vault, event: &Event) {
    support::append_event_to_vault(vault, event, 1).expect("append raw event");
}

/// Counter for deterministic raw event IDs in this test binary.
struct Raw {
    next: u64,
    seq: u64,
}

impl Raw {
    fn new() -> Self {
        Self { next: 5000, seq: 0 }
    }

    fn event(
        &mut self,
        kind: &str,
        refs: serde_json::Value,
        body: serde_json::Value,
        writer: &str,
        ts: &str,
    ) -> Event {
        self.next += 1;
        self.seq += 1;
        let value = json!({
            "v": 1,
            "id": format!("{:026}", self.next),
            "seq": self.seq,
            "writer": writer,
            "ts": ts,
            "project": PROJECT,
            "refs": refs,
            "kind": kind,
            "body": body,
        });
        support::parse_value(&value).expect("raw event decodes")
    }

    fn operator_event(
        &mut self,
        kind: &str,
        refs: serde_json::Value,
        body: serde_json::Value,
        ts: &str,
    ) -> Event {
        let writer = support::OPERATOR.to_owned();
        self.event(kind, refs, body, &writer, ts)
    }
}

fn declared_patch() -> ProjectDeclared {
    serde_json::from_value(json!({
        "title": "Agenda",
        "brief": "Agenda edits and budgets.",
        "goal": "Keep the plan human-owned."
    }))
    .expect("project patch")
}

fn feature_patch(name: &str) -> PlanFeature {
    serde_json::from_value(json!({
        "title": name,
        "slice": "core",
        "demo_kind": "cli_transcript"
    }))
    .expect("feature patch")
}

#[test]
fn update_project_guards_each_patched_field() {
    let scratch = Scratch::new("agenda-project");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let session = register_session(&service, registration(&scratch, "agenda-project"))
        .expect("register session");
    service
        .declare_project(&project(), &session, declared_patch(), None)
        .expect("declare project");
    let now = SystemClock.now();
    let state = service.export_state(now).expect("state");
    let revisions = project_revisions(&state.projects[&project()]);

    // A stale goal token is refused before any event is written.
    let patch: ProjectDeclared =
        serde_json::from_value(json!({"goal": "New goal"})).expect("patch");
    let mut expected = std::collections::BTreeMap::new();
    expected.insert("goal".to_owned(), revisions["goal"].clone());
    let event_id = service
        .update_project(&project(), patch.clone(), &expected)
        .expect("fresh goal token is accepted");
    assert!(event_id.as_str().len() == 26);

    let stale = service
        .update_project(&project(), patch.clone(), &expected)
        .expect_err("the same token is stale after the write");
    assert_eq!(stale.code(), ErrorCode::StaleAnswer);
    assert_eq!(stale.detail(), Some("stale_field"));

    // Missing or extra fields are validation errors, never silent writes.
    let mut missing = std::collections::BTreeMap::new();
    missing.insert("title".to_owned(), revisions["title"].clone());
    let error = service
        .update_project(&project(), patch.clone(), &missing)
        .expect_err("expected_fields must match exactly the patched fields");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("expected_fields_mismatch"));

    let state = service.export_state(SystemClock.now()).expect("state");
    assert_eq!(state.projects[&project()].summary.goal, "New goal");
    // A human goal survives a later agent declaration.
    let agent_patch: ProjectDeclared =
        serde_json::from_value(json!({"goal": "Agent goal", "brief": "Agent brief"}))
            .expect("patch");
    service
        .declare_project(&project(), &session, agent_patch, None)
        .expect("agent declaration");
    let state = service.export_state(SystemClock.now()).expect("state");
    let summary = &state.projects[&project()].summary;
    assert_eq!(summary.goal, "New goal", "human goal wins");
    assert_eq!(summary.brief, "Agent brief", "unowned field still updates");
    let _ = patch;
}

#[test]
fn update_feature_guards_fields_graph_and_cycles() {
    let scratch = Scratch::new("agenda-feature");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let session = register_session(&service, registration(&scratch, "agenda-feature"))
        .expect("register session");
    service
        .declare_project(&project(), &session, declared_patch(), None)
        .expect("declare project");
    service
        .declare_feature(&project(), &session, node("a"), feature_patch("a"), None)
        .expect("declare a");
    service
        .declare_feature(&project(), &session, node("b"), feature_patch("b"), None)
        .expect("declare b");

    let state = service.export_state(SystemClock.now()).expect("state");
    let feature = &state.projects[&project()].features[&node("b")];
    let revisions = feature_revisions(feature);
    let mut expected = std::collections::BTreeMap::new();
    expected.insert("priority".to_owned(), revisions["priority"].clone());
    let patch: PlanFeature = serde_json::from_value(json!({"priority": 0})).expect("patch");
    service
        .update_feature(&project(), &node("b"), patch.clone(), &expected, None)
        .expect("priority edit");
    let state = service.export_state(SystemClock.now()).expect("state");
    assert_eq!(state.projects[&project()].features[&node("b")].priority, 0);

    // A dependency edit needs the graph revision and rejects a cycle.
    let state = service.export_state(SystemClock.now()).expect("state");
    let feature = &state.projects[&project()].features[&node("b")];
    let mut expected = std::collections::BTreeMap::new();
    expected.insert(
        "depends_on".to_owned(),
        feature_revisions(feature)["depends_on"].clone(),
    );
    let depend: PlanFeature = serde_json::from_value(json!({"depends_on": ["a"]})).expect("patch");
    let error = service
        .update_feature(&project(), &node("b"), depend.clone(), &expected, None)
        .expect_err("a dependency edit requires the graph token");
    assert_eq!(error.detail(), Some("expected_graph_revision_required"));

    let stale_graph = agendar_core::model::graph_revision(&project(), &node("a"), None);
    let error = service
        .update_feature(
            &project(),
            &node("b"),
            depend.clone(),
            &expected,
            Some(&stale_graph),
        )
        .expect_err("another feature's graph token is stale");
    assert_eq!(error.detail(), Some("stale_graph"));

    let current_graph = feature_graph_revision(feature);
    service
        .update_feature(
            &project(),
            &node("b"),
            depend.clone(),
            &expected,
            Some(&current_graph),
        )
        .expect("b now depends on a");
    let state = service.export_state(SystemClock.now()).expect("state");
    assert_eq!(
        state.projects[&project()].features[&node("b")].depends_on,
        vec![node("a")]
    );

    // Closing a cycle is rejected before any append.
    let state = service.export_state(SystemClock.now()).expect("state");
    let feature_a = &state.projects[&project()].features[&node("a")];
    let mut expected = std::collections::BTreeMap::new();
    expected.insert(
        "depends_on".to_owned(),
        feature_revisions(feature_a)["depends_on"].clone(),
    );
    let cycle: PlanFeature = serde_json::from_value(json!({"depends_on": ["b"]})).expect("patch");
    let error = service
        .update_feature(
            &project(),
            &node("a"),
            cycle,
            &expected,
            Some(&feature_graph_revision(feature_a)),
        )
        .expect_err("a cycle is rejected");
    assert_eq!(error.detail(), Some("dependency_cycle"));
}

#[test]
fn set_policy_requires_a_current_revision_and_keeps_the_hard_floor() {
    let scratch = Scratch::new("policy");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let session =
        register_session(&service, registration(&scratch, "policy")).expect("register session");
    service
        .declare_project(&project(), &session, declared_patch(), None)
        .expect("declare project");
    let state = service.export_state(SystemClock.now()).expect("state");
    let current = state.projects[&project()].policy.revision.clone();
    let mut policy = state.projects[&project()].policy.value.clone();
    policy.preset = agendar_core::model::Preset::Strict;
    policy.cells = agendar_core::reduce::policy::STRICT_CELLS;
    policy.allow_timeouts = false;
    service
        .set_policy(&project(), policy.clone(), &current)
        .expect("strict policy");

    // The same token is stale now.
    let error = service
        .set_policy(&project(), policy.clone(), &current)
        .expect_err("stale revision");
    assert_eq!(error.code(), ErrorCode::StaleAnswer);

    let state = service.export_state(SystemClock.now()).expect("state");
    assert_eq!(
        state.projects[&project()].policy.value.preset,
        agendar_core::model::Preset::Strict
    );
    assert!(!state.projects[&project()].policy.value.allow_timeouts);

    // Irreversible cells can never be loosened, even by an operator write.
    let current = state.projects[&project()].policy.revision.clone();
    let mut loosened = state.projects[&project()].policy.value.clone();
    loosened.preset = agendar_core::model::Preset::Custom;
    loosened.cells.irreversible_local = Mode::Auto;
    let error = service
        .set_policy(&project(), loosened, &current)
        .expect_err("irreversible stays locked to ask");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("policy_locked_risk_floor"));
}

#[test]
fn ask_refuses_speculation_over_the_budget() {
    let scratch = Scratch::new("budget");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let vault = Vault::open(&scratch.vault).expect("open vault");
    let mut raw = Raw::new();
    let ts = "2026-01-01T00:00:00.000Z";
    // A declared project and one feature already built on an assumed choice.
    append(
        &vault,
        &raw.operator_event(
            "project.declared",
            json!({}),
            json!({"title": "Budget", "brief": "b", "goal": "g"}),
            ts,
        ),
    );
    append(
        &vault,
        &raw.operator_event(
            "plan.feature",
            json!({"node": "a"}),
            json!({"title": "a", "slice": "core", "demo_kind": "cli_transcript"}),
            ts,
        ),
    );
    append(
        &vault,
        &raw.operator_event(
            "plan.feature",
            json!({"node": "b"}),
            json!({"title": "b", "slice": "core", "demo_kind": "cli_transcript"}),
            ts,
        ),
    );
    let agent = "h-00000000000000000000000001/s-00000000000000000000000111";
    let session = "00000000000000000000000111";
    let attempt = raw.event("work.progress", json!({"node": "a"}), json!({"agent": {"session": session, "harness": "generic"}, "status": "started", "eta": {"min_s": 600, "max_s": 600}, "evidence": []}), agent, "2026-01-01T00:01:00.000Z");
    append(&vault, &attempt);
    let ask = raw.event("decision.ask", json!({"node": "a"}), json!({"agent": {"session": session, "harness": "generic"}, "card": support::card(), "policy": assume_eval()}), agent, "2026-01-01T00:02:00.000Z");
    append(&vault, &ask);
    append(&vault, &raw.event("decision.assumed", json!({"node": "a", "ask": ask.id, "attempt": attempt.id}), json!({"agent": {"session": session, "harness": "generic"}, "option": "a", "reason": "policy", "policy": assume_eval()}), agent, "2026-01-01T00:03:00.000Z"));

    // Tighten the budget to one speculative feature.
    let state = service.export_state(SystemClock.now()).expect("state");
    let project_state = &state.projects[&project()];
    assert_eq!(project_state.summary.budget.speculative_features, 1);
    let mut policy = project_state.policy.value.clone();
    policy.budget.max_speculative_features = 1;
    service
        .set_policy(&project(), policy, &project_state.policy.revision)
        .expect("tight budget");

    let asking = register_session(&service, registration(&scratch, "budget-agent"))
        .expect("register session");
    let error = service
        .ask(&project(), &asking, &node("b"), card(), Mode::Assume, None)
        .expect_err("a second speculative feature is over budget");
    assert_eq!(
        error.code(),
        ErrorCode::Budget,
        "E_BUDGET, not a capability gap"
    );
    assert!(error.message().contains("limit 1"));
    assert_eq!(error.detail(), Some("budget_exceeded"));

    // No ask event was written for b.
    let state = service.export_state(SystemClock.now()).expect("state");
    assert!(
        !state.projects[&project()]
            .decisions
            .values()
            .any(|decision| decision.node == node("b"))
    );

    // A sincere human ask is never refused by the speculation budget.
    let firm = service
        .ask(&project(), &asking, &node("b"), card(), Mode::Ask, None)
        .expect("ask stays available");
    assert_eq!(firm.effective_mode, Mode::Ask);
    assert!(
        firm.policy.budget.blocked,
        "the policy snapshot reports the overrun"
    );
}

fn assume_eval() -> serde_json::Value {
    let mut policy = support::policy_eval();
    policy["requested"] = json!("assume");
    policy["effective"] = json!("assume");
    policy
}

// The projection helpers below mirror what the app adds to the wire DTO so the
// service tests assert against the same tokens the frontend round-trips.
fn project_revisions(
    project_state: &agendar_core::model::ProjectState,
) -> std::collections::BTreeMap<String, agendar_core::model::Sha256> {
    agendar_core::model::project_field_revisions(&project(), &project_state.summary)
}

fn feature_revisions(
    feature: &agendar_core::model::FeatureState,
) -> std::collections::BTreeMap<String, agendar_core::model::Sha256> {
    agendar_core::model::feature_field_revisions(&project(), feature)
}

fn feature_graph_revision(
    feature: &agendar_core::model::FeatureState,
) -> agendar_core::model::Sha256 {
    agendar_core::model::graph_revision(
        &project(),
        &feature.node,
        feature.field_sources.get("depends_on"),
    )
}
