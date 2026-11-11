mod support;

use agendar_core::AgendarError;
use agendar_core::model::{
    Domain, Event, EventId, EventPayload, HostId, ObservationMethod, PolicySet, PolicyState,
    ProjectSlug, RuleId, RuleSet, RuleState, SessionId, Sha256, VaultId, WriterId, canonical_json,
    parse_event, policy_revision, rule_revision,
};
use serde_json::{Value, json};
use support::*;

const ANSWER_CONFLICT: &str = include_str!("oracle/seeds/answer-conflict/events.jsonl");
const CONCURRENT_CONSUMPTION: &str =
    include_str!("oracle/seeds/concurrent-consumption/events.jsonl");
const CONCURRENT_OBSERVATIONS: &str =
    include_str!("oracle/seeds/concurrent-observations/events.jsonl");
const DELIVERY_SEEN_REVIEW: &str = include_str!("oracle/seeds/delivery-seen-review/events.jsonl");
const INVALID_DELIVERY: &str =
    include_str!("oracle/seeds/delivery-seen-review/invalid-delivery.json");
const ETA_FIVE_FEATURES: &str = include_str!("oracle/seeds/eta-five-features-mixed/events.jsonl");
const ETA_ONE_FEATURE: &str =
    include_str!("oracle/seeds/eta-one-feature-five-attempts/events.jsonl");
const ETA_OVERLAPPING: &str = include_str!("oracle/seeds/eta-overlapping-pauses/events.jsonl");
const PLAN_CYCLE: &str = include_str!("oracle/seeds/plan-cycle/events.jsonl");
const RANKING_TWO_ASKS: &str = include_str!("oracle/seeds/ranking-two-asks/events.jsonl");
const TRANSITIVE_TAINT: &str = include_str!("oracle/seeds/transitive-taint/events.jsonl");

fn parse_seed_text(text: &str) -> Vec<Event> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line).expect("seed line is JSON");
            let project = value["project"].as_str().expect("seed project");
            let writer = value["writer"].as_str().expect("seed writer");
            let source = source_for(project, writer);
            let mut with_lf = line.to_owned();
            with_lf.push('\n');
            parse_event(with_lf.as_bytes(), &source).expect("seed line must decode")
        })
        .collect()
}

fn assert_round_trip(event: &Event) {
    let value = serde_json::to_value(event).expect("event serializes");
    let source = source_for(event.project.as_ref(), &event.writer.to_string());
    let parsed = parse_event(line(&value).as_bytes(), &source).expect("round-trip decodes");
    assert_eq!(&parsed, event);
}

fn seed_event_with_kind(text: &str, kind: &str) -> Value {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("seed line is JSON"))
        .find(|value| value["kind"] == kind)
        .unwrap_or_else(|| panic!("seed contains a {kind} event"))
}

#[test]
fn seed_answer_conflict_decodes_exactly() {
    let events = parse_seed_text(ANSWER_CONFLICT);
    assert_eq!(events.len(), 5);
    assert_eq!(events[0].kind(), "project.declared");
    assert_eq!(events[2].kind(), "decision.ask");
    assert_eq!(events[4].id.as_str(), "00000000000000000000000005");
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_concurrent_consumption_decodes_exactly() {
    let events = parse_seed_text(CONCURRENT_CONSUMPTION);
    assert_eq!(events.len(), 8);
    assert_eq!(events[6].kind(), "decision.assumed");
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_concurrent_observations_decodes_exactly() {
    let events = parse_seed_text(CONCURRENT_OBSERVATIONS);
    assert_eq!(events.len(), 9);
    assert_eq!(events[7].kind(), "evidence.observed");
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_delivery_seen_review_decodes_exactly() {
    let events = parse_seed_text(DELIVERY_SEEN_REVIEW);
    assert_eq!(events.len(), 14);
    assert_eq!(events[6].kind(), "delivery");
    assert_eq!(events[10].kind(), "review.result");
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_eta_five_features_decodes_exactly() {
    let events = parse_seed_text(ETA_FIVE_FEATURES);
    assert_eq!(events.len(), 16);
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_eta_one_feature_decodes_exactly() {
    let events = parse_seed_text(ETA_ONE_FEATURE);
    assert_eq!(events.len(), 12);
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_eta_overlapping_pauses_decodes_exactly() {
    let events = parse_seed_text(ETA_OVERLAPPING);
    assert_eq!(events.len(), 8);
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_plan_cycle_decodes_exactly() {
    let events = parse_seed_text(PLAN_CYCLE);
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].kind(), "plan.feature");
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_ranking_two_asks_decodes_exactly() {
    let events = parse_seed_text(RANKING_TWO_ASKS);
    assert_eq!(events.len(), 7);
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn seed_transitive_taint_decodes_exactly() {
    let events = parse_seed_text(TRANSITIVE_TAINT);
    assert_eq!(events.len(), 11);
    for event in &events {
        assert_round_trip(event);
    }
}

#[test]
fn invalid_delivery_seen_is_rejected_before_target_lookup() {
    let value: Value = serde_json::from_str(INVALID_DELIVERY).expect("negative input is JSON");
    let error = parse_value(&value).expect_err("Seen delivery must be rejected");
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.field(), Some("/criteria/0/claim"));
    assert_eq!(
        error.detail(),
        Some("delivery_seen_requires_later_observation")
    );
    assert!(!error.message().contains("causal_cycle"));
}

#[test]
fn base_decision_ask_accepts() {
    let value: Value = serde_json::from_str(
        ANSWER_CONFLICT
            .lines()
            .nth(2)
            .expect("third seed line is the ask"),
    )
    .expect("seed line is JSON");
    let event = parse_value(&value).expect("base ask accepts");
    assert_eq!(event.kind(), "decision.ask");
    assert_eq!(
        event.refs.node.as_ref().map(ToString::to_string),
        Some("x".into())
    );
}

#[test]
fn all_valid_fixture_kinds_accept() {
    for kind in [
        "project.declared",
        "project.declared.agent",
        "plan.feature",
        "plan.feature.agent",
        "work.progress.started",
        "work.progress.done",
        "work.progress.verified",
        "work.progress.dropped",
        "pulse.work",
        "pulse.session",
        "decision.ask",
        "decision.assumed",
        "decision.auto",
        "decision.withdrawn",
        "decision.answer",
        "decision.ack",
        "delivery",
        "evidence.observed",
        "review.result",
        "rule.set",
        "policy.set",
        "plan.dropped",
        "need.snoozed",
        "session.lost",
        "session.continued",
        "delivered.answer",
        "delivered.review",
    ] {
        let event = parse_value(&valid_event(kind))
            .unwrap_or_else(|error| panic!("{kind} should decode: {error}"));
        assert_round_trip(&event);
    }
}

#[test]
fn refs_are_omitted_when_empty() {
    let event = parse_value(&valid_event("project.declared")).expect("declaration accepts");
    let value = serde_json::to_value(&event).expect("event serializes");
    assert_eq!(value["refs"], json!({}));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_ok(value: &Value) -> Event {
    parse_value(value).unwrap_or_else(|error| panic!("expected valid event: {error}"))
}

fn expect_err(value: &Value) -> agendar_core::AgendarError {
    parse_value(value).expect_err("expected validation failure")
}

fn assert_error(value: &Value, field: &str, detail: &str) {
    let error = expect_err(value);
    assert_eq!(error.code().as_str(), "E_VALIDATION", "{error}");
    assert_eq!(error.field(), Some(field), "{error}");
    assert_eq!(error.detail(), Some(detail), "{error}");
}

fn assert_error_code(value: &Value, code: &str) {
    let error = expect_err(value);
    assert_eq!(error.code().as_str(), code, "{error}");
}

fn refs_mut(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    value
        .get_mut("refs")
        .expect("fixture refs")
        .as_object_mut()
        .expect("fixture refs object")
}

fn body_mut(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    value
        .get_mut("body")
        .expect("fixture body")
        .as_object_mut()
        .expect("fixture body object")
}

/// Insert a field that the base fixture does not declare.
fn insert_field(object: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
    object.insert(key.to_owned(), value);
}

fn insert_ref(value: &mut Value, name: &str, id: &str) {
    refs_mut(value).insert(name.to_owned(), json!(id));
}

fn remove_ref(value: &mut Value, name: &str) {
    refs_mut(value).remove(name);
}

fn retarget(value: &mut Value, writer: &str) {
    value["writer"] = json!(writer);
}

// ---------------------------------------------------------------------------
// Fixed wire cases
// ---------------------------------------------------------------------------

fn base_ask() -> Value {
    serde_json::from_str(
        ANSWER_CONFLICT
            .lines()
            .nth(2)
            .expect("third seed line is the ask"),
    )
    .expect("seed line is JSON")
}

#[test]
fn fixed_wire_base_ask_accepts() {
    let event = expect_ok(&base_ask());
    assert_eq!(event.kind(), "decision.ask");
}

#[test]
fn fixed_wire_source_project_mismatch_rejected() {
    let source = agendar_core::model::SourcePath::from_relative_path(
        "projects/other-project/h-00000000000000000000000001/s-00000000000000000000000101.jsonl",
    )
    .expect("source path parses");
    let error = support::parse_value_with_source(&base_ask(), &source).expect_err("mismatch");
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.detail(), Some("source_project_mismatch"));
}

#[test]
fn fixed_wire_source_writer_mismatch_rejected() {
    let source = agendar_core::model::SourcePath::from_relative_path(
        "projects/example/h-00000000000000000000000002/s-00000000000000000000000101.jsonl",
    )
    .expect("source path parses");
    let error = support::parse_value_with_source(&base_ask(), &source).expect_err("mismatch");
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.detail(), Some("source_writer_mismatch"));
}

#[test]
fn fixed_wire_operator_ask_rejected() {
    let mut value = base_ask();
    retarget(&mut value, OPERATOR);
    let error = expect_err(&value);
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.detail(), Some("writer_role_forbidden"));
}

#[test]
fn fixed_wire_inconsistent_source_segment_rejected() {
    let mut source = source_for(PROJECT, AGENT);
    source.segment = 2;
    let error = support::parse_value_with_source(&base_ask(), &source).expect_err("inconsistent");
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.detail(), Some("inconsistent_source_path"));
}

#[test]
fn fixed_wire_forbidden_receipt_ref_rejected() {
    let mut value = base_ask();
    insert_ref(&mut value, "receipt", "00000000000000000000000001");
    assert_error(&value, "/refs/receipt", "forbidden_ref");
}

#[test]
fn fixed_wire_empty_forbidden_basis_is_rejected() {
    let mut value = base_ask();
    refs_mut(&mut value).insert("basis".to_owned(), json!([]));
    assert_error(&value, "/refs/basis", "forbidden_ref");
}

#[test]
fn fixed_wire_missing_node_ref_rejected() {
    let mut value = base_ask();
    remove_ref(&mut value, "node");
    assert_error(&value, "/refs/node", "required_ref_missing");
}

#[test]
fn fixed_wire_unresolved_supersedes_is_structurally_accepted() {
    let mut value = base_ask();
    insert_ref(&mut value, "supersedes", "00000000000000000000000001");
    let event = expect_ok(&value);
    assert_eq!(
        event.refs.supersedes.as_ref().map(ToString::to_string),
        Some("00000000000000000000000001".to_owned())
    );
}

#[test]
fn fixed_wire_seen_fact_requires_causes() {
    let mut value = base_ask();
    body_mut(&mut value)["card"]["already_checked"] = json!([{
        "text": "Output matched.",
        "claim": "seen",
        "observation": "00000000000000000000000001",
    }]);
    assert_error(
        &value,
        "/card/already_checked/0/observation",
        "body_reference_missing_from_causes",
    );
}

#[test]
fn fixed_wire_seen_fact_with_cause_is_structurally_accepted() {
    let mut value = base_ask();
    body_mut(&mut value)["card"]["already_checked"] = json!([{
        "text": "Output matched.",
        "claim": "seen",
        "observation": "00000000000000000000000001",
    }]);
    refs_mut(&mut value).insert("causes".to_owned(), json!(["00000000000000000000000001"]));
    expect_ok(&value);
}

// ---------------------------------------------------------------------------
// Complete allowed-ref matrix
// ---------------------------------------------------------------------------

const SCALAR_REFS: [&str; 7] = [
    "node",
    "ask",
    "answer",
    "delivery",
    "receipt",
    "attempt",
    "supersedes",
];

struct MatrixCase {
    key: &'static str,
    required: &'static [&'static str],
    allowed: &'static [&'static str],
    basis: bool,
}

const MATRIX: [MatrixCase; 25] = [
    MatrixCase {
        key: "project.declared",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "plan.feature",
        required: &["node"],
        allowed: &["node"],
        basis: false,
    },
    MatrixCase {
        key: "work.progress.started",
        required: &["node"],
        allowed: &["node"],
        basis: true,
    },
    MatrixCase {
        key: "work.progress.done",
        required: &["node", "attempt"],
        allowed: &["node", "attempt"],
        basis: true,
    },
    MatrixCase {
        key: "work.progress.verified",
        required: &["node", "attempt"],
        allowed: &["node", "attempt"],
        basis: true,
    },
    MatrixCase {
        key: "work.progress.dropped",
        // The attempt is optional: a never-started step is dropped without one.
        required: &["node"],
        allowed: &["node", "attempt"],
        basis: true,
    },
    MatrixCase {
        key: "pulse.work",
        required: &["node", "attempt"],
        allowed: &["node", "attempt"],
        basis: false,
    },
    MatrixCase {
        key: "pulse.session",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "decision.ask",
        required: &["node"],
        allowed: &["node", "supersedes"],
        basis: false,
    },
    MatrixCase {
        key: "decision.assumed",
        required: &["node", "ask"],
        allowed: &["node", "ask", "attempt"],
        basis: false,
    },
    MatrixCase {
        key: "decision.auto",
        required: &["node", "ask"],
        allowed: &["node", "ask", "attempt"],
        basis: false,
    },
    MatrixCase {
        key: "decision.withdrawn",
        required: &["node", "ask"],
        allowed: &["node", "ask"],
        basis: false,
    },
    MatrixCase {
        key: "decision.answer",
        required: &["node", "ask"],
        allowed: &["node", "ask"],
        basis: false,
    },
    MatrixCase {
        key: "decision.ack",
        required: &["node", "ask", "answer"],
        allowed: &["node", "ask", "answer"],
        basis: false,
    },
    MatrixCase {
        key: "delivery",
        // The receipt is optional; without one the delivery lists no criteria.
        required: &["node", "attempt"],
        allowed: &["node", "attempt", "receipt", "supersedes"],
        basis: false,
    },
    MatrixCase {
        key: "evidence.observed",
        required: &["node", "delivery"],
        allowed: &["node", "delivery"],
        basis: false,
    },
    MatrixCase {
        key: "review.result",
        required: &["node", "delivery"],
        allowed: &["node", "delivery"],
        basis: false,
    },
    MatrixCase {
        key: "rule.set",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "policy.set",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "plan.dropped",
        required: &["node"],
        allowed: &["node"],
        basis: false,
    },
    MatrixCase {
        key: "need.snoozed",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "session.lost",
        required: &[],
        allowed: &["node"],
        basis: false,
    },
    MatrixCase {
        key: "session.continued",
        required: &[],
        allowed: &[],
        basis: false,
    },
    MatrixCase {
        key: "delivered.answer",
        required: &["node", "ask", "answer"],
        allowed: &["node", "ask", "answer"],
        basis: false,
    },
    MatrixCase {
        key: "delivered.review",
        required: &["node", "delivery"],
        allowed: &["node", "delivery"],
        basis: false,
    },
];

#[test]
fn every_matrix_case_accepts_with_unresolved_allowed_refs() {
    for case in &MATRIX {
        let event = expect_ok(&valid_event(case.key));
        assert_eq!(event.kind(), wire_kind(case.key));
        for allowed in case.allowed {
            let mut value = valid_event(case.key);
            if !case.required.contains(allowed) {
                insert_ref(&mut value, allowed, REF_ID);
                expect_ok(&value);
            }
        }
    }
    expect_ok(&valid_event("delivered.review"));
}

fn wire_kind(key: &str) -> &'static str {
    match key {
        "work.progress.started"
        | "work.progress.done"
        | "work.progress.verified"
        | "work.progress.dropped" => "work.progress",
        "pulse.work" | "pulse.session" => "pulse",
        "delivered.answer" | "delivered.review" => "delivered",
        "project.declared.agent" => "project.declared",
        "plan.feature.agent" => "plan.feature",
        other => match other {
            "project.declared" => "project.declared",
            "plan.feature" => "plan.feature",
            "decision.ask" => "decision.ask",
            "decision.assumed" => "decision.assumed",
            "decision.auto" => "decision.auto",
            "decision.withdrawn" => "decision.withdrawn",
            "decision.answer" => "decision.answer",
            "decision.ack" => "decision.ack",
            "delivery" => "delivery",
            "evidence.observed" => "evidence.observed",
            "review.result" => "review.result",
            "rule.set" => "rule.set",
            "policy.set" => "policy.set",
            "plan.dropped" => "plan.dropped",
            "need.snoozed" => "need.snoozed",
            "session.lost" => "session.lost",
            "session.continued" => "session.continued",
            _ => panic!("unknown wire kind key {other}"),
        },
    }
}

#[test]
fn every_required_ref_is_enforced() {
    for case in &MATRIX {
        for required in case.required {
            let mut value = valid_event(case.key);
            remove_ref(&mut value, required);
            assert_error_code(&value, "E_VALIDATION");
        }
    }
    let mut review = valid_event("delivered.review");
    remove_ref(&mut review, "node");
    assert_error_code(&review, "E_VALIDATION");
    let mut review = valid_event("delivered.review");
    remove_ref(&mut review, "delivery");
    assert_error_code(&review, "E_VALIDATION");
}

#[test]
fn every_forbidden_ref_is_rejected() {
    for case in &MATRIX {
        for name in SCALAR_REFS {
            if case.allowed.contains(&name) {
                continue;
            }
            let mut value = valid_event(case.key);
            insert_ref(&mut value, name, REF_ID);
            assert_error_code(&value, "E_VALIDATION");
        }
    }
}

#[test]
fn basis_is_allowed_only_on_work_progress() {
    for case in &MATRIX {
        let mut value = valid_event(case.key);
        refs_mut(&mut value).insert("basis".to_owned(), json!([]));
        if case.basis {
            expect_ok(&value);
            let mut with_id = valid_event(case.key);
            refs_mut(&mut with_id).insert("basis".to_owned(), json!([REF_ID]));
            expect_ok(&with_id);
        } else {
            assert_error(&value, "/refs/basis", "forbidden_ref");
        }
    }
}

#[test]
fn causes_are_allowed_on_every_kind_and_review_requires_them() {
    for case in &MATRIX {
        let mut value = valid_event(case.key);
        if value["refs"].get("causes").is_none() {
            refs_mut(&mut value).insert("causes".to_owned(), json!([REF_ID]));
        }
        expect_ok(&value);
    }
    let mut review = valid_event("delivered.review");
    remove_ref(&mut review, "causes");
    assert_error(&review, "/refs/causes", "delivered_review_requires_causes");
}

#[test]
fn delivered_review_rejects_ask_and_answer_refs() {
    let mut value = valid_event("delivered.review");
    insert_ref(&mut value, "ask", REF_ID);
    assert_error_code(&value, "E_VALIDATION");
    let mut value = valid_event("delivered.review");
    insert_ref(&mut value, "answer", REF_ID);
    assert_error_code(&value, "E_VALIDATION");
}

#[test]
fn delivered_answer_rejects_delivery_ref() {
    let mut value = valid_event("delivered.answer");
    insert_ref(&mut value, "delivery", REF_ID);
    assert_error_code(&value, "E_VALIDATION");
}

#[test]
fn pulse_node_and_attempt_must_be_paired() {
    let mut value = valid_event("pulse.session");
    insert_ref(&mut value, "node", "x");
    assert_error_code(&value, "E_VALIDATION");
    let mut value = valid_event("pulse.session");
    insert_ref(&mut value, "attempt", REF_ID);
    assert_error_code(&value, "E_VALIDATION");
}

// ---------------------------------------------------------------------------
// SourcePath grammar
// ---------------------------------------------------------------------------

#[test]
fn source_path_accepts_exact_writer_grammar() {
    let cases = [
        (
            "projects/example/h-00000000000000000000000001/operator.jsonl",
            "operator",
            1,
        ),
        (
            "projects/example/h-00000000000000000000000001/steward.jsonl",
            "steward",
            1,
        ),
        (
            "projects/example/h-00000000000000000000000001/s-00000000000000000000000101.jsonl",
            "agent",
            1,
        ),
        (
            "projects/example/h-00000000000000000000000001/operator.2.jsonl",
            "operator",
            2,
        ),
        (
            "projects/example/h-00000000000000000000000001/steward.10.jsonl",
            "steward",
            10,
        ),
        (
            "projects/a-b9/h-00000000000000000000000001/s-00000000000000000000000101.3.jsonl",
            "agent",
            3,
        ),
    ];
    for (path, role, segment) in cases {
        let source = agendar_core::model::SourcePath::from_relative_path(path)
            .unwrap_or_else(|error| panic!("{path} should parse: {error}"));
        assert_eq!(source.segment, segment);
        match role {
            "operator" => assert!(source.writer.is_operator()),
            "steward" => assert!(source.writer.is_steward()),
            "agent" => assert!(source.writer.is_agent()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn source_path_rejects_traversal_absolute_and_conflict_names() {
    let invalid = [
        "",
        "/projects/example/h-00000000000000000000000001/operator.jsonl",
        "projects\\example\\h-00000000000000000000000001\\operator.jsonl",
        "../projects/example/h-00000000000000000000000001/operator.jsonl",
        "projects/example/../h-00000000000000000000000001/operator.jsonl",
        "projects/example/h-00000000000000000000000001/../operator.jsonl",
        "projects/example/h-00000000000000000000000001/.jsonl",
        "projects/example/h-00000000000000000000000001/operator",
        "projects/example/h-00000000000000000000000001/operator.jsonlx",
        "projects/example/h-00000000000000000000000001/operator.1.jsonl",
        "projects/example/h-00000000000000000000000001/operator.0.jsonl",
        "projects/example/h-00000000000000000000000001/operator.02.jsonl",
        "projects/example/h-00000000000000000000000001/operator.2x.jsonl",
        "projects/example/h-00000000000000000000000001/operator.2.3.jsonl",
        "projects/example/h-00000000000000000000000001/operator 2.jsonl",
        "projects/example/h-00000000000000000000000001/operator (1).jsonl",
        "projects/example/h-00000000000000000000000001/operator copy.jsonl",
        "projects/example/h-00000000000000000000000001/operator.4294967296.jsonl",
        "projects/example/h-00000000000000000000000001/operator.2.JSONL",
        "projects/example/h-00000000000000000000000001//operator.jsonl",
        "projects/example/h-00000000000000000000000001/operator.jsonl/",
        "projects/Example/h-00000000000000000000000001/operator.jsonl",
        "projects/example/H-00000000000000000000000001/operator.jsonl",
        "projects/example/h-00000000000000000000000001/OPERATOR.jsonl",
        "projects/example/h-00000000000000000000000001/s-00000000000000000000000101.jsonl ",
        "projects/example/h-00000000000000000000000001/s-000000000000000000000001a1.jsonl",
    ];
    for path in invalid {
        let error = agendar_core::model::SourcePath::from_relative_path(path)
            .expect_err(&format!("{path} must be rejected"));
        assert_eq!(error.code().as_str(), "E_VALIDATION", "{path}: {error}");
    }
}

#[test]
fn source_path_reparse_uses_parsed_fields() {
    let mut source = source_for(PROJECT, OPERATOR);
    source.segment = 2;
    let value = valid_event("project.declared");
    let error = support::parse_value_with_source(&value, &source).expect_err("inconsistent");
    assert_eq!(error.detail(), Some("inconsistent_source_path"));
}

// ---------------------------------------------------------------------------
// Roles and agent binding
// ---------------------------------------------------------------------------

const AGENT_ONLY: [&str; 12] = [
    "work.progress.started",
    "work.progress.done",
    "work.progress.verified",
    "work.progress.dropped",
    "pulse.work",
    "pulse.session",
    "decision.ask",
    "decision.assumed",
    "decision.auto",
    "decision.withdrawn",
    "decision.ack",
    "delivery",
];

const HUMAN_ONLY: [&str; 6] = [
    "decision.answer",
    "review.result",
    "rule.set",
    "policy.set",
    "plan.dropped",
    "need.snoozed",
];

const STEWARD_ONLY: [&str; 5] = [
    "evidence.observed",
    "session.lost",
    "session.continued",
    "delivered.answer",
    "delivered.review",
];

#[test]
fn agent_only_kinds_reject_human_and_steward_writers() {
    for key in AGENT_ONLY {
        for writer in [OPERATOR, STEWARD] {
            let mut value = valid_event(key);
            retarget(&mut value, writer);
            let error = expect_err(&value);
            assert_eq!(error.code().as_str(), "E_VALIDATION");
            assert_eq!(
                error.detail(),
                Some("writer_role_forbidden"),
                "{key} {writer}"
            );
        }
    }
}

#[test]
fn agent_only_kinds_require_matching_agent_session() {
    for key in AGENT_ONLY {
        let mut value = valid_event(key);
        retarget(&mut value, AGENT2);
        let error = expect_err(&value);
        assert_eq!(error.code().as_str(), "E_VALIDATION");
        assert_eq!(error.detail(), Some("agent_session_mismatch"), "{key}");
    }
}

#[test]
fn human_only_kinds_reject_agent_and_steward_writers() {
    for key in HUMAN_ONLY {
        for writer in [AGENT, STEWARD] {
            let mut value = valid_event(key);
            retarget(&mut value, writer);
            let error = expect_err(&value);
            assert_eq!(error.code().as_str(), "E_VALIDATION");
            assert_eq!(
                error.detail(),
                Some("writer_role_forbidden"),
                "{key} {writer}"
            );
        }
    }
}

#[test]
fn steward_only_kinds_reject_operator_and_agent_writers() {
    for key in STEWARD_ONLY {
        for writer in [OPERATOR, AGENT] {
            let mut value = valid_event(key);
            retarget(&mut value, writer);
            let error = expect_err(&value);
            assert_eq!(error.code().as_str(), "E_VALIDATION");
            assert_eq!(
                error.detail(),
                Some("writer_role_forbidden"),
                "{key} {writer}"
            );
        }
    }
}

#[test]
fn human_or_agent_kinds_require_the_matching_agent_binding() {
    for (key, agent_key) in [
        ("project.declared", "project.declared.agent"),
        ("plan.feature", "plan.feature.agent"),
    ] {
        let mut steward = valid_event(key);
        retarget(&mut steward, STEWARD);
        assert_error(&steward, "/writer", "writer_role_forbidden");

        let mut human_with_agent = valid_event(agent_key);
        retarget(&mut human_with_agent, OPERATOR);
        assert_error(&human_with_agent, "/agent", "agent_forbidden");

        let mut agent_without_agent = valid_event(key);
        retarget(&mut agent_without_agent, AGENT);
        assert_error(&agent_without_agent, "/agent", "agent_required");

        let mut mismatched = valid_event(agent_key);
        retarget(&mut mismatched, AGENT2);
        assert_error(&mismatched, "/agent/session", "agent_session_mismatch");
    }
}

#[test]
fn human_owned_plan_fields_reject_agent_writers() {
    // Agents may archive a project; a human's value outranks theirs in the fold.
    let mut archived = valid_event("project.declared.agent");
    body_mut(&mut archived).insert("archived".to_owned(), json!(true));
    expect_ok(&archived);

    let mut paused = valid_event("plan.feature.agent");
    body_mut(&mut paused).insert("paused".to_owned(), json!(true));
    assert_error(&paused, "/paused", "human_only_field");

    let mut handoff = valid_event("plan.feature.agent");
    body_mut(&mut handoff).insert("handoff".to_owned(), json!(true));
    assert_error(&handoff, "/handoff", "human_only_field");
}

#[test]
fn human_owned_plan_fields_accept_operator_writers() {
    let mut archived = valid_event("project.declared");
    body_mut(&mut archived).insert("archived".to_owned(), json!(false));
    expect_ok(&archived);

    let mut paused = valid_event("plan.feature");
    body_mut(&mut paused).insert("paused".to_owned(), json!(true));
    body_mut(&mut paused).insert("handoff".to_owned(), json!(false));
    expect_ok(&paused);
}

// ---------------------------------------------------------------------------
// Envelope and JSON limits
// ---------------------------------------------------------------------------

fn raw_parse(line: &[u8], source: &agendar_core::model::SourcePath) -> agendar_core::Result<Event> {
    parse_event(line, source)
}

#[test]
fn envelope_rejects_wrong_version_sequence_and_ids() {
    let mut value = valid_event("project.declared");
    value["v"] = json!(2);
    assert_error(&value, "/v", "unsupported_version");

    let mut value = valid_event("project.declared");
    value["seq"] = json!(0);
    assert_error(&value, "/seq", "sequence_range");

    let mut value = valid_event("project.declared");
    value["seq"] = json!(9_007_199_254_740_992_u64);
    assert_error(&value, "/seq", "sequence_range");

    let mut value = valid_event("project.declared");
    value["id"] = json!("0000000000000000000000000a");
    assert_error_code(&value, "E_VALIDATION");

    let mut value = valid_event("project.declared");
    value["id"] = json!("IIIIIIIIIIIIIIIIIIIIIIIIII");
    assert_error_code(&value, "E_VALIDATION");
}

#[test]
fn envelope_rejects_unknown_and_missing_keys() {
    let mut value = valid_event("project.declared");
    value["extra"] = json!(true);
    assert_error_code(&value, "E_VALIDATION");

    let mut value = valid_event("project.declared");
    value.as_object_mut().expect("object").remove("refs");
    assert_error(&value, "/refs", "missing_field");

    let mut value = valid_event("project.declared");
    value.as_object_mut().expect("object").remove("body");
    assert_error(&value, "/body", "missing_field");

    let mut value = valid_event("project.declared");
    value["kind"] = json!("decision.delivered");
    assert_error(&value, "/kind", "unknown_kind");

    let mut value = valid_event("project.declared");
    value["body"] = json!("not an object");
    assert_error(&value, "/body", "invalid_type");

    let mut value = valid_event("project.declared");
    body_mut(&mut value).insert("extra".to_owned(), json!(true));
    assert_error(&value, "/body", "invalid_body");
}

#[test]
fn envelope_rejects_nulls_floats_duplicate_keys_and_excess_depth() {
    let mut value = valid_event("project.declared");
    body_mut(&mut value)["title"] = Value::Null;
    assert_error_code(&value, "E_VALIDATION");

    let mut value = valid_event("project.declared");
    value["seq"] = json!(1.5);
    assert_error_code(&value, "E_VALIDATION");

    let source = source_for(PROJECT, OPERATOR);
    let base = line(&valid_event("project.declared"));
    let duplicate = base.replacen("\"v\":1,", "\"v\":1,\"v\":1,", 1);
    let error = raw_parse(duplicate.as_bytes(), &source).expect_err("duplicate key");
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert!(error.message().contains("duplicate"), "{error}");

    let mut value = valid_event("project.declared");
    let mut nested = json!(0);
    for _ in 0..40 {
        nested = json!([nested]);
    }
    value["padding"] = nested;
    let error = expect_err(&value);
    assert_eq!(error.detail(), Some("json_nesting_limit"));
}

#[test]
fn line_grammar_requires_one_complete_lf_or_crlf_line() {
    let source = source_for(PROJECT, OPERATOR);
    let base = line(&valid_event("project.declared"));

    raw_parse(base.as_bytes(), &source).expect("LF line accepts");

    let crlf = format!("{}\r\n", base.trim_end_matches('\n'));
    raw_parse(crlf.as_bytes(), &source).expect("CRLF line accepts");

    let no_lf = base.trim_end_matches('\n');
    let error = raw_parse(no_lf.as_bytes(), &source).expect_err("missing LF");
    assert_eq!(error.detail(), Some("line_terminator"));

    let bare_cr = format!("{}\r", base.trim_end_matches('\n'));
    let error = raw_parse(bare_cr.as_bytes(), &source).expect_err("bare CR");
    assert_eq!(error.detail(), Some("line_terminator"));

    let two_lines = format!("{base}{base}");
    let error = raw_parse(two_lines.as_bytes(), &source).expect_err("two lines");
    assert_eq!(error.detail(), Some("line_terminator"));

    let error = raw_parse(b"\n", &source).expect_err("empty line");
    assert_eq!(error.code().as_str(), "E_VALIDATION");

    let mut invalid_utf8 = base.into_bytes();
    invalid_utf8[1] = 0xFF;
    let error = raw_parse(&invalid_utf8, &source).expect_err("invalid UTF-8");
    assert_eq!(error.detail(), Some("invalid_utf8"));
}

#[test]
fn oversized_line_is_rejected_before_body_validation() {
    let mut value = valid_event("project.declared");
    value["padding"] = json!("x".repeat(70_000));
    let error = expect_err(&value);
    assert_eq!(error.detail(), Some("line_size"));
}

#[test]
fn envelope_rejects_bad_project_writer_and_timestamps() {
    let valid_source = source_for(PROJECT, OPERATOR);

    let mut value = valid_event("project.declared");
    value["project"] = json!("Example");
    let error = support::parse_value_with_source(&value, &valid_source).expect_err("bad project");
    assert_eq!(error.code().as_str(), "E_VALIDATION");

    let mut value = valid_event("project.declared");
    value["writer"] = json!("h-00000000000000000000000001/admin");
    let error = support::parse_value_with_source(&value, &valid_source).expect_err("bad writer");
    assert_eq!(error.code().as_str(), "E_VALIDATION");

    for ts in [
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00.000+00:00",
        "2026-01-01T00:00:00.000z",
        "2026-01-01T00:00:00.0001Z",
        "1969-12-31T23:59:59.000Z",
    ] {
        let mut value = valid_event("project.declared");
        value["ts"] = json!(ts);
        assert_error_code(&value, "E_VALIDATION");
    }

    let mut value = valid_event("project.declared");
    value["ts"] = json!("2026-01-01T00:00:00.123Z");
    expect_ok(&value);
}

// ---------------------------------------------------------------------------
// Identifier and primitive limits
// ---------------------------------------------------------------------------

fn node_with_segments(count: usize, width: usize) -> String {
    (0..count)
        .map(|index| {
            let mut segment = format!("s{index}");
            while segment.len() < width {
                segment.push('a');
            }
            segment
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn node_path_limits_and_grammar_are_enforced() {
    let mut valid = valid_event("plan.feature");
    let sixteen = node_with_segments(16, 1);
    insert_ref(&mut valid, "node", &sixteen);
    expect_ok(&valid);

    let mut max_bytes = valid_event("plan.feature");
    insert_ref(&mut max_bytes, "node", &node_with_segments(16, 15));
    expect_ok(&max_bytes);

    let mut too_many = valid_event("plan.feature");
    insert_ref(&mut too_many, "node", &node_with_segments(17, 1));
    assert_error_code(&too_many, "E_VALIDATION");

    let mut too_wide = valid_event("plan.feature");
    insert_ref(&mut too_wide, "node", &"a".repeat(65));
    assert_error_code(&too_wide, "E_VALIDATION");

    let mut too_long = valid_event("plan.feature");
    insert_ref(&mut too_long, "node", &node_with_segments(16, 16));
    assert_error_code(&too_long, "E_VALIDATION");

    for path in ["a//b", "/a", "a/", "a\\b", "../x", "A", "a.b", "a/b/c/"] {
        let mut value = valid_event("plan.feature");
        insert_ref(&mut value, "node", path);
        assert_error_code(&value, "E_VALIDATION");
    }
}

#[test]
fn project_slug_and_member_id_grammar_are_enforced() {
    let slug_64 = "a".repeat(64);
    let mut value = valid_event("project.declared");
    value["project"] = json!(slug_64);
    expect_ok(&value);

    let valid_source = source_for(PROJECT, OPERATOR);
    let mut too_long = valid_event("project.declared");
    too_long["project"] = json!("a".repeat(65));
    let error = support::parse_value_with_source(&too_long, &valid_source).expect_err("slug");
    assert_eq!(error.code().as_str(), "E_VALIDATION");

    let mut bad_option = valid_event("decision.ask");
    body_mut(&mut bad_option)["card"]["options"][0]["id"] = json!("A");
    assert_error_code(&bad_option, "E_VALIDATION");

    let mut bad_option = valid_event("decision.ask");
    body_mut(&mut bad_option)["card"]["options"][0]["id"] = json!("1a");
    assert_error_code(&bad_option, "E_VALIDATION");

    let mut bad_criterion = valid_event("decision.ask");
    body_mut(&mut bad_criterion)["card"]["done_when"][0]["id"] = json!("1done");
    assert_error_code(&bad_criterion, "E_VALIDATION");

    let mut bad_rule = valid_event("rule.set");
    body_mut(&mut bad_rule)["rule_id"] = json!("-bad");
    assert_error_code(&bad_rule, "E_VALIDATION");
}

#[test]
fn sha_and_blob_limits_are_enforced() {
    let mut uppercase = valid_event("delivery");
    body_mut(&mut uppercase)["criteria"][0]["evidence"]["sha256"] =
        json!(DIGEST.to_ascii_uppercase());
    assert_error_code(&uppercase, "E_VALIDATION");

    let mut zero = valid_event("delivery");
    body_mut(&mut zero)["criteria"][0]["evidence"]["bytes"] = json!(0);
    assert_error(&zero, "/criteria/0/evidence/bytes", "blob_size");

    let mut too_big = valid_event("delivery");
    body_mut(&mut too_big)["criteria"][0]["evidence"]["bytes"] = json!(52_428_801);
    assert_error(&too_big, "/criteria/0/evidence/bytes", "blob_size");

    let mut bad_ext = valid_event("delivery");
    body_mut(&mut bad_ext)["criteria"][0]["evidence"]["ext"] = json!("svg");
    assert_error_code(&bad_ext, "E_VALIDATION");

    let mut max_bytes = valid_event("delivery");
    body_mut(&mut max_bytes)["criteria"][0]["evidence"]["bytes"] = json!(52_428_800);
    expect_ok(&max_bytes);
}

#[test]
fn eta_seconds_and_deadline_ranges_are_enforced() {
    let mut zero = valid_event("work.progress.started");
    body_mut(&mut zero)["eta"]["min_s"] = json!(0);
    assert_error_code(&zero, "E_VALIDATION");

    let mut too_long = valid_event("work.progress.started");
    body_mut(&mut too_long)["eta"]["max_s"] = json!(2_592_001);
    assert_error_code(&too_long, "E_VALIDATION");

    let mut reversed = valid_event("work.progress.started");
    body_mut(&mut reversed)["eta"] = json!({"min_s": 120, "max_s": 60});
    assert_error(&reversed, "/eta", "eta_order");

    let mut deadline = valid_event("plan.feature");
    body_mut(&mut deadline).insert(
        "deadline".to_owned(),
        json!({"op": "set", "value": "2026-01-01T00:00:00.000Z"}),
    );
    let event = expect_ok(&deadline);
    assert_round_trip(&event);

    let mut bad_deadline = valid_event("plan.feature");
    body_mut(&mut bad_deadline).insert(
        "deadline".to_owned(),
        json!({"op": "set", "value": "2026-01-01T00:00:00Z"}),
    );
    assert_error_code(&bad_deadline, "E_VALIDATION");

    let mut clear = valid_event("plan.feature");
    body_mut(&mut clear).insert("deadline".to_owned(), json!({"op": "clear"}));
    let event = expect_ok(&clear);
    assert_round_trip(&event);

    let mut bad_op = valid_event("plan.feature");
    body_mut(&mut bad_op).insert("deadline".to_owned(), json!({"op": "later"}));
    assert_error_code(&bad_op, "E_VALIDATION");
}

#[test]
fn duplicate_refs_and_self_references_are_rejected() {
    let mut duplicate_causes = valid_event("rule.set");
    refs_mut(&mut duplicate_causes).insert("causes".to_owned(), json!([REF_ID, REF_ID]));
    assert_error(&duplicate_causes, "/refs/causes/1", "duplicate_ref");

    let mut duplicate_basis = valid_event("work.progress.started");
    refs_mut(&mut duplicate_basis).insert("basis".to_owned(), json!([REF_ID, REF_ID]));
    assert_error(&duplicate_basis, "/refs/basis/1", "duplicate_ref");

    let mut self_ask = valid_event("decision.ask");
    insert_ref(&mut self_ask, "supersedes", EVENT_ID);
    assert_error(&self_ask, "/refs/supersedes", "self_reference");

    let mut self_cause = valid_event("rule.set");
    refs_mut(&mut self_cause).insert("causes".to_owned(), json!([EVENT_ID]));
    assert_error(&self_cause, "/refs/causes/0", "self_reference");

    let mut self_basis = valid_event("work.progress.started");
    refs_mut(&mut self_basis).insert("basis".to_owned(), json!([EVENT_ID]));
    assert_error(&self_basis, "/refs/basis/0", "self_reference");

    let mut too_many = valid_event("rule.set");
    let many: Vec<String> = (0..129)
        .map(|index| format!("{:026}", index + 1000))
        .collect();
    refs_mut(&mut too_many).insert("causes".to_owned(), json!(many));
    assert_error(&too_many, "/refs/causes", "refs_limit");
}

// ---------------------------------------------------------------------------
// Text limits, trimming and control characters
// ---------------------------------------------------------------------------

fn set_project_title(value: &mut Value, title: &str) {
    body_mut(value)["title"] = json!(title);
}

#[test]
fn text_limits_are_counted_in_unicode_scalars() {
    let mut valid = valid_event("project.declared");
    set_project_title(&mut valid, &"a".repeat(100));
    expect_ok(&valid);

    let mut too_long = valid_event("project.declared");
    set_project_title(&mut too_long, &"a".repeat(126));
    assert_error(&too_long, "/title", "length");

    let mut valid_astral = valid_event("project.declared");
    set_project_title(&mut valid_astral, &"😀".repeat(100));
    expect_ok(&valid_astral);

    let mut too_long_astral = valid_event("project.declared");
    set_project_title(&mut too_long_astral, &"😀".repeat(126));
    assert_error(&too_long_astral, "/title", "length");

    let mut trimmed = valid_event("project.declared");
    set_project_title(&mut trimmed, &format!("    {}", "😀".repeat(80)));
    expect_ok(&trimmed);

    for blank in ["", "   ", "\t"] {
        let mut value = valid_event("project.declared");
        set_project_title(&mut value, blank);
        assert_error(&value, "/title", "blank_string");
    }

    for control in ["a\nb", "a\tb", "a\0b", "a\rb"] {
        let mut value = valid_event("project.declared");
        set_project_title(&mut value, control);
        assert_error_code(&value, "E_VALIDATION");
    }

    let mut brief = valid_event("project.declared");
    body_mut(&mut brief).insert("brief".to_owned(), json!("b".repeat(220)));
    expect_ok(&brief);
    let mut brief = valid_event("project.declared");
    body_mut(&mut brief).insert("brief".to_owned(), json!("b".repeat(276)));
    assert_error(&brief, "/brief", "length");

    let mut goal = valid_event("project.declared");
    body_mut(&mut goal).insert("goal".to_owned(), json!("g".repeat(220)));
    expect_ok(&goal);
    let mut goal = valid_event("project.declared");
    body_mut(&mut goal).insert("goal".to_owned(), json!("g".repeat(276)));
    assert_error(&goal, "/goal", "length");
}

#[test]
fn plan_feature_and_note_text_limits_are_enforced() {
    for (field, limit) in [("title", 110), ("slice", 100), ("milestone", 100)] {
        let mut valid = valid_event("plan.feature");
        body_mut(&mut valid).insert(field.to_owned(), json!("a".repeat(limit)));
        expect_ok(&valid);
        let mut invalid = valid_event("plan.feature");
        body_mut(&mut invalid).insert(field.to_owned(), json!("a".repeat(limit + limit / 4 + 1)));
        assert_error(&invalid, &format!("/{field}"), "length");
    }

    let mut pulse = valid_event("pulse.session");
    body_mut(&mut pulse)["message"] = json!("m".repeat(240));
    expect_ok(&pulse);
    let mut pulse = valid_event("pulse.session");
    body_mut(&mut pulse)["message"] = json!("m".repeat(301));
    assert_error(&pulse, "/message", "length");

    let mut note = valid_event("work.progress.done");
    body_mut(&mut note).insert("note".to_owned(), json!("n".repeat(240)));
    expect_ok(&note);
    let mut note = valid_event("work.progress.done");
    body_mut(&mut note).insert("note".to_owned(), json!("n".repeat(301)));
    assert_error(&note, "/note", "length");

    let mut withdrawn = valid_event("decision.withdrawn");
    body_mut(&mut withdrawn)["reason"] = json!("line one\nline two");
    expect_ok(&withdrawn);
    let mut withdrawn = valid_event("decision.withdrawn");
    body_mut(&mut withdrawn)["reason"] = json!("r".repeat(201));
    assert_error(&withdrawn, "/reason", "length");

    let mut answer_note = valid_event("decision.answer");
    body_mut(&mut answer_note).insert("note".to_owned(), json!("n".repeat(500)));
    expect_ok(&answer_note);
    let mut answer_note = valid_event("decision.answer");
    body_mut(&mut answer_note).insert("note".to_owned(), json!("n".repeat(626)));
    assert_error(&answer_note, "/note", "length");
}

#[test]
fn rule_and_review_reason_limits_are_enforced() {
    let mut rule = valid_event("rule.set");
    body_mut(&mut rule)["text"] = json!("r".repeat(500));
    expect_ok(&rule);
    let mut rule = valid_event("rule.set");
    body_mut(&mut rule)["text"] = json!("r".repeat(626));
    assert_error(&rule, "/text", "length");

    let mut review = valid_event("review.result");
    body_mut(&mut review)["verdict"] = json!("send_back");
    body_mut(&mut review).insert("reason".to_owned(), json!("r".repeat(500)));
    expect_ok(&review);

    let mut multiline_review = valid_event("review.result");
    body_mut(&mut multiline_review)["verdict"] = json!("send_back");
    body_mut(&mut multiline_review).insert("reason".to_owned(), json!("line one\nline two"));
    expect_ok(&multiline_review);
    let mut review = valid_event("review.result");
    body_mut(&mut review)["verdict"] = json!("send_back");
    body_mut(&mut review).insert("reason".to_owned(), json!("r".repeat(626)));
    assert_error(&review, "/reason", "length");
}

// ---------------------------------------------------------------------------
// Decision card shape and limits
// ---------------------------------------------------------------------------

fn ask_with_card(card: Value) -> Value {
    event(
        "decision.ask",
        json!({"agent": agent(), "card": card, "policy": policy_eval()}),
        json!({"node": "x"}),
        AGENT,
    )
}

#[test]
fn card_option_structure_and_text_limits_are_enforced() {
    let mut one_option = card();
    one_option["options"] = json!([card()["options"][0].clone()]);
    assert_error(&ask_with_card(one_option), "/card/options", "option_count");

    let mut five_options = card();
    let mut options = five_options["options"].as_array().expect("options").clone();
    for index in 0..3 {
        let mut option = options[0].clone();
        option["id"] = json!(format!("x{index}"));
        options.push(option);
    }
    five_options["options"] = Value::Array(options);
    assert_error(
        &ask_with_card(five_options),
        "/card/options",
        "option_count",
    );

    let mut duplicate = card();
    duplicate["options"][1]["id"] = json!("a");
    assert_error(
        &ask_with_card(duplicate),
        "/card/options/1/id",
        "duplicate_option",
    );

    let mut recommendation = card();
    recommendation["recommended"]["option"] = json!("missing");
    assert_error(
        &ask_with_card(recommendation),
        "/card/recommended/option",
        "unknown_recommendation",
    );

    let mut label = card();
    label["options"][0]["label"] = json!("l".repeat(63));
    assert_error(&ask_with_card(label), "/card/options/0/label", "length");

    let mut label = card();
    label["options"][0]["label"] = json!("😀".repeat(50));
    expect_ok(&ask_with_card(label));

    let mut consequence = card();
    consequence["options"][0]["consequence"] = json!("c".repeat(163));
    assert_error(
        &ask_with_card(consequence),
        "/card/options/0/consequence",
        "length",
    );

    let mut cost = card();
    cost["options"][0]
        .as_object_mut()
        .expect("option")
        .insert("cost".to_owned(), json!("c".repeat(126)));
    assert_error(&ask_with_card(cost), "/card/options/0/cost", "length");

    let mut icon = card();
    icon["options"][0]["icon"] = json!("emoji");
    let unknown = ask_with_card(icon);
    assert_error(&unknown, "/card/options/0/icon", "unknown_icon");
    // The rejection names the icons an agent may use instead.
    let hint = expect_err(&unknown).hint().unwrap_or_default().to_owned();
    assert!(
        hint.contains("git-branch") && hint.contains("circle-dot"),
        "{hint}"
    );

    let mut icon = card();
    icon["options"][0]["icon"] = json!("link");
    expect_ok(&ask_with_card(icon));

    let mut eta = card();
    eta["options"][0]["eta"] = json!({"min_s": 10, "max_s": 5});
    assert_error(&ask_with_card(eta), "/card/options/0/eta", "eta_order");

    let mut because = card();
    because["recommended"]["because"] = json!("b".repeat(163));
    assert_error(
        &ask_with_card(because),
        "/card/recommended/because",
        "length",
    );

    let mut why_now = card();
    why_now["why_now"] = json!("w".repeat(176));
    assert_error(&ask_with_card(why_now), "/card/why_now", "length");

    let mut question = card();
    question["question"] = json!("😀".repeat(138));
    assert_error(&ask_with_card(question), "/card/question", "length");

    let mut question = card();
    question["question"] = json!("😀".repeat(110));
    expect_ok(&ask_with_card(question));
}

#[test]
fn card_facts_and_criteria_are_validated() {
    let mut facts = card();
    facts["already_checked"] = json!([
        {"text": "A", "claim": "agent_says"},
        {"text": "B", "claim": "agent_says"},
        {"text": "C", "claim": "agent_says"},
        {"text": "D", "claim": "agent_says"},
    ]);
    assert_error(
        &ask_with_card(facts),
        "/card/already_checked",
        "facts_limit",
    );

    let mut fact_text = card();
    fact_text["already_checked"] = json!([{"text": "t".repeat(176), "claim": "agent_says"}]);
    assert_error(
        &ask_with_card(fact_text),
        "/card/already_checked/0/text",
        "length",
    );

    let mut seen_without_observation = card();
    seen_without_observation["already_checked"] =
        json!([{"text": "Output matched.", "claim": "seen"}]);
    assert_error(
        &ask_with_card(seen_without_observation),
        "/card/already_checked/0/observation",
        "seen_requires_observation",
    );

    let mut agent_says_with_observation = card();
    agent_says_with_observation["already_checked"] = json!([{
        "text": "Output matched.",
        "claim": "agent_says",
        "observation": REF_ID,
    }]);
    assert_error(
        &ask_with_card(agent_says_with_observation),
        "/card/already_checked/0/observation",
        "agent_says_forbids_observation",
    );

    let mut no_criterion = card();
    no_criterion["done_when"] = json!([]);
    assert_error(
        &ask_with_card(no_criterion),
        "/card/done_when",
        "done_when_count",
    );

    let mut four_criteria = card();
    four_criteria["done_when"] = json!([
        {"id": "a", "text": "A"},
        {"id": "b", "text": "B"},
        {"id": "c", "text": "C"},
        {"id": "d", "text": "D"},
    ]);
    assert_error(
        &ask_with_card(four_criteria),
        "/card/done_when",
        "done_when_count",
    );

    let mut duplicate = card();
    duplicate["done_when"] = json!([
        {"id": "done", "text": "A"},
        {"id": "done", "text": "B"},
    ]);
    assert_error(
        &ask_with_card(duplicate),
        "/card/done_when/1/id",
        "duplicate_criterion",
    );

    let mut criterion_text = card();
    criterion_text["done_when"] = json!([{"id": "done", "text": "t".repeat(176)}]);
    assert_error(
        &ask_with_card(criterion_text),
        "/card/done_when/0/text",
        "length",
    );

    let mut missing_facts = card();
    missing_facts
        .as_object_mut()
        .expect("card")
        .remove("already_checked");
    expect_ok(&ask_with_card(missing_facts));
}

#[test]
fn card_evidence_bullets_and_tables_are_validated() {
    let mut bullets = card();
    bullets["evidence"] = json!({"kind": "bullets", "items": ["A", "B", "C"]});
    expect_ok(&ask_with_card(bullets));

    let mut empty_bullets = card();
    empty_bullets["evidence"] = json!({"kind": "bullets", "items": []});
    assert_error(
        &ask_with_card(empty_bullets),
        "/card/evidence/items",
        "bullets_count",
    );

    let mut four_bullets = card();
    four_bullets["evidence"] = json!({"kind": "bullets", "items": ["A", "B", "C", "D"]});
    assert_error(
        &ask_with_card(four_bullets),
        "/card/evidence/items",
        "bullets_count",
    );

    let mut long_bullet = card();
    long_bullet["evidence"] = json!({"kind": "bullets", "items": ["x".repeat(151)]});
    assert_error(
        &ask_with_card(long_bullet),
        "/card/evidence/items/0",
        "length",
    );

    let mut table = card();
    table["evidence"] = json!({
        "kind": "table",
        "rows": [
            ["h1", "h2", "h3", "h4"],
            ["a", "b", "c", "d"],
            ["1", "2", "3", "4"],
            ["w", "x", "y", "z"],
        ],
    });
    expect_ok(&ask_with_card(table));

    let mut empty_table = card();
    empty_table["evidence"] = json!({"kind": "table", "rows": []});
    assert_error(
        &ask_with_card(empty_table),
        "/card/evidence/rows",
        "table_rows",
    );

    let mut five_rows = card();
    five_rows["evidence"] = json!({
        "kind": "table",
        "rows": [["1"], ["2"], ["3"], ["4"], ["5"]],
    });
    assert_error(
        &ask_with_card(five_rows),
        "/card/evidence/rows",
        "table_rows",
    );

    let mut no_columns = card();
    no_columns["evidence"] = json!({"kind": "table", "rows": [[]]});
    assert_error(
        &ask_with_card(no_columns),
        "/card/evidence/rows/0",
        "table_columns",
    );

    let mut five_columns = card();
    five_columns["evidence"] = json!({
        "kind": "table",
        "rows": [["1", "2", "3", "4", "5"]],
    });
    assert_error(
        &ask_with_card(five_columns),
        "/card/evidence/rows/0",
        "table_columns",
    );

    let mut unequal = card();
    unequal["evidence"] = json!({
        "kind": "table",
        "rows": [["1", "2"], ["3"]],
    });
    assert_error(
        &ask_with_card(unequal),
        "/card/evidence/rows/1",
        "table_columns",
    );

    let mut long_cell = card();
    long_cell["evidence"] = json!({
        "kind": "table",
        "rows": [["x".repeat(151)]],
    });
    assert_error(
        &ask_with_card(long_cell),
        "/card/evidence/rows/0/0",
        "length",
    );
}

#[test]
fn card_timeout_and_report_are_validated() {
    let mut missing = card();
    missing.as_object_mut().expect("card").remove("on_timeout");
    assert_error_code(&ask_with_card(missing), "E_VALIDATION");

    let mut unknown = card();
    unknown["on_timeout"] = json!({"kind": "forever"});
    assert_error_code(&ask_with_card(unknown), "E_VALIDATION");

    let mut after = card();
    after["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 14_400});
    expect_ok(&ask_with_card(after));

    let mut zero = card();
    zero["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 0});
    assert_error_code(&ask_with_card(zero), "E_VALIDATION");

    let mut too_long = card();
    too_long["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 2_592_001});
    assert_error_code(&ask_with_card(too_long), "E_VALIDATION");

    let mut report = card();
    report["report"] = blob();
    expect_ok(&ask_with_card(report));

    let mut empty_report = card();
    empty_report["report"] = json!({
        "host": HOST,
        "sha256": DIGEST,
        "ext": "txt",
        "bytes": 0,
    });
    assert_error(
        &ask_with_card(empty_report),
        "/card/report/bytes",
        "blob_size",
    );

    // SVG is a project icon only.
    let mut svg_report = card();
    svg_report["report"] = json!({"host": HOST, "sha256": DIGEST, "ext": "svg", "bytes": 10});
    assert_error(
        &ask_with_card(svg_report),
        "/card/report/ext",
        "svg_icon_only",
    );
    let mut svg_image = card();
    svg_image["options"][0]["image"] =
        json!({"host": HOST, "sha256": DIGEST, "ext": "svg", "bytes": 10});
    assert_error_code(&ask_with_card(svg_image), "E_VALIDATION");

    let mut extra = card();
    extra["extra"] = json!(true);
    assert_error_code(&ask_with_card(extra), "E_VALIDATION");
}

// ---------------------------------------------------------------------------
// Delivery demos, criteria and disclosure
// ---------------------------------------------------------------------------

#[test]
fn a_delivery_line_without_a_receipt_lists_no_criteria() {
    let mut without = valid_event("delivery");
    remove_ref(&mut without, "receipt");
    assert_error(&without, "/criteria", "delivery_criteria_without_receipt");

    body_mut(&mut without)["criteria"] = json!([]);
    let event = expect_ok(&without);
    assert!(event.refs.receipt.is_none());
}

fn delivery_with_demo(demo: Value) -> Value {
    let mut value = valid_event("delivery");
    body_mut(&mut value)["demos"] = json!([demo]);
    value
}

#[test]
fn delivery_demo_count_and_variants_are_validated() {
    let mut empty = valid_event("delivery");
    body_mut(&mut empty)["demos"] = json!([]);
    assert_error(&empty, "/demos", "demo_count");

    let mut four = valid_event("delivery");
    let demo = json!({"kind": "cli_transcript", "body": {"blob": blob()}});
    body_mut(&mut four)["demos"] = json!([demo.clone(), demo.clone(), demo.clone(), demo]);
    assert_error(&four, "/demos", "demo_count");

    let screenshot = json!({
        "kind": "screenshot",
        "body": {"blob": blob_with_ext("png"), "alt": "Screen"},
    });
    let event = expect_ok(&delivery_with_demo(screenshot.clone()));
    assert_round_trip(&event);

    let mut long_alt = screenshot;
    long_alt["body"]["alt"] = json!("a".repeat(201));
    assert_error(&delivery_with_demo(long_alt), "/demos/0/body/alt", "length");

    let mut bad_screenshot = json!({
        "kind": "screenshot",
        "body": {"blob": blob_with_ext("png"), "alt": "Screen"},
    });
    bad_screenshot["body"]["blob"]["bytes"] = json!(0);
    assert_error(
        &delivery_with_demo(bad_screenshot),
        "/demos/0/body/blob/bytes",
        "blob_size",
    );

    for url in ["https://example.com/path", "http://example.com"] {
        let preview = json!({"kind": "preview_url", "body": {"url": url}});
        expect_ok(&delivery_with_demo(preview));
    }

    let ftp = json!({"kind": "preview_url", "body": {"url": "ftp://example.com"}});
    assert_error(
        &delivery_with_demo(ftp),
        "/demos/0/body/url",
        "preview_url_scheme",
    );

    let credentials =
        json!({"kind": "preview_url", "body": {"url": "https://user:pass@example.com"}});
    assert_error(
        &delivery_with_demo(credentials),
        "/demos/0/body/url",
        "preview_url_credentials",
    );

    let malformed = json!({"kind": "preview_url", "body": {"url": "not a url"}});
    assert_error(
        &delivery_with_demo(malformed),
        "/demos/0/body/url",
        "preview_url_invalid",
    );

    let too_long = json!({"kind": "preview_url", "body": {"url": "a".repeat(2049)}});
    assert_error(&delivery_with_demo(too_long), "/demos/0/body/url", "length");

    let recording = json!({
        "kind": "recording",
        "body": {"blob": blob_with_ext("mp4"), "poster": blob_with_ext("png")},
    });
    let event = expect_ok(&delivery_with_demo(recording.clone()));
    assert_round_trip(&event);

    let mut bad_poster = recording;
    bad_poster["body"]["poster"]["bytes"] = json!(0);
    assert_error(
        &delivery_with_demo(bad_poster),
        "/demos/0/body/poster/bytes",
        "blob_size",
    );

    let unknown = json!({"kind": "hologram", "body": {"blob": blob()}});
    assert_error_code(&delivery_with_demo(unknown), "E_VALIDATION");

    let extra = json!({"kind": "cli_transcript", "body": {"blob": blob()}, "extra": true});
    assert_error_code(&delivery_with_demo(extra), "E_VALIDATION");
}

#[test]
fn delivery_demo_extensions_must_match_the_demo_kind() {
    for ext in ["png", "jpg", "webp"] {
        let screenshot = json!({
            "kind": "screenshot",
            "body": {"blob": blob_with_ext(ext), "alt": "Screen"},
        });
        let event = expect_ok(&delivery_with_demo(screenshot));
        assert_round_trip(&event);
    }
    for ext in ["txt", "md", "html"] {
        let transcript = json!({"kind": "cli_transcript", "body": {"blob": blob_with_ext(ext)}});
        let event = expect_ok(&delivery_with_demo(transcript));
        assert_round_trip(&event);
    }
    for ext in ["mp4", "webm"] {
        let recording = json!({
            "kind": "recording",
            "body": {"blob": blob_with_ext(ext), "poster": blob_with_ext("webp")},
        });
        let event = expect_ok(&delivery_with_demo(recording));
        assert_round_trip(&event);
    }

    let text_screenshot = json!({
        "kind": "screenshot",
        "body": {"blob": blob(), "alt": "Screen"},
    });
    assert_error(
        &delivery_with_demo(text_screenshot),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let video_screenshot = json!({
        "kind": "screenshot",
        "body": {"blob": blob_with_ext("mp4"), "alt": "Screen"},
    });
    assert_error(
        &delivery_with_demo(video_screenshot),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let video_transcript =
        json!({"kind": "cli_transcript", "body": {"blob": blob_with_ext("mp4")}});
    assert_error(
        &delivery_with_demo(video_transcript),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let image_transcript =
        json!({"kind": "cli_transcript", "body": {"blob": blob_with_ext("png")}});
    assert_error(
        &delivery_with_demo(image_transcript),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let text_recording = json!({
        "kind": "recording",
        "body": {"blob": blob(), "poster": blob_with_ext("png")},
    });
    assert_error(
        &delivery_with_demo(text_recording),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let text_poster = json!({
        "kind": "recording",
        "body": {"blob": blob_with_ext("mp4"), "poster": blob()},
    });
    assert_error(
        &delivery_with_demo(text_poster),
        "/demos/0/body/poster/ext",
        "demo_extension_mismatch",
    );

    let video_poster = json!({
        "kind": "recording",
        "body": {"blob": blob_with_ext("mp4"), "poster": blob_with_ext("mp4")},
    });
    assert_error(
        &delivery_with_demo(video_poster),
        "/demos/0/body/poster/ext",
        "demo_extension_mismatch",
    );
}

// ---------------------------------------------------------------------------
// Exact nested object shapes
// ---------------------------------------------------------------------------

/// Replace an object-shaped slot with a positional array of its field values,
/// which a derived serde struct decoder would otherwise accept.
fn positional(value: &Value) -> Value {
    Value::Array(
        value
            .as_object()
            .expect("fixture slot is an object")
            .values()
            .cloned()
            .collect(),
    )
}

fn replace_with_positional(value: &mut Value, path: &[&str]) {
    let mut slot = value;
    for segment in &path[..path.len() - 1] {
        slot = slot_mut(slot, segment);
    }
    let target = slot_mut(slot, path[path.len() - 1]);
    let replacement = positional(target);
    *target = replacement;
}

fn slot_mut<'a>(value: &'a mut Value, segment: &str) -> &'a mut Value {
    if value.is_array() {
        let index: usize = segment.parse().expect("fixture array index segment");
        value.get_mut(index).expect("fixture array index exists")
    } else {
        value.get_mut(segment).expect("fixture object key exists")
    }
}

fn assert_object_shape_required(mut value: Value, path: &[&str], field: &str) {
    replace_with_positional(&mut value, path);
    assert_error(&value, field, "object_shape_required");
}

fn collect_object_paths(value: &Value, path: String, paths: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            if !path.is_empty() {
                paths.push(path.clone());
            }
            for (key, child) in fields {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                collect_object_paths(child, child_path, paths);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_object_paths(child, format!("{path}/{index}"), paths);
            }
        }
        _ => {}
    }
}

#[test]
fn nested_object_slots_reject_positional_arrays() {
    let agent_declaration = valid_event("project.declared.agent");
    assert_object_shape_required(agent_declaration, &["body", "agent"], "/body/agent");

    let feature_agent = valid_event("plan.feature.agent");
    assert_object_shape_required(feature_agent, &["body", "agent"], "/body/agent");

    let mut deadline = valid_event("plan.feature");
    insert_field(body_mut(&mut deadline), "deadline", json!({"op": "clear"}));
    assert_object_shape_required(deadline, &["body", "deadline"], "/body/deadline");

    let progress_agent = valid_event("work.progress.started");
    assert_object_shape_required(progress_agent, &["body", "agent"], "/body/agent");

    let progress_eta = valid_event("work.progress.started");
    assert_object_shape_required(progress_eta, &["body", "eta"], "/body/eta");

    let mut pulse_eta = valid_event("pulse.work");
    insert_field(
        body_mut(&mut pulse_eta),
        "eta",
        json!({"min_s": 60, "max_s": 120}),
    );
    assert_object_shape_required(pulse_eta, &["body", "eta"], "/body/eta");

    let ask_card = valid_event("decision.ask");
    assert_object_shape_required(ask_card, &["body", "card"], "/body/card");

    let ask_policy = valid_event("decision.ask");
    assert_object_shape_required(ask_policy, &["body", "policy"], "/body/policy");

    let ask_option = valid_event("decision.ask");
    assert_object_shape_required(
        ask_option,
        &["body", "card", "options", "0"],
        "/body/card/options/0",
    );

    let ask_recommendation = valid_event("decision.ask");
    assert_object_shape_required(
        ask_recommendation,
        &["body", "card", "recommended"],
        "/body/card/recommended",
    );

    let mut ask_fact = valid_event("decision.ask");
    body_mut(&mut ask_fact)["card"]["already_checked"] =
        json!([{"text": "Output matched.", "claim": "agent_says"}]);
    assert_object_shape_required(
        ask_fact,
        &["body", "card", "already_checked", "0"],
        "/body/card/already_checked/0",
    );

    let ask_criterion = valid_event("decision.ask");
    assert_object_shape_required(
        ask_criterion,
        &["body", "card", "done_when", "0"],
        "/body/card/done_when/0",
    );

    let ask_timeout = valid_event("decision.ask");
    assert_object_shape_required(
        ask_timeout,
        &["body", "card", "on_timeout"],
        "/body/card/on_timeout",
    );

    let mut ask_evidence = valid_event("decision.ask");
    insert_field(
        body_mut(&mut ask_evidence)["card"]
            .as_object_mut()
            .expect("card"),
        "evidence",
        json!({"kind": "bullets", "items": ["One"]}),
    );
    assert_object_shape_required(
        ask_evidence,
        &["body", "card", "evidence"],
        "/body/card/evidence",
    );

    let mut ask_report = valid_event("decision.ask");
    insert_field(
        body_mut(&mut ask_report)["card"]
            .as_object_mut()
            .expect("card"),
        "report",
        blob(),
    );
    assert_object_shape_required(ask_report, &["body", "card", "report"], "/body/card/report");

    let ask_budget = valid_event("decision.ask");
    assert_object_shape_required(
        ask_budget,
        &["body", "policy", "budget"],
        "/body/policy/budget",
    );

    let mut answer_eta = valid_event("decision.answer");
    insert_field(
        body_mut(&mut answer_eta),
        "expected_eta",
        json!({"min_s": 60, "max_s": 120}),
    );
    assert_object_shape_required(answer_eta, &["body", "expected_eta"], "/body/expected_eta");

    let ack_agent = valid_event("decision.ack");
    assert_object_shape_required(ack_agent, &["body", "agent"], "/body/agent");

    let delivery_agent = valid_event("delivery");
    assert_object_shape_required(delivery_agent, &["body", "agent"], "/body/agent");

    let delivery_criterion = valid_event("delivery");
    assert_object_shape_required(
        delivery_criterion,
        &["body", "criteria", "0"],
        "/body/criteria/0",
    );

    let delivery_evidence = valid_event("delivery");
    assert_object_shape_required(
        delivery_evidence,
        &["body", "criteria", "0", "evidence"],
        "/body/criteria/0/evidence",
    );

    let delivery_demo = valid_event("delivery");
    assert_object_shape_required(delivery_demo, &["body", "demos", "0"], "/body/demos/0");

    let mut demo_body = valid_event("delivery");
    body_mut(&mut demo_body)["demos"] = json!([{
        "kind": "screenshot",
        "body": {"blob": blob_with_ext("png"), "alt": "Screen"},
    }]);
    assert_object_shape_required(
        demo_body,
        &["body", "demos", "0", "body"],
        "/body/demos/0/body",
    );

    let delivery_disclosure = valid_event("delivery");
    assert_object_shape_required(
        delivery_disclosure,
        &["body", "disclosure"],
        "/body/disclosure",
    );

    let observation_method = valid_event("evidence.observed");
    assert_object_shape_required(observation_method, &["body", "method"], "/body/method");

    let observation_output = valid_event("evidence.observed");
    assert_object_shape_required(observation_output, &["body", "output"], "/body/output");

    let mut review_rule = valid_event("review.result");
    body_mut(&mut review_rule)["verdict"] = json!("send_back");
    body_mut(&mut review_rule).insert("reason".to_owned(), json!("Check disagrees."));
    body_mut(&mut review_rule).insert(
        "save_rule".to_owned(),
        json!({"rule_id": "no-shell", "text": "Use explicit argv.", "enabled": true}),
    );
    assert_object_shape_required(review_rule, &["body", "save_rule"], "/body/save_rule");

    let policy_cells = valid_event("policy.set");
    assert_object_shape_required(policy_cells, &["body", "cells"], "/body/cells");

    let policy_budget = valid_event("policy.set");
    assert_object_shape_required(policy_budget, &["body", "budget"], "/body/budget");

    let mut refs = valid_event("project.declared");
    refs["refs"] = json!([{"node": "x"}]);
    assert_error(&refs, "/refs", "object_shape_required");
}

#[test]
fn semantic_arrays_still_accept() {
    let mut evidence = valid_event("decision.ask");
    insert_field(
        body_mut(&mut evidence)["card"]
            .as_object_mut()
            .expect("card"),
        "evidence",
        json!({
            "kind": "table",
            "rows": [["Header", "Value"], ["Seen", "yes"]],
        }),
    );
    let event = expect_ok(&evidence);
    assert_round_trip(&event);

    let mut facts = valid_event("decision.ask");
    body_mut(&mut facts)["card"]["already_checked"] = json!([
        {"text": "Output matched.", "claim": "agent_says"},
        {"text": "Digest matched.", "claim": "agent_says"},
    ]);
    let event = expect_ok(&facts);
    assert_round_trip(&event);

    let mut depends = valid_event("plan.feature");
    insert_field(body_mut(&mut depends), "depends_on", json!(["a", "a/b"]));
    let event = expect_ok(&depends);
    assert_round_trip(&event);

    let mut restarted = valid_event("decision.ack");
    body_mut(&mut restarted)["restarted"] = json!(["a", "b"]);
    let event = expect_ok(&restarted);
    assert_round_trip(&event);

    let mut disclosure = valid_event("delivery");
    body_mut(&mut disclosure)["disclosure"] = json!({
        "proven": ["One", "Two"],
        "not_proven_yet": ["Three"],
        "faked_for_now": ["Four"],
    });
    let event = expect_ok(&disclosure);
    assert_round_trip(&event);

    let mut causes = valid_event("project.declared");
    refs_mut(&mut causes).insert("causes".to_owned(), json!([REF_ID]));
    let event = expect_ok(&causes);
    assert_round_trip(&event);
}

/// The exact malformed inputs reported by review round 1, applied to the frozen
/// seed events rather than to test fixtures.
#[test]
fn every_object_slot_in_frozen_seeds_rejects_positional_arrays() {
    let seeds = [
        ANSWER_CONFLICT,
        CONCURRENT_CONSUMPTION,
        CONCURRENT_OBSERVATIONS,
        DELIVERY_SEEN_REVIEW,
        ETA_FIVE_FEATURES,
        ETA_ONE_FEATURE,
        ETA_OVERLAPPING,
        PLAN_CYCLE,
        RANKING_TWO_ASKS,
        TRANSITIVE_TAINT,
    ];
    let mut checked = 0_u32;
    for seed in seeds {
        for seed_line in seed.lines().filter(|line| !line.is_empty()) {
            let value: Value = serde_json::from_str(seed_line).expect("seed line is JSON");
            let mut paths = Vec::new();
            collect_object_paths(&value, String::new(), &mut paths);
            for path in paths {
                let mut mutated = value.clone();
                let segments: Vec<&str> = path.split('/').collect();
                replace_with_positional(&mut mutated, &segments);
                let error = parse_value(&mutated)
                    .expect_err("a positional array must never satisfy an object slot");
                assert_eq!(
                    error.detail(),
                    Some("object_shape_required"),
                    "{path} decoded as an object: {error}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 300, "only {checked} object slots were checked");
}

#[test]
fn review_round_1_reported_wire_inputs_are_rejected() {
    let started = seed_event_with_kind(ETA_ONE_FEATURE, "work.progress");
    assert_eq!(started["body"]["status"], json!("started"));

    let mut eta_array = started.clone();
    let eta = eta_array["body"]["eta"].clone();
    insert_field(body_mut(&mut eta_array), "eta", positional(&eta));
    assert_error(&eta_array, "/body/eta", "object_shape_required");

    let mut agent_array = started.clone();
    let agent = agent_array["body"]["agent"].clone();
    insert_field(body_mut(&mut agent_array), "agent", positional(&agent));
    assert_error(&agent_array, "/body/agent", "object_shape_required");

    let delivery = seed_event_with_kind(DELIVERY_SEEN_REVIEW, "delivery");
    let text_blob = delivery["body"]["criteria"][0]["evidence"].clone();
    assert_eq!(text_blob["ext"], json!("txt"));

    let mut screenshot_text = delivery.clone();
    insert_field(
        body_mut(&mut screenshot_text),
        "demos",
        json!([{"kind": "screenshot", "body": {"blob": text_blob, "alt": "Screen"}}]),
    );
    assert_error(
        &screenshot_text,
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let mut recording_text = delivery.clone();
    insert_field(
        body_mut(&mut recording_text),
        "demos",
        json!([{"kind": "recording", "body": {"blob": text_blob, "poster": text_blob}}]),
    );
    assert_error(
        &recording_text,
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let mut video_transcript = delivery;
    let mut video_blob = text_blob;
    video_blob["ext"] = json!("mp4");
    insert_field(
        body_mut(&mut video_transcript),
        "demos",
        json!([{"kind": "cli_transcript", "body": {"blob": video_blob}}]),
    );
    assert_error(
        &video_transcript,
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let mut distinct_method_digest =
        seed_event_with_kind(DELIVERY_SEEN_REVIEW, "evidence.observed");
    let method_digest = "a".repeat(64);
    insert_field(
        body_mut(&mut distinct_method_digest),
        "method",
        json!({"kind": "blob_digest", "sha256": method_digest}),
    );
    let event = expect_ok(&distinct_method_digest);
    let EventPayload::EvidenceObserved(body) = &event.payload else {
        panic!("expected evidence.observed, got {:?}", event.payload);
    };
    assert_eq!(body.digest, body.output.sha256);
    assert_eq!(
        body.method,
        ObservationMethod::BlobDigest {
            sha256: Sha256::parse(&method_digest).expect("sha256 literal")
        }
    );
    assert_round_trip(&event);
}

#[test]
fn delivery_criteria_reject_seen_and_observation() {
    let mut seen = valid_event("delivery");
    body_mut(&mut seen)["criteria"][0]["claim"] = json!("seen");
    assert_error(
        &seen,
        "/criteria/0/claim",
        "delivery_seen_requires_later_observation",
    );

    let mut seen_with_observation = valid_event("delivery");
    body_mut(&mut seen_with_observation)["criteria"][0]["claim"] = json!("seen");
    body_mut(&mut seen_with_observation)["criteria"][0]
        .as_object_mut()
        .expect("criterion")
        .insert("observation".to_owned(), json!(REF_ID));
    assert_error(
        &seen_with_observation,
        "/criteria/0/claim",
        "delivery_seen_requires_later_observation",
    );

    let mut observation = valid_event("delivery");
    body_mut(&mut observation)["criteria"][0]
        .as_object_mut()
        .expect("criterion")
        .insert("observation".to_owned(), json!(REF_ID));
    assert_error(
        &observation,
        "/criteria/0/observation",
        "delivery_observation_forbidden",
    );

    let mut duplicate = valid_event("delivery");
    let result = body_mut(&mut duplicate)["criteria"][0].clone();
    body_mut(&mut duplicate)["criteria"] = json!([result.clone(), result]);
    assert_error(&duplicate, "/criteria/1/criterion", "duplicate_criterion");
}

#[test]
fn delivery_name_and_disclosure_are_validated() {
    let mut name = valid_event("delivery");
    body_mut(&mut name)["name"] = json!("n".repeat(101));
    assert_error(&name, "/name", "length");

    let mut ten = valid_event("delivery");
    body_mut(&mut ten)["disclosure"]["proven"] = Value::Array(vec![json!("p"); 10]);
    expect_ok(&ten);

    let mut eleven = valid_event("delivery");
    body_mut(&mut eleven)["disclosure"]["proven"] = Value::Array(vec![json!("p"); 11]);
    assert_error(&eleven, "/disclosure/proven", "disclosure_limit");

    let mut long = valid_event("delivery");
    body_mut(&mut long)["disclosure"]["proven"] = json!(["p".repeat(301)]);
    assert_error(&long, "/disclosure/proven/0", "length");

    let mut multiline = valid_event("delivery");
    body_mut(&mut multiline)["disclosure"]["proven"] = json!(["proven one\nproven two\tstill"]);
    expect_ok(&multiline);

    let mut carriage = valid_event("delivery");
    body_mut(&mut carriage)["disclosure"]["proven"] = json!(["proven\rreturn"]);
    assert_error_code(&carriage, "E_VALIDATION");

    let mut blank = valid_event("delivery");
    body_mut(&mut blank)["disclosure"]["proven"] = json!([""]);
    assert_error(&blank, "/disclosure/proven/0", "blank_string");
}

// ---------------------------------------------------------------------------
// Work progress and observations
// ---------------------------------------------------------------------------

#[test]
fn work_progress_status_refs_and_evidence_are_validated() {
    let mut missing_eta = valid_event("work.progress.started");
    body_mut(&mut missing_eta).remove("eta");
    assert_error(&missing_eta, "/eta", "started_requires_eta");

    let mut started_with_attempt = valid_event("work.progress.started");
    insert_ref(&mut started_with_attempt, "attempt", REF_ID);
    assert_error_code(&started_with_attempt, "E_VALIDATION");

    let mut done_with_eta = valid_event("work.progress.done");
    body_mut(&mut done_with_eta).insert("eta".to_owned(), json!({"min_s": 10, "max_s": 20}));
    assert_error(&done_with_eta, "/eta", "eta_only_started");

    let mut done_without_attempt = valid_event("work.progress.done");
    remove_ref(&mut done_without_attempt, "attempt");
    assert_error(
        &done_without_attempt,
        "/refs/attempt",
        "required_ref_missing",
    );

    let mut verified_without_passed = valid_event("work.progress.verified");
    body_mut(&mut verified_without_passed)["evidence"] = json!([]);
    assert_error(
        &verified_without_passed,
        "/evidence",
        "verified_requires_passed_evidence",
    );

    let mut passed_without_proof = valid_event("work.progress.verified");
    body_mut(&mut passed_without_proof)["evidence"] = json!([{
        "criterion": "done",
        "status": "passed",
        "claim": "agent_says",
    }]);
    assert_error(
        &passed_without_proof,
        "/evidence",
        "verified_requires_passed_evidence",
    );

    let mut seen_without_observation = valid_event("work.progress.verified");
    body_mut(&mut seen_without_observation)["evidence"] = json!([{
        "criterion": "done",
        "status": "passed",
        "claim": "seen",
    }]);
    assert_error(
        &seen_without_observation,
        "/evidence/0/observation",
        "seen_requires_observation",
    );

    let mut agent_says_with_observation = valid_event("work.progress.verified");
    body_mut(&mut agent_says_with_observation)["evidence"] = json!([{
        "criterion": "done",
        "status": "passed",
        "claim": "agent_says",
        "observation": REF_ID,
    }]);
    assert_error(
        &agent_says_with_observation,
        "/evidence/0/observation",
        "agent_says_forbids_observation",
    );

    let mut four_equal = valid_event("work.progress.done");
    body_mut(&mut four_equal)["evidence"] = json!([
        {"criterion": "a", "status": "passed", "claim": "agent_says", "evidence": blob()},
        {"criterion": "b", "status": "passed", "claim": "agent_says", "evidence": blob()},
        {"criterion": "c", "status": "passed", "claim": "agent_says", "evidence": blob()},
        {"criterion": "d", "status": "passed", "claim": "agent_says", "evidence": blob()},
    ]);
    assert_error(&four_equal, "/evidence", "evidence_limit");

    let mut duplicate = valid_event("work.progress.done");
    body_mut(&mut duplicate)["evidence"] = json!([
        {"criterion": "a", "status": "passed", "claim": "agent_says", "evidence": blob()},
        {"criterion": "a", "status": "passed", "claim": "agent_says", "evidence": blob()},
    ]);
    assert_error(&duplicate, "/evidence/1/criterion", "duplicate_criterion");
}

#[test]
fn progress_observation_must_be_in_causes() {
    let mut missing = valid_event("work.progress.verified");
    body_mut(&mut missing)["evidence"] = json!([{
        "criterion": "done",
        "status": "passed",
        "claim": "seen",
        "observation": REF_ID,
    }]);
    assert_error(
        &missing,
        "/evidence/0/observation",
        "body_reference_missing_from_causes",
    );

    let mut with_cause = valid_event("work.progress.verified");
    body_mut(&mut with_cause)["evidence"] = json!([{
        "criterion": "done",
        "status": "passed",
        "claim": "seen",
        "observation": REF_ID,
    }]);
    refs_mut(&mut with_cause).insert("causes".to_owned(), json!([REF_ID]));
    expect_ok(&with_cause);
}

#[test]
fn evidence_observed_checks_times_digests_and_host() {
    let mut finished_before = valid_event("evidence.observed");
    body_mut(&mut finished_before)["finished_at"] = json!("2025-12-31T23:59:59.000Z");
    assert_error(&finished_before, "/finished_at", "finished_before_started");

    let mut digest = valid_event("evidence.observed");
    body_mut(&mut digest)["digest"] = json!("a".repeat(64));
    assert_error(&digest, "/digest", "digest_output_mismatch");

    let mut host = valid_event("evidence.observed");
    body_mut(&mut host)["output"]["host"] = json!(HOST2);
    assert_error(&host, "/output/host", "output_host_mismatch");

    let mut empty_output = valid_event("evidence.observed");
    body_mut(&mut empty_output)["output"]["bytes"] = json!(0);
    assert_error(&empty_output, "/output/bytes", "blob_size");

    let mut blob_digest = valid_event("evidence.observed");
    body_mut(&mut blob_digest)["method"] = json!({"kind": "blob_digest", "sha256": DIGEST});
    let event = expect_ok(&blob_digest);
    assert_round_trip(&event);

    // Only requires `digest == output.sha256`; the checked
    // artifact digest may legitimately differ from the check-output digest.
    let mut distinct_method_digest = valid_event("evidence.observed");
    let method_digest = "b".repeat(64);
    body_mut(&mut distinct_method_digest)["method"] =
        json!({"kind": "blob_digest", "sha256": method_digest});
    let event = expect_ok(&distinct_method_digest);
    match &event.payload {
        EventPayload::EvidenceObserved(body) => {
            assert_eq!(body.digest.as_str(), DIGEST);
            assert_eq!(body.output.sha256.as_str(), DIGEST);
            assert_eq!(
                body.method,
                ObservationMethod::BlobDigest {
                    sha256: Sha256::parse(&method_digest).expect("sha256 literal")
                }
            );
        }
        other => panic!("expected evidence.observed, got {other:?}"),
    }
    assert_round_trip(&event);

    let mut blank_check = valid_event("evidence.observed");
    body_mut(&mut blank_check)["method"]["check_id"] = json!("");
    assert_error(&blank_check, "/method/check_id", "blank_string");

    let mut blank_approval = valid_event("evidence.observed");
    body_mut(&mut blank_approval)["approval_id"] = json!("");
    assert_error(&blank_approval, "/approval_id", "blank_string");

    let mut unknown_method = valid_event("evidence.observed");
    body_mut(&mut unknown_method)["method"] = json!({"kind": "camera"});
    assert_error_code(&unknown_method, "E_VALIDATION");

    let mut bad_time = valid_event("evidence.observed");
    body_mut(&mut bad_time)["started_at"] = json!("2026-01-01T00:00:00Z");
    assert_error_code(&bad_time, "E_VALIDATION");
}

// ---------------------------------------------------------------------------
// Reviews, rules, policy and policy evaluation
// ---------------------------------------------------------------------------

#[test]
fn review_result_requires_reason_for_send_back_and_rule_only_then() {
    let mut missing_reason = valid_event("review.result");
    body_mut(&mut missing_reason)["verdict"] = json!("send_back");
    assert_error(&missing_reason, "/reason", "send_back_requires_reason");

    let mut blank_reason = valid_event("review.result");
    body_mut(&mut blank_reason)["verdict"] = json!("send_back");
    body_mut(&mut blank_reason).insert("reason".to_owned(), json!("   "));
    assert_error(&blank_reason, "/reason", "blank_string");

    let mut valid_send_back = valid_event("review.result");
    body_mut(&mut valid_send_back)["verdict"] = json!("send_back");
    body_mut(&mut valid_send_back).insert("reason".to_owned(), json!("Check disagrees."));
    expect_ok(&valid_send_back);

    let mut accept_rule = valid_event("review.result");
    body_mut(&mut accept_rule).insert(
        "save_rule".to_owned(),
        json!({"rule_id": "no-shell", "text": "Use explicit argv.", "enabled": true}),
    );
    assert_error(&accept_rule, "/save_rule", "save_rule_requires_send_back");

    let mut send_back_rule = valid_event("review.result");
    body_mut(&mut send_back_rule)["verdict"] = json!("send_back");
    body_mut(&mut send_back_rule).insert("reason".to_owned(), json!("Check disagrees."));
    body_mut(&mut send_back_rule).insert(
        "save_rule".to_owned(),
        json!({"rule_id": "no-shell", "text": "Use explicit argv.", "enabled": true}),
    );
    expect_ok(&send_back_rule);

    let mut revision_rule = valid_event("review.result");
    body_mut(&mut revision_rule)["verdict"] = json!("send_back");
    body_mut(&mut revision_rule).insert("reason".to_owned(), json!("Check disagrees."));
    body_mut(&mut revision_rule).insert(
        "save_rule".to_owned(),
        json!({
            "rule_id": "no-shell",
            "text": "Use explicit argv.",
            "enabled": true,
            "revision": DIGEST,
        }),
    );
    assert_error_code(&revision_rule, "E_VALIDATION");

    let mut accepted_with_reason = valid_event("review.result");
    body_mut(&mut accepted_with_reason).insert("reason".to_owned(), json!("Looks good."));
    expect_ok(&accepted_with_reason);
}

fn send_back_with(comments: Value) -> Value {
    let mut event = valid_event("review.result");
    body_mut(&mut event)["verdict"] = json!("send_back");
    body_mut(&mut event).insert("comments".to_owned(), comments);
    event
}

fn diff_comment() -> Value {
    json!({
        "anchor": {"kind": "diff_line", "demo": 0, "file": "src/a.rs", "side": "new", "line": 840, "quote": "let x = 1;"},
        "text": "Use the helper here."
    })
}

#[test]
fn review_comments_round_trip_and_are_validated() {
    let comments = json!([
        diff_comment(),
        {"anchor": {"kind": "transcript_line", "demo": 1, "line": 12, "quote": ""}, "text": "Missing output."},
        {"anchor": {"kind": "region", "demo": 2, "x": 100, "y": 50, "w": 300, "h": 150}, "text": "Clipped."},
    ]);
    // A send-back with only comments needs no reason, and survives a round trip.
    let event = expect_ok(&send_back_with(comments.clone()));
    let EventPayload::ReviewResult(body) = &event.payload else {
        panic!("review.result");
    };
    assert_eq!(body.comments.len(), 3);
    assert_eq!(body.reason, None);
    let wire = serde_json::to_value(body).expect("serialize");
    assert_eq!(wire["comments"], comments);
    assert!(wire.get("reason").is_none());
    let back: agendar_core::model::ReviewResult =
        serde_json::from_value(wire).expect("deserialize");
    assert_eq!(&back, body);
    // No comments: the field is not written at all.
    let plain = agendar_core::model::ReviewResult {
        comments: Vec::new(),
        ..back
    };
    assert!(
        serde_json::to_value(&plain)
            .expect("plain")
            .get("comments")
            .is_none()
    );

    let mut accept = valid_event("review.result");
    body_mut(&mut accept).insert("comments".to_owned(), json!([diff_comment()]));
    assert_error(&accept, "/comments", "comments_require_send_back");

    assert_error(
        &send_back_with(json!([])),
        "/reason",
        "send_back_requires_reason",
    );

    let region = |x: u16, y: u16, w: u16, h: u16| json!([{"anchor": {"kind": "region", "demo": 0, "x": x, "y": y, "w": w, "h": h}, "text": "here"}]);
    for bad in [
        region(900, 0, 101, 10),
        region(0, 999, 10, 2),
        region(0, 0, 0, 10),
        region(0, 0, 10, 0),
    ] {
        assert_error(
            &send_back_with(bad),
            "/comments/0/anchor",
            "region_out_of_range",
        );
    }
    expect_ok(&send_back_with(region(0, 0, 1000, 1000)));
    // Floats are not integers.
    assert_error_code(
        &send_back_with(
            json!([{"anchor": {"kind": "region", "demo": 0, "x": 0.5, "y": 0, "w": 10, "h": 10}, "text": "here"}]),
        ),
        "E_VALIDATION",
    );

    let many: Vec<Value> = (0..21).map(|_| diff_comment()).collect();
    assert_error(
        &send_back_with(json!(many)),
        "/comments",
        "too_many_comments",
    );
    let twenty: Vec<Value> = (0..20).map(|_| diff_comment()).collect();
    expect_ok(&send_back_with(json!(twenty)));

    let mut empty = diff_comment();
    empty["text"] = json!(" ");
    assert_error(
        &send_back_with(json!([empty])),
        "/comments/0/text",
        "blank_string",
    );

    let mut demo = diff_comment();
    demo["anchor"]["demo"] = json!(3);
    assert_error(
        &send_back_with(json!([demo])),
        "/comments/0/anchor/demo",
        "demo_out_of_range",
    );

    let mut zero = diff_comment();
    zero["anchor"]["line"] = json!(0);
    assert_error(
        &send_back_with(json!([zero])),
        "/comments/0/anchor/line",
        "line_out_of_range",
    );

    let mut long_quote = diff_comment();
    long_quote["anchor"]["quote"] = json!("x".repeat(201));
    assert_error(
        &send_back_with(json!([long_quote])),
        "/comments/0/anchor/quote",
        "length",
    );

    let mut extra = diff_comment();
    extra["anchor"]["color"] = json!("red");
    assert_error_code(&send_back_with(json!([extra])), "E_VALIDATION");

    let mut unknown = diff_comment();
    unknown["anchor"]["kind"] = json!("word");
    assert_error_code(&send_back_with(json!([unknown])), "E_VALIDATION");
}

#[test]
fn rule_set_text_and_revision_shape_are_validated() {
    let mut blank = valid_event("rule.set");
    body_mut(&mut blank)["text"] = json!("");
    assert_error(&blank, "/text", "blank_string");

    let mut revision = valid_event("rule.set");
    body_mut(&mut revision).insert("revision".to_owned(), json!(DIGEST));
    assert_error_code(&revision, "E_VALIDATION");

    let mut disabled = valid_event("rule.set");
    body_mut(&mut disabled)["enabled"] = json!(false);
    expect_ok(&disabled);
}

#[test]
fn policy_presets_cells_and_budget_bounds_are_enforced() {
    let mut mismatch = valid_event("policy.set");
    body_mut(&mut mismatch)["cells"]["easy_local"] = json!("ask");
    assert_error(&mismatch, "/cells/easy_local", "preset_cells_mismatch");

    let mut timeout_mismatch = valid_event("policy.set");
    body_mut(&mut timeout_mismatch)["allow_timeouts"] = json!(false);
    assert_error(
        &timeout_mismatch,
        "/allow_timeouts",
        "preset_timeout_mismatch",
    );

    let mut strict = valid_event("policy.set");
    body_mut(&mut strict)["preset"] = json!("strict");
    body_mut(&mut strict)["cells"] = json!({
        "easy_local": "ask",
        "easy_wide": "ask",
        "costly_local": "ask",
        "costly_wide": "ask",
        "irreversible_local": "ask",
        "irreversible_wide": "ask",
    });
    body_mut(&mut strict)["allow_timeouts"] = json!(false);
    expect_ok(&strict);

    let mut strict_timeout = valid_event("policy.set");
    body_mut(&mut strict_timeout)["preset"] = json!("strict");
    body_mut(&mut strict_timeout)["cells"] = json!({
        "easy_local": "ask",
        "easy_wide": "ask",
        "costly_local": "ask",
        "costly_wide": "ask",
        "irreversible_local": "ask",
        "irreversible_wide": "ask",
    });
    assert_error(
        &strict_timeout,
        "/allow_timeouts",
        "preset_timeout_mismatch",
    );

    let mut autopilot = valid_event("policy.set");
    body_mut(&mut autopilot)["preset"] = json!("autopilot");
    body_mut(&mut autopilot)["cells"] = json!({
        "easy_local": "auto",
        "easy_wide": "auto",
        "costly_local": "assume",
        "costly_wide": "assume",
        "irreversible_local": "ask",
        "irreversible_wide": "ask",
    });
    expect_ok(&autopilot);

    let mut custom = valid_event("policy.set");
    body_mut(&mut custom)["preset"] = json!("custom");
    body_mut(&mut custom)["cells"] = json!({
        "easy_local": "auto",
        "easy_wide": "assume",
        "costly_local": "assume",
        "costly_wide": "assume",
        "irreversible_local": "ask",
        "irreversible_wide": "ask",
    });
    body_mut(&mut custom)["allow_timeouts"] = json!(false);
    expect_ok(&custom);

    let mut locked = valid_event("policy.set");
    body_mut(&mut locked)["preset"] = json!("custom");
    body_mut(&mut locked)["cells"]["irreversible_local"] = json!("auto");
    assert_error(
        &locked,
        "/cells/irreversible_local",
        "locked_irreversible_cell",
    );

    for (field, value) in [
        ("max_speculative_features", 0),
        ("max_speculative_features", 1001),
        ("max_pending_slices", 0),
        ("max_pending_slices", 101),
        ("max_demo_gap_s", 59),
        ("max_demo_gap_s", 604_801),
    ] {
        let mut budget = valid_event("policy.set");
        body_mut(&mut budget)["budget"][field] = json!(value);
        assert_error(&budget, &format!("/budget/{field}"), "budget_bounds");
    }

    let mut domain = valid_event("policy.set");
    body_mut(&mut domain)["domain_floor"] = json!({"unknown": "ask"});
    assert_error_code(&domain, "E_VALIDATION");

    let mut revision = valid_event("policy.set");
    body_mut(&mut revision).insert("revision".to_owned(), json!(DIGEST));
    assert_error_code(&revision, "E_VALIDATION");

    let mut unknown_preset = valid_event("policy.set");
    body_mut(&mut unknown_preset)["preset"] = json!("chaos");
    assert_error_code(&unknown_preset, "E_VALIDATION");

    let mut missing = valid_event("policy.set");
    body_mut(&mut missing).remove("budget");
    assert_error_code(&missing, "E_VALIDATION");
}

#[test]
fn domain_map_keys_follow_canonical_string_order() {
    let mut value = valid_event("policy.set");
    body_mut(&mut value)["domain_floor"] = json!({"ux": "ask", "arch": "assume", "code": "auto"});
    let event = expect_ok(&value);
    let EventPayload::PolicySet(body) = &event.payload else {
        panic!("expected policy.set, got {:?}", event.payload);
    };
    let keys: Vec<Domain> = body.domain_floor.keys().copied().collect();
    assert_eq!(keys, vec![Domain::Arch, Domain::Code, Domain::Ux]);
    let serialized = serde_json::to_string(&body.domain_floor).expect("domain floor serializes");
    assert_eq!(serialized, r#"{"arch":"assume","code":"auto","ux":"ask"}"#);
    assert_round_trip(&event);
}

#[test]
fn policy_evaluation_effective_mode_and_text_are_validated() {
    let mut below = base_ask();
    body_mut(&mut below)["policy"]["requested"] = json!("ask");
    body_mut(&mut below)["policy"]["effective"] = json!("auto");
    assert_error(&below, "/policy/effective", "effective_below_requested");

    let mut escalated = base_ask();
    body_mut(&mut escalated)["policy"]["requested"] = json!("auto");
    body_mut(&mut escalated)["policy"]["effective"] = json!("ask");
    expect_ok(&escalated);

    let mut blank_hit = base_ask();
    body_mut(&mut blank_hit)["policy"]["heuristic_hits"] = json!([""]);
    assert_error(&blank_hit, "/policy/heuristic_hits/0", "blank_string");

    let mut blank_reason = base_ask();
    body_mut(&mut blank_reason)["policy"]["budget"]["reasons"] = json!([""]);
    assert_error(&blank_reason, "/policy/budget/reasons/0", "blank_string");
}

#[test]
fn policy_event_and_observation_body_references_require_causes() {
    let mut policy = base_ask();
    body_mut(&mut policy)["policy"]
        .as_object_mut()
        .expect("policy")
        .insert("policy_event".to_owned(), json!(REF_ID));
    assert_error(
        &policy,
        "/policy/policy_event",
        "body_reference_missing_from_causes",
    );

    let mut policy_with_cause = base_ask();
    body_mut(&mut policy_with_cause)["policy"]
        .as_object_mut()
        .expect("policy")
        .insert("policy_event".to_owned(), json!(REF_ID));
    refs_mut(&mut policy_with_cause).insert("causes".to_owned(), json!([REF_ID]));
    expect_ok(&policy_with_cause);

    let mut self_reference = base_ask();
    let own_id = self_reference["id"]
        .as_str()
        .expect("base ask id")
        .to_owned();
    body_mut(&mut self_reference)["card"]["already_checked"] = json!([{
        "text": "Output matched.",
        "claim": "seen",
        "observation": own_id,
    }]);
    refs_mut(&mut self_reference).insert("causes".to_owned(), json!([own_id]));
    assert_error(&self_reference, "/refs/causes/0", "self_reference");
}

// ---------------------------------------------------------------------------
// policy/rule wrapper isolation and literal hashes
// ---------------------------------------------------------------------------

const POLICY_REVISION_ABSENT: &str =
    "cc72a32077417c9f554f0a3b5a305c33331c767c36c79c9e2bafe1c038ab4cb5";
const RULE_REVISION_ABSENT: &str =
    "959142f0a544a7c44ae841ff48e39896e98d7862097507e2d2d2f0e3640248f6";
const RULE_REVISION_SOURCE: &str =
    "c997000bad7281edd53502f7208cd08844cda663794b9849b8275862ddd2a8e9";

#[test]
fn revision_literal_hashes_match_native_boundary() {
    let project = ProjectSlug::parse("example").expect("project");
    assert_eq!(
        policy_revision(&project, None).as_str(),
        POLICY_REVISION_ABSENT
    );

    let rule_id = RuleId::parse("no-shell").expect("rule id");
    assert_eq!(
        rule_revision(&project, &rule_id, None).as_str(),
        RULE_REVISION_ABSENT
    );

    let source = EventId::parse("00000000000000000000000001").expect("source id");
    assert_eq!(
        rule_revision(&project, &rule_id, Some(&source)).as_str(),
        RULE_REVISION_SOURCE
    );
}

#[test]
fn policy_and_rule_bodies_round_trip_without_revision() {
    let raw_policy: PolicySet = serde_json::from_value(policy_set()).expect("policy body decodes");
    let policy_value = serde_json::to_value(&raw_policy).expect("policy body serializes");
    assert!(policy_value.get("revision").is_none());
    assert_eq!(policy_value, policy_set());

    let rule_body = json!({
        "rule_id": "no-shell",
        "text": "Use explicit argv.",
        "enabled": true,
    });
    let raw_rule: RuleSet = serde_json::from_value(rule_body.clone()).expect("rule body decodes");
    let rule_value = serde_json::to_value(&raw_rule).expect("rule body serializes");
    assert!(rule_value.get("revision").is_none());
    assert_eq!(rule_value, rule_body);
}

#[test]
fn projection_wrappers_round_trip_and_are_not_event_bodies() {
    let raw_policy: PolicySet = serde_json::from_value(policy_set()).expect("policy body decodes");
    let policy_state = PolicyState {
        value: raw_policy,
        revision: Sha256::parse(POLICY_REVISION_ABSENT).expect("revision"),
    };
    let policy_json = serde_json::to_value(&policy_state).expect("wrapper serializes");
    assert!(policy_json["value"].get("revision").is_none());
    assert_eq!(policy_json["revision"], json!(POLICY_REVISION_ABSENT));
    let decoded: PolicyState =
        serde_json::from_value(policy_json.clone()).expect("wrapper decodes");
    assert_eq!(decoded, policy_state);

    let raw_rule: RuleSet = serde_json::from_value(json!({
        "rule_id": "no-shell",
        "text": "Use explicit argv.",
        "enabled": true,
    }))
    .expect("rule body decodes");
    let rule_state = RuleState {
        value: raw_rule,
        revision: Sha256::parse(RULE_REVISION_SOURCE).expect("revision"),
    };
    let rule_json = serde_json::to_value(&rule_state).expect("wrapper serializes");
    assert!(rule_json["value"].get("revision").is_none());
    assert_eq!(rule_json["revision"], json!(RULE_REVISION_SOURCE));
    let decoded: RuleState = serde_json::from_value(rule_json).expect("wrapper decodes");
    assert_eq!(decoded, rule_state);

    let policy_wrapper = event(
        "policy.set",
        json!({"value": policy_set(), "revision": POLICY_REVISION_ABSENT}),
        json!({}),
        OPERATOR,
    );
    assert_error(&policy_wrapper, "/body", "projection_wrapper_as_body");

    let rule_wrapper = event(
        "rule.set",
        json!({
            "value": {
                "rule_id": "no-shell",
                "text": "Use explicit argv.",
                "enabled": true,
            },
            "revision": RULE_REVISION_SOURCE,
        }),
        json!({}),
        OPERATOR,
    );
    assert_error(&rule_wrapper, "/body", "projection_wrapper_as_body");
}

#[test]
fn revision_is_not_a_raw_policy_or_rule_body_field() {
    let mut policy = policy_set();
    policy["revision"] = json!(POLICY_REVISION_ABSENT);
    let value = event("policy.set", policy, json!({}), OPERATOR);
    assert_error_code(&value, "E_VALIDATION");

    let mut rule = json!({
        "rule_id": "no-shell",
        "text": "Use explicit argv.",
        "enabled": true,
    });
    rule["revision"] = json!(RULE_REVISION_SOURCE);
    let value = event("rule.set", rule, json!({}), OPERATOR);
    assert_error_code(&value, "E_VALIDATION");
}

// ---------------------------------------------------------------------------
// Remaining body shapes
// ---------------------------------------------------------------------------

#[test]
fn empty_patches_and_glyph_vocabulary_are_validated() {
    let empty_declaration = event("project.declared", json!({}), json!({}), OPERATOR);
    assert_error(&empty_declaration, "/body", "empty_patch");

    let agent_only_declaration = event(
        "project.declared",
        json!({"agent": agent()}),
        json!({}),
        AGENT,
    );
    assert_error(&agent_only_declaration, "/body", "empty_patch");

    let mut glyph = valid_event("project.declared");
    body_mut(&mut glyph).insert("glyph".to_owned(), json!("radar"));
    expect_ok(&glyph);
    let mut glyph = valid_event("project.declared");
    body_mut(&mut glyph).insert("glyph".to_owned(), json!("star"));
    assert_error(&glyph, "/glyph", "unknown_glyph");

    let empty_feature = event("plan.feature", json!({}), json!({"node": "x"}), OPERATOR);
    assert_error(&empty_feature, "/body", "empty_patch");

    let agent_only_feature = event(
        "plan.feature",
        json!({"agent": agent()}),
        json!({"node": "x"}),
        AGENT,
    );
    assert_error(&agent_only_feature, "/body", "empty_patch");
}

#[test]
fn plan_feature_dependencies_priority_and_demo_kind_are_validated() {
    let depends_on: Vec<String> = (0..64).map(|index| format!("f{index}")).collect();
    let mut valid = valid_event("plan.feature");
    body_mut(&mut valid).insert("depends_on".to_owned(), json!(depends_on));
    expect_ok(&valid);

    let too_many: Vec<String> = (0..65).map(|index| format!("f{index}")).collect();
    let mut too_many_value = valid_event("plan.feature");
    body_mut(&mut too_many_value).insert("depends_on".to_owned(), json!(too_many));
    assert_error(&too_many_value, "/depends_on", "depends_on_limit");

    let mut duplicate = valid_event("plan.feature");
    body_mut(&mut duplicate).insert("depends_on".to_owned(), json!(["x", "x"]));
    assert_error(&duplicate, "/depends_on/1", "duplicate_node");

    for priority in [0, 3] {
        let mut value = valid_event("plan.feature");
        body_mut(&mut value).insert("priority".to_owned(), json!(priority));
        expect_ok(&value);
    }
    let mut priority = valid_event("plan.feature");
    body_mut(&mut priority).insert("priority".to_owned(), json!(4));
    assert_error(&priority, "/priority", "priority_range");

    let mut demo_kind = valid_event("plan.feature");
    body_mut(&mut demo_kind).insert("demo_kind".to_owned(), json!("screenshot"));
    expect_ok(&demo_kind);

    let mut demo_kind = valid_event("plan.feature");
    body_mut(&mut demo_kind).insert("demo_kind".to_owned(), json!("video"));
    assert_error_code(&demo_kind, "E_VALIDATION");
}

#[test]
fn decision_answer_and_ack_shapes_are_validated() {
    let mut no_criteria = valid_event("decision.answer");
    body_mut(&mut no_criteria)["done_when"] = json!([]);
    assert_error(&no_criteria, "/done_when", "done_when_count");

    let mut four_criteria = valid_event("decision.answer");
    body_mut(&mut four_criteria)["done_when"] = json!([
        {"id": "a", "text": "A"},
        {"id": "b", "text": "B"},
        {"id": "c", "text": "C"},
        {"id": "d", "text": "D"},
    ]);
    assert_error(&four_criteria, "/done_when", "done_when_count");

    let mut duplicate = valid_event("decision.answer");
    body_mut(&mut duplicate)["done_when"] = json!([
        {"id": "done", "text": "A"},
        {"id": "done", "text": "B"},
    ]);
    assert_error(&duplicate, "/done_when/1/id", "duplicate_criterion");

    let mut eta = valid_event("decision.answer");
    body_mut(&mut eta).insert("expected_eta".to_owned(), json!({"min_s": 20, "max_s": 10}));
    assert_error(&eta, "/expected_eta", "eta_order");

    let mut with_agent = valid_event("decision.answer");
    body_mut(&mut with_agent).insert("agent".to_owned(), agent());
    assert_error_code(&with_agent, "E_VALIDATION");

    let restarted: Vec<String> = (0..128).map(|index| format!("f{index}")).collect();
    let mut ack = valid_event("decision.ack");
    body_mut(&mut ack)["restarted"] = json!(restarted);
    expect_ok(&ack);

    let too_many: Vec<String> = (0..129).map(|index| format!("f{index}")).collect();
    let mut ack = valid_event("decision.ack");
    body_mut(&mut ack)["restarted"] = json!(too_many);
    assert_error(&ack, "/restarted", "restarted_limit");

    let mut ack = valid_event("decision.ack");
    body_mut(&mut ack)["restarted"] = json!(["x", "x"]);
    assert_error(&ack, "/restarted/1", "duplicate_node");

    let mut ack = valid_event("decision.ack");
    body_mut(&mut ack)["applied_option"] = json!("Bad");
    assert_error_code(&ack, "E_VALIDATION");
}

#[test]
fn session_lost_and_delivered_shapes_are_validated() {
    let mut pid_reused = valid_event("session.lost");
    body_mut(&mut pid_reused)["reason"] = json!("pid_reused");
    expect_ok(&pid_reused);

    let mut exit_code = valid_event("session.lost");
    body_mut(&mut exit_code).insert("exit_code".to_owned(), json!(0));
    expect_ok(&exit_code);

    let mut node = valid_event("session.lost");
    insert_ref(&mut node, "node", "x");
    expect_ok(&node);

    let mut bad_reason = valid_event("session.lost");
    body_mut(&mut bad_reason)["reason"] = json!("unknown");
    assert_error_code(&bad_reason, "E_VALIDATION");

    let mut bad_session = valid_event("session.lost");
    body_mut(&mut bad_session)["session"] = json!("0000000000000000000000010a");
    assert_error_code(&bad_session, "E_VALIDATION");

    let mut no_note = valid_event("plan.dropped");
    body_mut(&mut no_note).remove("note");
    expect_ok(&no_note);

    let mut long_note = valid_event("plan.dropped");
    body_mut(&mut long_note)["note"] = json!("n".repeat(400));
    assert_error_code(&long_note, "E_VALIDATION");

    let mut by = valid_event("plan.dropped");
    body_mut(&mut by).insert("agent".to_owned(), agent());
    assert_error_code(&by, "E_VALIDATION");

    // A snooze: a row id, the row's revision and a time at most seven days
    // after the event's own. A time at or before it is valid (it ends one).
    let seven_days = valid_event("need.snoozed");
    let ts = seven_days["ts"].as_str().expect("ts").to_owned();
    let after = |ms: i64| {
        agendar_core::model::UtcTime::parse(&ts)
            .expect("ts")
            .add_millis(ms)
            .canonical()
    };
    const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
    for (ms, valid) in [
        (WEEK_MS, true),
        (WEEK_MS + 1, false),
        (0, true),
        (-WEEK_MS * 8, true),
    ] {
        let mut snooze = valid_event("need.snoozed");
        body_mut(&mut snooze)["until"] = json!(after(ms));
        if valid {
            expect_ok(&snooze);
        } else {
            assert_eq!(expect_err(&snooze).detail(), Some("snooze_too_long"));
        }
    }
    for need in ["", " decide:x", "decide:x\u{0007}"] {
        let mut blank = valid_event("need.snoozed");
        body_mut(&mut blank)["need"] = json!(need);
        assert_error_code(&blank, "E_VALIDATION");
    }
    let mut long_need = valid_event("need.snoozed");
    body_mut(&mut long_need)["need"] = json!("n".repeat(201));
    assert_error_code(&long_need, "E_VALIDATION");
    for field in ["need", "revision", "until"] {
        let mut missing = valid_event("need.snoozed");
        body_mut(&mut missing).remove(field);
        assert_error_code(&missing, "E_VALIDATION");
    }
    let mut extra = valid_event("need.snoozed");
    body_mut(&mut extra).insert("note".to_owned(), json!("later"));
    assert_error_code(&extra, "E_VALIDATION");
    let mut on_a_step = valid_event("need.snoozed");
    insert_ref(&mut on_a_step, "node", "x");
    assert_error_code(&on_a_step, "E_VALIDATION");

    let mut itself = valid_event("session.continued");
    body_mut(&mut itself)["previous"] = body_mut(&mut itself)["session"].clone();
    assert_error_code(&itself, "E_VALIDATION");

    let mut extra = valid_event("session.continued");
    body_mut(&mut extra).insert("harness".to_owned(), json!("claude_code"));
    assert_error_code(&extra, "E_VALIDATION");

    let mut bad_transport = valid_event("delivered.answer");
    body_mut(&mut bad_transport)["transport"] = json!("carrier-pigeon");
    assert_error_code(&bad_transport, "E_VALIDATION");

    let mut bad_outcome = valid_event("delivered.answer");
    body_mut(&mut bad_outcome)["outcome"] = json!("maybe");
    assert_error_code(&bad_outcome, "E_VALIDATION");

    let mut bad_packet = valid_event("delivered.answer");
    body_mut(&mut bad_packet)["packet_id"] = json!("A".repeat(64));
    assert_error_code(&bad_packet, "E_VALIDATION");
}

#[test]
fn choice_policy_event_requires_causes_and_auto_cannot_timeout() {
    let mut timeout = valid_event("decision.auto");
    body_mut(&mut timeout)["reason"] = json!("timeout");
    assert_error(&timeout, "/reason", "auto_cannot_timeout");

    let mut assumed_timeout = valid_event("decision.assumed");
    body_mut(&mut assumed_timeout)["reason"] = json!("timeout");
    expect_ok(&assumed_timeout);

    let mut policy_event = valid_event("decision.assumed");
    body_mut(&mut policy_event)["policy"]
        .as_object_mut()
        .expect("policy")
        .insert("policy_event".to_owned(), json!(REF_ID));
    assert_error(
        &policy_event,
        "/policy/policy_event",
        "body_reference_missing_from_causes",
    );

    let mut with_cause = valid_event("decision.assumed");
    body_mut(&mut with_cause)["policy"]
        .as_object_mut()
        .expect("policy")
        .insert("policy_event".to_owned(), json!(REF_ID));
    refs_mut(&mut with_cause).insert("causes".to_owned(), json!([REF_ID]));
    expect_ok(&with_cause);
}

#[test]
fn exact_line_size_boundary_is_enforced() {
    let mut value = valid_event("evidence.observed");
    body_mut(&mut value).insert("approval_id".to_owned(), json!(""));
    let base = line(&value).len();
    let padding = 65_536 - base;
    body_mut(&mut value).insert("approval_id".to_owned(), json!("a".repeat(padding)));
    assert_eq!(line(&value).len(), 65_536);
    expect_ok(&value);

    body_mut(&mut value).insert("approval_id".to_owned(), json!("a".repeat(padding + 1)));
    let error = expect_err(&value);
    assert_eq!(error.detail(), Some("line_size"));
}

#[test]
fn utc_time_year_boundary_is_enforced() {
    let mut minimum = valid_event("project.declared");
    minimum["ts"] = json!("1970-01-01T00:00:00.000Z");
    expect_ok(&minimum);

    let mut maximum = valid_event("project.declared");
    maximum["ts"] = json!("9999-12-31T23:59:59.999Z");
    expect_ok(&maximum);

    let mut too_early = valid_event("project.declared");
    too_early["ts"] = json!("1969-12-31T23:59:59.999Z");
    assert_error_code(&too_early, "E_VALIDATION");
}

#[test]
fn identifier_newtypes_enforce_canonical_forms() {
    assert!(VaultId::parse("00000000000000000000000001").is_ok());
    assert!(VaultId::parse("0000000000000000000000000a").is_err());
    assert!(HostId::parse("h-00000000000000000000000001").is_ok());
    assert!(HostId::parse("h-0000000000000000000000000A").is_err());
    assert!(SessionId::parse("00000000000000000000000101").is_ok());
    assert!(SessionId::parse("0000000000000000000000010i").is_err());
    assert!(WriterId::parse(AGENT).is_ok());
    assert!(WriterId::parse("h-00000000000000000000000001/admin").is_err());
}

#[test]
fn canonical_json_sorts_keys_and_omits_whitespace() {
    let value = json!({"b": 1, "a": [true, "x"]});
    assert_eq!(canonical_json(&value), r#"{"a":[true,"x"],"b":1}"#);
}

#[test]
fn error_builder_carries_code_field_detail_hint_and_retryability() {
    let error = AgendarError::validation_at("/body/x", "some_detail", "Message.")
        .with_hint("Try again.")
        .with_retryable(true);
    assert_eq!(error.code().as_str(), "E_VALIDATION");
    assert_eq!(error.message(), "Message.");
    assert_eq!(error.to_string(), "Message.");
    assert_eq!(error.field(), Some("/body/x"));
    assert_eq!(error.detail(), Some("some_detail"));
    assert_eq!(error.hint(), Some("Try again."));
    assert!(error.retryable());

    let plain = AgendarError::validation("Plain.");
    assert_eq!(plain.field(), None);
    assert_eq!(plain.detail(), None);
    assert_eq!(plain.hint(), None);
    assert!(!plain.retryable());
}

#[test]
fn text_a_little_over_its_limit_is_accepted_and_far_over_explains_why() {
    use agendar_core::model::validate::{grace, limits};
    let mut near = valid_event("project.declared");
    set_project_title(&mut near, &"a".repeat(grace(limits::PROJECT_TITLE)));
    expect_ok(&near);
    let mut far = valid_event("project.declared");
    set_project_title(&mut far, &"a".repeat(grace(limits::PROJECT_TITLE) + 10));
    let error = expect_err(&far);
    assert!(
        error.message().contains("keep it to 100"),
        "{}",
        error.message()
    );
    assert!(
        error
            .hint()
            .unwrap_or_default()
            .contains("Shorten /title by 10"),
        "{:?}",
        error.hint()
    );
}

fn diff_demo() -> Value {
    json!({
        "kind": "diff",
        "body": {
            "blob": blob(),
            "commit": "233cf80".to_owned() + &"0".repeat(33),
            "files_total": 3,
            "hunks_total": 5,
            "added_total": 40,
            "removed_total": 12,
            "shown": [{"path": "crates/x/src/a.rs", "hunk": 2, "why": "The receipt becomes optional."}],
            "omitted": [{"path": "README.md", "added": 1, "removed": 0}],
        },
    })
}

#[test]
fn a_diff_demo_round_trips_and_is_validated() {
    let event = expect_ok(&delivery_with_demo(diff_demo()));
    assert_round_trip(&event);

    for bad in ["233cf80", &"A".repeat(40), &"g".repeat(40)] {
        let mut demo = diff_demo();
        demo["body"]["commit"] = json!(bad);
        assert_error(
            &delivery_with_demo(demo),
            "/demos/0/body/commit",
            "diff_commit",
        );
    }

    let mut none = diff_demo();
    none["body"]["shown"] = json!([]);
    assert_error(
        &delivery_with_demo(none),
        "/demos/0/body/shown",
        "diff_shown_count",
    );

    let mut thirteen = diff_demo();
    let one = thirteen["body"]["shown"][0].clone();
    thirteen["body"]["shown"] = json!(vec![one; 13]);
    assert_error(
        &delivery_with_demo(thirteen),
        "/demos/0/body/shown",
        "diff_shown_count",
    );

    let mut blank_why = diff_demo();
    blank_why["body"]["shown"][0]["why"] = json!("  ");
    assert_error(
        &delivery_with_demo(blank_why),
        "/demos/0/body/shown/0/why",
        "blank_string",
    );

    let mut markdown = diff_demo();
    markdown["body"]["blob"] = blob_with_ext("md");
    assert_error(
        &delivery_with_demo(markdown),
        "/demos/0/body/blob/ext",
        "demo_extension_mismatch",
    );

    let mut extra = diff_demo();
    extra["body"]["repo"] = json!(".");
    assert_error_code(&delivery_with_demo(extra), "E_VALIDATION");
}

#[test]
fn delivery_replies_need_supersedes_and_one_short_reply_per_target() {
    let with_replies = |replies: Value, supersedes: bool| {
        let mut value = valid_event("delivery");
        body_mut(&mut value).insert("replies".to_owned(), replies);
        if supersedes {
            insert_ref(&mut value, "supersedes", "00000000000000000000000001");
        }
        value
    };
    // A delivery without replies keeps its exact bytes: the field is skipped.
    let plain = expect_ok(&valid_event("delivery"));
    let bytes = serde_json::to_value(&plain).expect("json");
    assert!(bytes["body"].get("replies").is_none(), "{bytes}");

    let good = json!([{"text": "Cached it"}, {"comment": 2, "text": "Renamed"}]);
    let event = expect_ok(&with_replies(good.clone(), true));
    let bytes = serde_json::to_value(&event).expect("json");
    assert_eq!(bytes["body"]["replies"], good);

    assert_error(
        &with_replies(good, false),
        "/replies",
        "delivery_replies_without_supersedes",
    );
    assert_error(
        &with_replies(
            json!([{"comment": 1, "text": "a"}, {"comment": 1, "text": "b"}]),
            true,
        ),
        "/replies/1",
        "duplicate_reply",
    );
    assert_error(
        &with_replies(json!([{"text": "a"}, {"text": "b"}]), true),
        "/replies/1",
        "duplicate_reply",
    );
    assert_error(
        &with_replies(json!([{"comment": 0, "text": "a"}]), true),
        "/replies/0/comment",
        "reply_comment_out_of_range",
    );
    assert_error(
        &with_replies(json!([{"text": "  "}]), true),
        "/replies/0/text",
        "blank_string",
    );
    assert_error(
        &with_replies(json!([{"text": "x".repeat(400)}]), true),
        "/replies/0/text",
        "length",
    );
    let many: Vec<Value> = (1..=22)
        .map(|n| json!({"comment": n, "text": "ok"}))
        .collect();
    assert_error(
        &with_replies(json!(many), true),
        "/replies",
        "too_many_replies",
    );
    assert_error_code(
        &with_replies(json!([{"comment": 1, "text": "a", "extra": true}]), true),
        "E_VALIDATION",
    );
}
