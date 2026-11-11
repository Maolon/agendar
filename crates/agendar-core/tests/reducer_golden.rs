//! Reducer golden fixtures and approved-seed adapters.
//!
//! The seed packages under `tests/oracle/seeds/` are independent, reviewed
//! oracles. This file adapts only the seeds
//! whose projections exist and asserts every authored field exactly;
//! it never rewrites an oracle or blesses reducer output.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use agendar_core::model::{
    Diagnostic, Event, EventId, EventPayload, Gate, NodePath, ProjectState, State, UtcTime,
    canonical_json,
};
use agendar_core::reduce::attention::{notification_eligible, runnable_nodes};
use agendar_core::reduce::{CausalView, ReduceContext, reduce, resolve};
use serde_json::{Value, json};

fn reduce_seed(seed: &support::Seed) -> State {
    reduce(&seed.events, &ReduceContext { as_of: seed.as_of })
}

fn seed_project<'a>(state: &'a State, seed: &support::Seed) -> &'a ProjectState {
    state
        .projects
        .get(&agendar_core::model::ProjectSlug::parse("example").expect("slug"))
        .unwrap_or_else(|| panic!("seed {} must project project `example`", seed.name))
}

fn id(value: &str) -> EventId {
    EventId::parse(value).expect("canonical ULID")
}

fn assert_seed_bytes_unchanged(seed: &support::Seed, files: &[(&str, &str)]) {
    for (name, digest) in files {
        let path = seed.dir.join(name);
        assert_eq!(
            &support::file_digest(&path),
            digest,
            "approved seed file {}/{} must stay byte-identical",
            seed.name,
            name
        );
    }
}

#[test]
fn seed_answer_conflict_matches_approved_assertions() {
    let seed = support::load_seed("answer-conflict");
    assert_seed_bytes_unchanged(
        &seed,
        &[
            (
                "events.jsonl",
                "6192a5a776dc974330835f7df8def35020497c44451188c06f66aedc53fb81e6",
            ),
            (
                "expected.json",
                "e97500cd6620389ccf267573cf547b4b3bb2223035d22b90d22b931cf7ca5cea",
            ),
        ],
    );
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let ask = id(seed.alias("ask"));
    let decision = project
        .decisions
        .get(&ask)
        .expect("the ask must project a decision");
    let assertions = &seed.assertions()["decision"];
    assert_eq!(
        decision.effective_receipt.as_ref().map(EventId::as_str),
        assertions["effective_receipt"].as_str()
    );
    assert_eq!(decision.applied_answer.as_ref().map(EventId::as_str), None);
    assert_eq!(decision.lifecycle.as_str(), "answered");
    let history: Vec<&str> = decision
        .answer_history
        .iter()
        .map(EventId::as_str)
        .collect();
    assert_eq!(
        history,
        vec![seed.alias("a"), seed.alias("b")],
        "both answers stay in `O` history"
    );
    assert!(!decision.pending_pivot);
    assert_eq!(seed.assertions()["pending_pivot"], json!(false));
    // Receipt snapshots come from the ask card, not from a later plan.
    let receipt = project
        .receipts
        .get(
            &decision
                .effective_receipt
                .clone()
                .expect("effective receipt"),
        )
        .expect("receipt exists");
    assert_eq!(receipt.chose.label, "Use inbox on next start");
    assert_eq!(receipt.ruled_out.len(), 1);
    assert_eq!(receipt.ruled_out[0].id.as_str(), "adapters");
    assert!(receipt.expected_finish.is_some());
}

#[test]
fn seed_plan_cycle_matches_approved_assertions() {
    let seed = support::load_seed("plan-cycle");
    assert_seed_bytes_unchanged(
        &seed,
        &[
            (
                "events.jsonl",
                "4b4fe6462ef2700c561557e64050e9d6856e2196bbac375276f82cff91aa5382",
            ),
            (
                "expected.json",
                "0c47097cbfb1a639f1754e2c4bad99fbfa1ef48c4b8ea42b2bec0b2e880a42aa",
            ),
        ],
    );
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let accepted: Vec<(String, String)> = project
        .features
        .values()
        .flat_map(|feature| {
            feature.accepted_depends_on.iter().map(|upstream| {
                (
                    upstream.as_str().to_owned(),
                    feature.node.as_str().to_owned(),
                )
            })
        })
        .collect();
    assert_eq!(
        accepted,
        vec![("x".to_owned(), "y".to_owned())],
        "the accepted edge is the one whose patch came first in O"
    );
    let mut rescue_ids: Vec<String> = Vec::new();
    let mut rejected: Vec<(String, String)> = Vec::new();
    for row in &project.needs_you {
        if row.id.starts_with("rescue:cycle:") {
            rescue_ids.push(row.id.clone());
            let parts: Vec<&str> = row.id.split(':').collect();
            rejected.push((parts[3].to_owned(), parts[4].to_owned()));
        }
    }
    assert_eq!(
        rescue_ids,
        vec!["rescue:cycle:00000000000000000000000004:y:x".to_owned()]
    );
    assert_eq!(rejected, vec![("y".to_owned(), "x".to_owned())]);
    let row = &project.needs_you[0];
    assert_eq!(row.source.as_str(), seed.alias("closing-patch"));
    assert_eq!(row.question, "Remove y → x to repair the plan?");
}

#[test]
fn seed_transitive_taint_matches_approved_assertions() {
    let seed = support::load_seed("transitive-taint");
    assert_seed_bytes_unchanged(
        &seed,
        &[
            (
                "events.jsonl",
                "50b4904cc889d8c009e2a8dcea2d2cc4f9a452a385c0055583baf93d398c449e",
            ),
            (
                "expected.json",
                "ddad61da998bfe5fe9a1227d089aa87995491a494a490cf55e8b961254ff20fe",
            ),
        ],
    );
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let assertions = seed.assertions();
    let ask = id(seed.alias("ask"));

    // Taint is independent of progress: the override never restarts, verifies
    // or reopens anything.
    for (node_name, want_progress) in [("x", "done"), ("y", "doing"), ("z", "doing")] {
        let node = NodePath::parse(node_name).expect("node");
        let feature = &project.features[&node];
        assert_eq!(feature.progress.as_str(), want_progress);
        assert_eq!(
            feature.basis.as_str(),
            assertions["basis"][node_name].as_str().expect("basis")
        );
        let taints: Vec<&str> = feature.taints.iter().map(EventId::as_str).collect();
        let want: Vec<&str> = assertions["taints"][node_name]
            .as_array()
            .expect("taints")
            .iter()
            .map(|value| value.as_str().expect("taint id"))
            .collect();
        assert_eq!(taints, want, "taints.{node_name}");
    }

    // `built_on` counts every current consumer, including the finished target.
    let decision = &project.decisions[&ask];
    let built_on: Vec<&str> = decision.built_on.iter().map(NodePath::as_str).collect();
    let want: Vec<&str> = assertions["built_on"]
        .as_array()
        .expect("built_on")
        .iter()
        .map(|value| value.as_str().expect("node"))
        .collect();
    assert_eq!(built_on, want);

    // The pivot redo set is the sorted tainted feature list.
    let mut redo: Vec<&str> = project
        .features
        .values()
        .filter(|feature| !feature.taints.is_empty())
        .map(|feature| feature.node.as_str())
        .collect();
    redo.sort_unstable();
    let want: Vec<&str> = assertions["redo"]
        .as_array()
        .expect("redo")
        .iter()
        .map(|value| value.as_str().expect("node"))
        .collect();
    assert_eq!(redo, want);

    // A tainted feature is not firm work for the runnable predicate.
    let session = agendar_core::model::SessionId::parse("00000000000000000000000101").expect("id");
    assert!(!agendar_core::reduce::attention::runnable_for(
        &session,
        &NodePath::parse("x").expect("node"),
        project
    ));
}

#[test]
fn seed_ranking_two_asks_matches_approved_assertions() {
    let seed = support::load_seed("ranking-two-asks");
    assert_seed_bytes_unchanged(
        &seed,
        &[
            (
                "events.jsonl",
                "2d94729da8773b13e404457605e596c8c017b885016b030739006a351e18861e",
            ),
            (
                "expected.json",
                "eef6203163ff67ffd1d30cd2cc27858371b1db0ef9f25002fea83e3081422caa",
            ),
        ],
    );
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let assertions = seed.assertions();
    let expected = assertions["needs"].as_array().expect("seed needs");
    assert_eq!(project.needs_you.len(), expected.len());
    for (row, want) in project.needs_you.iter().zip(expected) {
        assert_eq!(row.id, want["id"].as_str().expect("need id"));
        assert_eq!(row.score_x2, want["score_x2"].as_u64().expect("score"));
        assert_eq!(
            row.stalled_agents,
            want["stalled_agents"].as_u64().expect("stalled") as u32
        );
        assert_eq!(
            row.waiting_minutes,
            want["waiting_minutes"].as_u64().expect("minutes")
        );
        assert_eq!(
            row.weight_x2,
            want["weight_x2"].as_u64().expect("weight") as u8
        );
        assert_eq!(
            notification_eligible(row),
            want["notification_eligible"].as_bool().expect("eligible")
        );
        assert_eq!(
            row.reason,
            format!("{} agent waiting · 10m", row.stalled_agents)
        );
        assert_eq!(row.unblocks, Vec::<NodePath>::new());
    }

    let session_a =
        agendar_core::model::SessionId::parse("00000000000000000000000101").expect("id");
    let session_b =
        agendar_core::model::SessionId::parse("00000000000000000000000102").expect("id");
    assert_eq!(runnable_nodes(&session_a, project), Vec::<NodePath>::new());
    assert_eq!(
        runnable_nodes(&session_b, project),
        vec![NodePath::parse("y").expect("node")]
    );
    assert_eq!(
        assertions["runnable_for_session"]["00000000000000000000000101"],
        json!([])
    );
    assert_eq!(
        assertions["runnable_for_session"]["00000000000000000000000102"],
        json!(["y"])
    );
    // Fleet dedupe: one session, two rows.
    let fleet = project
        .sessions
        .values()
        .filter(|session| !session.stalled_on.is_empty())
        .count();
    assert_eq!(
        fleet as u64,
        assertions["fleet_stalled"].as_u64().expect("fleet")
    );
}

/// Canonical state serialization used by the full-State goldens.
fn canonical_state(state: &State) -> String {
    canonical_json(&serde_json::to_value(state).expect("state serializes"))
}

#[test]
fn seed_answer_conflict_is_arrival_order_independent() {
    let seed = support::load_seed("answer-conflict");
    let expected = canonical_state(&reduce_seed(&seed));
    let permutations = [
        vec![0, 1, 2, 3, 4],
        vec![4, 3, 2, 1, 0],
        vec![2, 0, 4, 1, 3],
        vec![3, 4, 0, 2, 1],
    ];
    for order in permutations {
        let mut events: Vec<Event> = order
            .iter()
            .map(|index| seed.events[*index].clone())
            .collect();
        // Duplicate one variant to prove exact-duplicate collapse.
        events.push(events[0].clone());
        let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
        assert_eq!(canonical_state(&state), expected);
    }
}

#[test]
fn unresolved_reference_is_pending_until_target_arrives() {
    // A human answer whose ask is absent stays pending; the ask then resolves it.
    let seed = support::load_seed("answer-conflict");
    let answer = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAnswer(_)))
        .expect("seed has an answer")
        .clone();
    let resolution = resolve(std::slice::from_ref(&answer));
    assert_eq!(resolution.ordered.len(), 0);
    assert!(
        resolution
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("missing_reference") })
    );
    let ask_pending = reduce(
        std::slice::from_ref(&answer),
        &ReduceContext { as_of: seed.as_of },
    );
    // A project with only pending events is still visible.
    assert!(
        ask_pending
            .projects
            .contains_key(&agendar_core::model::ProjectSlug::parse("example").expect("slug"))
    );
}

#[test]
fn duplicate_id_with_different_payload_quarantines_every_variant() {
    let seed = support::load_seed("answer-conflict");
    let original = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAnswer(_)))
        .expect("seed has an answer")
        .clone();
    let mut mutated = original.clone();
    if let EventPayload::DecisionAnswer(body) = &mut mutated.payload {
        body.option = agendar_core::model::OptionId::parse("inbox").expect("option");
    }
    let resolution = resolve(&[original.clone(), mutated.clone()]);
    assert_eq!(resolution.ordered.len(), 0);
    for event in [&original, &mutated] {
        let digest = agendar_core::reduce::order::event_digest(event);
        assert_eq!(
            resolution.variants[&(event.id.clone(), digest)].status,
            agendar_core::reduce::VariantStatus::Quarantined
        );
    }
    assert!(
        resolution
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("duplicate_event_id"))
    );
}

#[test]
fn writer_sequence_fork_quarantines_both_and_holds_later_events_pending() {
    let seed = support::load_seed("answer-conflict");
    let answer = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAnswer(_)))
        .expect("seed has an answer")
        .clone();
    // A different ID at the same (writer, seq): both sides are quarantined.
    let mut second = answer.clone();
    second.id = id("00000000000000000000000077");
    let late = {
        let mut event = answer.clone();
        event.id = id("00000000000000000000000078");
        event.seq = answer.seq + 1;
        event
    };
    let resolution = resolve(&[answer.clone(), second, late]);
    let status = |event: &Event| {
        let digest = agendar_core::reduce::order::event_digest(event);
        resolution.variants[&(event.id.clone(), digest)].status
    };
    assert_eq!(
        status(&answer),
        agendar_core::reduce::VariantStatus::Quarantined
    );
    assert_eq!(resolution.ordered.len(), 0);
    assert!(
        resolution
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("writer_seq_fork"))
    );
}

#[test]
fn cyclic_reference_quarantines_the_component() {
    let seed = support::load_seed("answer-conflict");
    let ask = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::DecisionAsk(_)))
        .expect("seed has an ask")
        .clone();
    let mut first = ask.clone();
    first.id = id("00000000000000000000000091");
    first.refs.causes = vec![id("00000000000000000000000092")];
    let mut second = ask.clone();
    second.id = id("00000000000000000000000092");
    second.seq = 2;
    second.refs.causes = vec![id("00000000000000000000000091")];
    let resolution = resolve(&[first, second]);
    assert_eq!(resolution.ordered.len(), 0);
    assert!(
        resolution
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("cyclic_reference"))
    );
}

#[test]
fn sequence_gaps_are_diagnostics_not_blockers() {
    let seed = support::load_seed("answer-conflict");
    let declared = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::ProjectDeclared(_)))
        .expect("seed has a declaration")
        .clone();
    let plan = seed
        .events
        .iter()
        .find(|event| matches!(event.payload, EventPayload::PlanFeature(_)))
        .expect("seed has a feature")
        .clone();
    // The operator writer jumps from seq 1 to seq 10: a diagnostic, not a block.
    let mut after_gap = plan.clone();
    after_gap.seq = 10;
    let resolution = resolve(&[declared, after_gap]);
    assert_eq!(resolution.ordered.len(), 2, "a gap alone never blocks");
    assert!(
        resolution
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("sequence_gap"))
    );
}

#[test]
fn event_set_digest_is_arrival_order_and_duplicate_independent() {
    let seed = support::load_seed("answer-conflict");
    let base = resolve(&seed.events).event_set_digest;
    let mut events: Vec<Event> = seed.events.iter().rev().cloned().collect();
    events.push(seed.events[1].clone());
    assert_eq!(resolve(&events).event_set_digest, base);
    let mut changed: Vec<Event> = seed.events.clone();
    changed.pop();
    assert_ne!(resolve(&changed).event_set_digest, base);
}

#[test]
fn causal_view_distinguishes_order_from_reachability() {
    let seed = support::load_seed("answer-conflict");
    let resolution = resolve(&seed.events);
    let causal = CausalView {
        order_index: &resolution.order_index,
        ancestors: &resolution.ancestors,
    };
    let ask = id(seed.alias("ask"));
    let answer_a = id(seed.alias("a"));
    let answer_b = id(seed.alias("b"));
    // A node address is not a causal edge: the ask only references its node.
    assert!(!causal.before(&id(seed.alias("plan-x")), &ask));
    // The answer references its ask, so it is causally later.
    assert!(causal.before(&ask, &answer_a));
    assert!(!causal.before(&answer_a, &answer_b));
    assert!(!causal.before(&answer_b, &answer_a));
    assert!(causal.order_of(&answer_a) < causal.order_of(&answer_b));
}

#[test]
fn diagnostics_are_sorted_and_free_of_local_paths() {
    let seed = support::load_seed("plan-cycle");
    let state = reduce_seed(&seed);
    let mut sorted: Vec<&Diagnostic> = state.diagnostics.iter().collect();
    sorted.sort_by(|left, right| left.id.cmp(&right.id));
    let ids: Vec<&str> = state.diagnostics.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        ids,
        sorted.iter().map(|d| d.id.as_str()).collect::<Vec<_>>()
    );
    let json = canonical_json(&serde_json::to_value(&state).expect("state"));
    assert!(!json.contains("/Users/"));
}

#[test]
fn canonical_json_is_stable_for_projection_values() {
    let value = json!({"b": 1, "a": "x"});
    assert_eq!(canonical_json(&value), r#"{"a":"x","b":1}"#);
    let again =
        canonical_json(&serde_json::from_str::<Value>(&canonical_json(&value)).expect("json"));
    assert_eq!(again, canonical_json(&value));
}

#[test]
fn ask_ts_is_the_attention_since() {
    let seed = support::load_seed("ranking-two-asks");
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let expect = UtcTime::parse("2026-01-01T11:50:00.000Z").expect("timestamp");
    for row in &project.needs_you {
        assert_eq!(row.since, expect);
    }
    let _: BTreeSet<&str> = BTreeSet::new();
    let _: BTreeMap<&str, &str> = BTreeMap::new();
}

// ---------------------------------------------------------------------------
// Implementation fixtures (§11.1): complete events + reviewed full-State goldens.
// ---------------------------------------------------------------------------

#[test]
fn seed_delivery_seen_review_matches_approved_assertions() {
    let seed = support::load_seed("delivery-seen-review");
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let delivery_id = id(seed.alias("delivery"));
    let delivery = project
        .deliveries
        .get(&delivery_id)
        .expect("the delivery must project");
    let observed = &delivery.criteria[0];
    assert_eq!(
        observed.agent_result,
        agendar_core::model::CheckResult::Passed
    );
    assert_eq!(
        observed.seen_result,
        Some(agendar_core::model::CheckResult::Failed)
    );
    assert_eq!(
        observed.observation.as_ref().map(EventId::as_str),
        Some(seed.alias("observation-2"))
    );
    assert!(observed.conflict, "the failed Seen contradicts the claim");
    assert_eq!(delivery.review, agendar_core::model::ReviewStatus::Accepted);
    assert_eq!(
        delivery.review_event.as_ref().map(EventId::as_str),
        Some(seed.alias("rereview"))
    );
    assert_eq!(delivery.review_frontier, vec![id(seed.alias("rereview"))]);
    assert_eq!(
        delivery.review_history,
        vec![
            id(seed.alias("accept")),
            id(seed.alias("send-back")),
            id(seed.alias("rereview")),
        ]
    );
    let rules: Vec<&str> = project
        .rules
        .iter()
        .filter(|rule| rule.value.enabled)
        .map(|rule| rule.value.rule_id.as_str())
        .collect();
    assert_eq!(rules, vec!["check-bytes"]);

    // The delivery event itself is immutable: no Seen claim was written into it.
    let delivery_event = seed
        .events
        .iter()
        .find(|event| event.id == delivery_id)
        .expect("delivery event");
    let agendar_core::model::EventPayload::Delivery(body) = &delivery_event.payload else {
        panic!("not a delivery");
    };
    assert_eq!(
        body.criteria[0].claim,
        agendar_core::model::Claim::AgentSays
    );
    assert!(body.criteria[0].observation.is_none());

    // Prefix assertions: the first observation is Seen failure and pending review;
    // the second observation is a Seen pass. The later send-back makes y blocked.
    for (through, expected_seen, expected_review) in [
        (
            seed.alias("observation-0"),
            agendar_core::model::CheckResult::Failed,
            agendar_core::model::ReviewStatus::Pending,
        ),
        (
            seed.alias("observation-1"),
            agendar_core::model::CheckResult::Passed,
            agendar_core::model::ReviewStatus::Pending,
        ),
    ] {
        let prefix_id = id(through);
        let prefix: Vec<agendar_core::model::Event> = seed
            .events
            .iter()
            .filter(|event| {
                let order = seed
                    .events
                    .iter()
                    .position(|candidate| candidate.id == prefix_id)
                    .expect("prefix event");
                let index = seed
                    .events
                    .iter()
                    .position(|candidate| candidate.id == event.id)
                    .expect("event index");
                index <= order
            })
            .cloned()
            .collect();
        let prefix_state = reduce(&prefix, &ReduceContext { as_of: seed.as_of });
        let prefix_delivery = &seed_project(&prefix_state, &seed).deliveries[&delivery_id];
        assert_eq!(prefix_delivery.criteria[0].seen_result, Some(expected_seen));
        assert_eq!(prefix_delivery.review, expected_review);
    }
}

#[test]
fn seed_concurrent_observations_choose_the_documented_winner() {
    let seed = support::load_seed("concurrent-observations");
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let delivery = &project.deliveries[&id(seed.alias("delivery"))];
    let criterion = &delivery.criteria[0];
    assert_eq!(
        criterion.observation_frontier,
        vec![
            id(seed.alias("concurrent-0")),
            id(seed.alias("concurrent-1"))
        ]
    );
    assert_eq!(
        criterion.observation.as_ref().map(EventId::as_str),
        Some(seed.alias("concurrent-1")),
        "equal timestamps break by writer bytes, then seq and id"
    );
    assert_eq!(
        criterion.seen_result,
        Some(agendar_core::model::CheckResult::Failed)
    );
    assert!(criterion.conflict);
}

#[test]
fn delivery_seen_review_acceptance_opens_the_downstream_gate() {
    let seed = support::load_seed("delivery-seen-review");
    let state = reduce_seed(&seed);
    let project = seed_project(&state, &seed);
    let y = &project.features[&NodePath::parse("y").expect("node")];
    // The owner's accept settles x: its only delivery is accepted and no work
    // signal follows it, so x reads done with no agent echo and y opens.
    assert_eq!(y.gate, Gate::Open);
    let x = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(x.progress.as_str(), "done");
    assert!(x.phase.is_none());
}

fn reviewed_state(fixture: &support::Fixture) -> State {
    reduce(
        &fixture.events,
        &ReduceContext {
            as_of: fixture.as_of,
        },
    )
}

#[test]
fn every_fixture_reduces_to_its_reviewed_full_state() {
    for name in support::fixture_names() {
        let fixture = support::load_fixture(&name);
        let expected = fixture
            .expected
            .as_ref()
            .unwrap_or_else(|| panic!("fixture {name} must carry expected.json"));
        let state = reviewed_state(&fixture);
        assert_eq!(
            canonical_state(&state),
            canonical_json(expected),
            "fixture {name} drifted from its reviewed full-State golden"
        );
    }
}

#[test]
fn fixtures_cover_the_required_slice_1_assertions() {
    // one-question: receipt and ack commit points.
    let fixture = support::load_fixture("one-question");
    let state = reviewed_state(&fixture);
    let project = example(&state);
    let decision = project
        .decisions
        .get(&id("00000000000000000000000004"))
        .expect("decision");
    assert_eq!(decision.lifecycle.as_str(), "acked");
    assert_eq!(
        decision.applied_answer.as_ref().map(EventId::as_str),
        Some("00000000000000000000000005")
    );
    assert_eq!(decision.applied_sessions.len(), 1);
    assert_eq!(
        project.features[&NodePath::parse("x").expect("node")]
            .progress
            .as_str(),
        "done"
    );

    // answer-race: equal timestamps resolve deterministically by writer bytes.
    let fixture = support::load_fixture("answer-race");
    let state = reviewed_state(&fixture);
    let decision = example(&state)
        .decisions
        .get(&id("00000000000000000000000013"))
        .expect("decision");
    assert_eq!(
        decision.effective_receipt.as_ref().map(EventId::as_str),
        Some("00000000000000000000000015"),
        "host-02 writer sorts after host-01 for equal timestamps"
    );
    assert_eq!(decision.applied_answer, None);
    assert_eq!(decision.lifecycle.as_str(), "answered");

    // human-plan: human fields win while the agent-owned milestone updates.
    let fixture = support::load_fixture("human-plan");
    let state = reviewed_state(&fixture);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.title, "Human title");
    assert_eq!(
        feature.field_sources["title"].as_str(),
        "00000000000000000000000022"
    );
    assert_eq!(feature.priority, 2);
    assert_eq!(
        feature.field_sources["priority"].as_str(),
        "00000000000000000000000024"
    );
    assert!(!feature.paused);
    assert_eq!(feature.milestone.as_deref(), Some("M1"));

    // withdrawn-answer: the answer is history only and never becomes effective.
    let fixture = support::load_fixture("withdrawn-answer");
    let state = reviewed_state(&fixture);
    let decision = example(&state)
        .decisions
        .get(&id("00000000000000000000000033"))
        .expect("decision");
    assert_eq!(decision.lifecycle.as_str(), "withdrawn");
    assert_eq!(decision.effective_receipt, None);
    assert_eq!(
        decision.withdrawn_by.as_ref().map(EventId::as_str),
        Some("00000000000000000000000034")
    );

    // cycle: the closing edge is rejected with its own rescue.
    let fixture = support::load_fixture("cycle");
    let state = reviewed_state(&fixture);
    let project = example(&state);
    assert_eq!(
        project.features[&NodePath::parse("y").expect("node")].accepted_depends_on,
        vec![NodePath::parse("x").expect("node")]
    );
    assert_eq!(
        project.features[&NodePath::parse("x").expect("node")].accepted_depends_on,
        Vec::<NodePath>::new()
    );
    assert!(
        project
            .needs_you
            .iter()
            .any(|row| row.id == "rescue:cycle:00000000000000000000000044:y:x")
    );

    // attempts: a later concurrent start wins; an old attempt's done is history.
    let fixture = support::load_fixture("attempts");
    let state = reviewed_state(&fixture);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        feature.attempt.as_ref().map(EventId::as_str),
        Some("00000000000000000000000056")
    );
    assert_eq!(feature.progress.as_str(), "done");
    assert!(
        example(&state)
            .needs_you
            .iter()
            .any(|row| { row.id == "rescue:start-conflict:00000000000000000000000053" })
    );

    // missing-refs: the pending ask never hides the valid plan.
    let fixture = support::load_fixture("missing-refs");
    let state = reviewed_state(&fixture);
    let project = example(&state);
    assert!(project.decisions.is_empty());
    assert!(
        project
            .features
            .contains_key(&NodePath::parse("x").expect("node"))
    );
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("missing_reference") })
    );

    // writer-fork: both fork sides are quarantined, later events stay pending.
    let fixture = support::load_fixture("writer-fork");
    let state = reviewed_state(&fixture);
    let project = example(&state);
    assert!(project.features.is_empty());
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("writer_seq_fork") })
    );
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("writer_fork_pending") })
    );
}

// ---------------------------------------------------------------------------
// Reducer semantics that do not need a whole-state golden.
// ---------------------------------------------------------------------------

fn uid(value: u64) -> String {
    format!("{value:026}")
}

fn minute_ts(minute: u64) -> String {
    format!("2026-01-01T{:02}:{:02}:00.000Z", minute / 60, minute % 60)
}

fn ev(
    event: u64,
    seq: u64,
    writer: &str,
    minute: u64,
    kind: &str,
    refs: Value,
    body: Value,
) -> Event {
    let value = json!({
        "v": 1,
        "id": uid(event),
        "seq": seq,
        "writer": writer,
        "ts": minute_ts(minute),
        "project": "example",
        "refs": refs,
        "kind": kind,
        "body": body,
    });
    support::parse_value(&value).expect("test event must decode")
}

const H1: &str = "h-00000000000000000000000001";
const H2: &str = "h-00000000000000000000000002";
const S1: &str = "00000000000000000000000101";
const A1: &str = "h-00000000000000000000000001/s-00000000000000000000000101";
const OP1: &str = "h-00000000000000000000000001/operator";
const OP2: &str = "h-00000000000000000000000002/operator";
const ST1: &str = "h-00000000000000000000000001/steward";

fn agent_json(session: &str) -> Value {
    json!({"session": session, "harness": "generic"})
}

fn card_json() -> Value {
    serde_json::from_str(include_str!("oracle/examples/ask.json")).expect("approved ask example")
}

fn policy_json() -> Value {
    json!({
        "requested": "ask",
        "effective": "ask",
        "reversibility": "easy",
        "external": false,
        "heuristic_hits": [],
        "budget": {
            "speculative_features": 0,
            "pending_slices": 0,
            "demo_gap_s": 0,
            "blocked": false,
            "reasons": [],
        },
    })
}

fn state_at(events: Vec<Event>, minute: u64) -> State {
    reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(minute)).expect("timestamp"),
        },
    )
}

fn example(state: &State) -> &ProjectState {
    state
        .projects
        .get(&agendar_core::model::ProjectSlug::parse("example").expect("slug"))
        .expect("example project")
}

fn declared(event: u64, minute: u64) -> Event {
    ev(
        event,
        1,
        OP1,
        minute,
        "project.declared",
        json!({}),
        json!({"title": "Example", "brief": "b", "goal": "g"}),
    )
}

fn plan(event: u64, seq: u64, minute: u64, body: Value) -> Event {
    ev(
        event,
        seq,
        OP1,
        minute,
        "plan.feature",
        json!({"node": "x"}),
        body,
    )
}

fn feature_body() -> Value {
    json!({"title": "x", "slice": "example", "demo_kind": "cli_transcript", "depends_on": []})
}

fn start(event: u64, seq: u64, session: &str, writer: &str, minute: u64) -> Event {
    ev(
        event,
        seq,
        writer,
        minute,
        "work.progress",
        json!({"node": "x"}),
        json!({
            "agent": agent_json(session),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    )
}

fn ask(event: u64, seq: u64, minute: u64) -> Event {
    ev(
        event,
        seq,
        A1,
        minute,
        "decision.ask",
        json!({"node": "x"}),
        json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
    )
}

/// An ask whose own evaluated mode is `auto` or `assume`, as a CLI would write
/// it before emitting the matching choice.
fn ask_with_mode(event: u64, seq: u64, minute: u64, mode: &str) -> Event {
    ev(
        event,
        seq,
        A1,
        minute,
        "decision.ask",
        json!({"node": "x"}),
        json!({
            "agent": agent_json(S1),
            "card": card_json(),
            "policy": policy_mode_json(mode),
        }),
    )
}

fn policy_mode_json(mode: &str) -> Value {
    let mut policy = policy_json();
    policy["requested"] = json!(mode);
    policy["effective"] = json!(mode);
    policy
}

fn answer(event: u64, seq: u64, writer: &str, minute: u64, ask: u64, option: &str) -> Event {
    answer_on(event, seq, writer, minute, "x", ask, option)
}

fn answer_on(
    event: u64,
    seq: u64,
    writer: &str,
    minute: u64,
    node: &str,
    ask: u64,
    option: &str,
) -> Event {
    ev(
        event,
        seq,
        writer,
        minute,
        "decision.answer",
        json!({"node": node, "ask": uid(ask)}),
        json!({"option": option, "done_when": [{"id": "continues", "text": "It continues."}]}),
    )
}

fn ack(event: u64, seq: u64, minute: u64, ask: u64, answer: u64, causes: Vec<u64>) -> Event {
    ev(
        event,
        seq,
        A1,
        minute,
        "decision.ack",
        json!({
            "node": "x",
            "ask": uid(ask),
            "answer": uid(answer),
            "causes": causes.into_iter().map(uid).collect::<Vec<_>>(),
        }),
        json!({"agent": agent_json(S1), "applied_option": "adapters", "restarted": []}),
    )
}

fn base_events() -> Vec<Event> {
    vec![declared(1, 0), plan(2, 2, 0, feature_body())]
}

#[test]
fn ack_amendment_after_a_later_answer_sets_pending_pivot() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        answer(5, 3, OP1, 3, 4, "adapters"),
        ack(6, 3, 4, 4, 5, vec![]),
        answer(7, 1, OP2, 5, 4, "inbox"),
    ];
    let state = state_at(events, 30);
    let decision = &example(&state).decisions[&id(&uid(4))];
    assert_eq!(decision.lifecycle.as_str(), "amended");
    assert_eq!(
        decision.effective_receipt.as_ref().map(EventId::as_str),
        Some(uid(7).as_str())
    );
    assert_eq!(
        decision.applied_answer.as_ref().map(EventId::as_str),
        Some(uid(5).as_str())
    );
    assert!(decision.pending_pivot);
    assert_eq!(
        example(&state).summary.variance,
        agendar_core::model::Variance::Rework
    );
}

#[test]
fn withdrawal_after_a_causally_observed_ack_is_invalid() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        answer(5, 3, OP1, 3, 4, "adapters"),
        ack(6, 3, 4, 4, 5, vec![]),
        ev(
            7,
            4,
            A1,
            5,
            "decision.withdrawn",
            json!({"node": "x", "ask": uid(4)}),
            json!({"agent": agent_json(S1), "reason": "Too late."}),
        ),
    ];
    let state = state_at(events, 30);
    let decision = &example(&state).decisions[&id(&uid(4))];
    assert_eq!(decision.lifecycle.as_str(), "acked");
    assert_eq!(decision.withdrawn_by, None);
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("observed ack")
    }));
}

#[test]
fn superseding_an_unanswered_ask_closes_it_but_an_answered_one_is_rejected() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ask(3, 1, 2),
        ev(
            4,
            2,
            A1,
            3,
            "decision.ask",
            json!({"node": "x", "supersedes": uid(3)}),
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        ),
    ];
    let state = state_at(events, 10);
    let old = &example(&state).decisions[&id(&uid(3))];
    assert_eq!(old.lifecycle.as_str(), "withdrawn");
    assert_eq!(
        old.superseded_by.as_ref().map(EventId::as_str),
        Some(uid(4).as_str())
    );

    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ask(3, 1, 2),
        answer(4, 3, OP1, 3, 3, "adapters"),
        ev(
            5,
            2,
            A1,
            4,
            "decision.ask",
            json!({"node": "x", "supersedes": uid(3)}),
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        ),
    ];
    let state = state_at(events, 10);
    let old = &example(&state).decisions[&id(&uid(3))];
    assert_eq!(old.lifecycle.as_str(), "answered");
    assert_eq!(old.superseded_by, None);
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("cannot be superseded")
    }));
}

#[test]
fn restart_citing_the_previous_attempt_is_a_clean_supersede() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(
            4,
            2,
            A1,
            2,
            "work.progress",
            json!({"node": "x", "causes": [uid(3)]}),
            json!({
                "agent": agent_json(S1),
                "status": "started",
                "eta": {"min_s": 900, "max_s": 900},
                "evidence": [],
            }),
        ),
        ev(
            5,
            3,
            A1,
            3,
            "work.progress",
            json!({"node": "x", "attempt": uid(3)}),
            json!({"agent": agent_json(S1), "status": "done", "evidence": []}),
        ),
    ];
    let state = state_at(events, 30);
    let project = example(&state);
    let feature = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        feature.attempt.as_ref().map(EventId::as_str),
        Some(uid(4).as_str())
    );
    assert_eq!(feature.progress.as_str(), "doing");
    assert!(
        !project
            .needs_you
            .iter()
            .any(|row| row.id.starts_with("rescue:start-conflict:"))
    );
}

#[test]
fn handoff_is_pending_until_a_replacement_start_cites_it() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        plan(3, 3, 1, json!({"handoff": true})),
    ];
    let state = state_at(events, 5);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert!(feature.handoff);
    assert!(feature.handoff_pending);
    assert_eq!(
        feature.handoff_event.as_ref().map(EventId::as_str),
        Some(uid(3).as_str())
    );
    assert_eq!(feature.gate, Gate::AwaitingDecision);

    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        plan(3, 3, 1, json!({"handoff": true})),
        ev(
            4,
            1,
            "h-00000000000000000000000001/s-00000000000000000000000102",
            2,
            "work.progress",
            json!({"node": "x", "causes": [uid(3)]}),
            json!({
                "agent": agent_json("00000000000000000000000102"),
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        ),
    ];
    let state = state_at(events, 5);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert!(!feature.handoff_pending);
    assert_eq!(
        feature.handoff_consumed_by.as_ref().map(EventId::as_str),
        Some(uid(4).as_str())
    );
    assert_eq!(feature.gate, Gate::Open);
}

#[test]
fn eta_unions_overlapping_human_waits() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 0),
        ask(4, 2, 2),
        ev(
            5,
            3,
            A1,
            3,
            "decision.ask",
            json!({"node": "x"}),
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        ),
        answer(6, 4, OP1, 6, 4, "adapters"),
        answer(7, 5, OP1, 8, 5, "adapters"),
        ev(
            8,
            6,
            A1,
            16,
            "work.progress",
            json!({"node": "x", "attempt": uid(3)}),
            json!({"agent": agent_json(S1), "status": "done", "evidence": []}),
        ),
    ];
    let state = state_at(events, 60);
    let eta = example(&state).features[&NodePath::parse("x").expect("node")]
        .eta
        .as_ref()
        .expect("eta state");
    // Union of [2,6] and [4,8] is [2,8]: 360 paused seconds, not 480.
    assert_eq!(eta.paused_s, 360);
    assert_eq!(
        eta.agent_finish.latest,
        UtcTime::parse("2026-01-01T00:16:00.000Z").expect("timestamp")
    );
    assert!(!eta.paused);
}

#[test]
fn risk_heuristics_escalate_and_keep_the_hard_floor() {
    use agendar_core::reduce::policy::{evaluate_policy, heuristic_hits};
    let mut card = serde_json::from_str::<agendar_core::model::DecisionCard>(include_str!(
        "oracle/examples/ask.json"
    ))
    .expect("approved card");
    card.why_now = "We deploy the migration tonight.".to_owned();
    let hits = heuristic_hits(&card);
    assert!(hits.contains(&"deploy".to_owned()));
    assert!(hits.contains(&"migration".to_owned()));
    let state = state_at(base_events(), 10);
    let evaluation = evaluate_policy(
        &card,
        agendar_core::model::Mode::Auto,
        example(&state),
        UtcTime::parse(&minute_ts(10)).expect("timestamp"),
    );
    assert_eq!(
        evaluation.reversibility,
        agendar_core::model::Reversibility::Irreversible
    );
    assert_eq!(evaluation.effective, agendar_core::model::Mode::Ask);
    assert_eq!(evaluation.heuristic_hits, vec!["deploy", "migration"]);

    let mut card = serde_json::from_str::<agendar_core::model::DecisionCard>(include_str!(
        "oracle/examples/ask.json"
    ))
    .expect("approved card");
    card.options[1].consequence = "Run rm -rf on the scratch   directory.".to_owned();
    assert_eq!(heuristic_hits(&card), vec!["rm -rf"]);
}

#[test]
fn policy_set_is_validated_before_it_can_change_the_projection() {
    // The wire decoder owns the preset table and locked cells; a mismatched
    // policy can never reach the fold.
    let mut bad = serde_json::to_value(agendar_core::reduce::policy::default_policy())
        .expect("policy serializes");
    bad["preset"] = json!("strict");
    let value = json!({
        "v": 1,
        "id": uid(2),
        "seq": 3,
        "writer": OP1,
        "ts": minute_ts(1),
        "project": "example",
        "refs": {},
        "kind": "policy.set",
        "body": bad,
    });
    let error = support::parse_value(&value).expect_err("strict cells must match the table");
    assert_eq!(error.detail(), Some("preset_cells_mismatch"));

    // A valid human policy.set wins and carries the frozen revision formula.
    let strict = json!({
        "preset": "strict",
        "cells": {
            "easy_local": "ask",
            "easy_wide": "ask",
            "costly_local": "ask",
            "costly_wide": "ask",
            "irreversible_local": "ask",
            "irreversible_wide": "ask",
        },
        "domain_floor": {},
        "budget": {"max_speculative_features": 10, "max_pending_slices": 2, "max_demo_gap_s": 14400},
        "require_human_review": false,
        "allow_timeouts": false,
    });
    let events = vec![
        declared(1, 0),
        ev(2, 3, OP1, 1, "policy.set", json!({}), strict),
    ];
    let state = state_at(events, 5);
    let policy = &example(&state).policy;
    assert_eq!(policy.value.preset, agendar_core::model::Preset::Strict);
    assert_eq!(
        policy.revision,
        agendar_core::model::policy_revision(
            &agendar_core::model::ProjectSlug::parse("example").expect("slug"),
            Some(&id(&uid(2)))
        )
    );
}

#[test]
fn gate_precedence_puts_an_open_decision_before_upstream() {
    let mut events = vec![
        declared(1, 0),
        plan(2, 2, 0, json!({"depends_on": []})),
        ev(
            3,
            3,
            OP1,
            0,
            "plan.feature",
            json!({"node": "y"}),
            json!({"title": "y", "slice": "example", "demo_kind": "cli_transcript", "depends_on": ["x"]}),
        ),
        ev(
            4,
            2,
            A1,
            2,
            "decision.ask",
            json!({"node": "y"}),
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        ),
    ];
    let state = state_at(events.clone(), 10);
    let feature = &example(&state).features[&NodePath::parse("y").expect("node")];
    assert_eq!(feature.gate.as_str(), "awaiting_decision");
    assert!(
        feature
            .waits_on
            .iter()
            .any(|id| id.as_str() == "decision:00000000000000000000000004")
    );
    assert!(feature.waits_on.iter().any(|id| id.as_str() == "feature:x"));

    events.push(answer_on(5, 4, OP1, 4, "y", 4, "adapters"));
    let state = state_at(events, 10);
    let feature = &example(&state).features[&NodePath::parse("y").expect("node")];
    assert_eq!(feature.gate.as_str(), "awaiting_upstream");
}

#[test]
fn runnable_waits_for_receipt_application() {
    use agendar_core::reduce::attention::{runnable_for, runnable_nodes};
    let session = agendar_core::model::SessionId::parse(S1).expect("session");
    let node = NodePath::parse("x").expect("node");
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ask(3, 1, 2),
        answer(4, 3, OP1, 3, 3, "adapters"),
    ];
    let state = state_at(events.clone(), 10);
    assert!(!runnable_for(&session, &node, example(&state)));
    let mut events = events;
    events.push(ack(5, 3, 4, 3, 4, vec![]));
    let state = state_at(events, 10);
    assert!(runnable_for(&session, &node, example(&state)));
    assert_eq!(runnable_nodes(&session, example(&state)), vec![node]);
}

#[test]
fn session_loss_sets_lost_liveness_and_stopped_variance() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(
            4,
            1,
            ST1,
            2,
            "session.lost",
            json!({}),
            json!({
                "session": S1,
                "process_start": 7,
                "reason": "exited",
            }),
        ),
    ];
    let state = state_at(events, 10);
    let project = example(&state);
    let session = project
        .sessions
        .get(&agendar_core::model::SessionId::parse(S1).expect("session"))
        .expect("session");
    assert_eq!(session.liveness, agendar_core::model::Liveness::Lost);
    assert_eq!(
        session.lost_event.as_ref().map(EventId::as_str),
        Some(uid(4).as_str())
    );
    let feature = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.liveness, agendar_core::model::Liveness::Lost);
    assert_eq!(
        project.summary.variance,
        agendar_core::model::Variance::Stopped
    );
    assert_eq!(project.summary.stopped_steps, 1);
}

const S2: &str = "00000000000000000000000102";
const A2: &str = "h-00000000000000000000000001/s-00000000000000000000000102";
const S3: &str = "00000000000000000000000103";
const A3: &str = "h-00000000000000000000000001/s-00000000000000000000000103";

fn lost(event: u64, seq: u64, minute: u64, session: &str) -> Event {
    ev(
        event,
        seq,
        ST1,
        minute,
        "session.lost",
        json!({}),
        json!({"session": session, "process_start": 7, "reason": "exited"}),
    )
}

#[test]
fn a_stopped_step_beside_live_work_leaves_the_project_active_with_a_count() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        start(4, 1, S1, A1, 1),
        lost(5, 1, 2, S1),
        start_on(6, 1, "y", S2, A2, 3),
    ];
    // The other agent is live.
    let state = state_at(events.clone(), 10);
    let summary = &example(&state).summary;
    assert_eq!(summary.variance, agendar_core::model::Variance::Active);
    assert_eq!(summary.stopped_steps, 1);
    assert_eq!(
        serde_json::to_value(summary).expect("summary")["stopped_steps"],
        json!(1)
    );
    // Its holder went quiet but is not lost: the step is still being done.
    let state = state_at(events.clone(), 60);
    let project = example(&state);
    assert_eq!(project.summary.active_agents, 0);
    assert_eq!(
        project.summary.variance,
        agendar_core::model::Variance::Active
    );
    assert_eq!(project.summary.stopped_steps, 1);
    // The other agent is lost too: nothing is live, the project is stopped.
    let mut events = events;
    events.push(lost(7, 2, 4, S2));
    let state = state_at(events, 10);
    let summary = &example(&state).summary;
    assert_eq!(summary.variance, agendar_core::model::Variance::Stopped);
    assert_eq!(summary.stopped_steps, 2);
}

#[test]
fn a_live_agent_without_a_step_keeps_a_project_with_a_stopped_step_active() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        start(4, 1, S1, A1, 1),
        lost(5, 1, 2, S1),
        start_on(6, 1, "y", S2, A2, 3),
        done_on(7, 2, "y", S2, A2, 6, 4),
    ];
    let state = state_at(events.clone(), 10);
    let project = example(&state);
    assert_eq!(project.summary.active_agents, 1);
    assert_eq!(
        project.summary.variance,
        agendar_core::model::Variance::Active
    );
    assert_eq!(project.summary.stopped_steps, 1);
    // Once that agent is quiet nothing is live any more.
    let state = state_at(events, 60);
    let summary = &example(&state).summary;
    assert_eq!(summary.variance, agendar_core::model::Variance::Stopped);
    assert_eq!(summary.stopped_steps, 1);
}

#[test]
fn rework_still_wins_over_live_work_with_a_stopped_step() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        feature_y(4, 4, 0, "z"),
        start(5, 1, S1, A1, 1),
        ask(6, 2, 2),
        answer(7, 5, OP1, 3, 6, "adapters"),
        ack(8, 3, 4, 6, 7, vec![]),
        answer(9, 1, OP2, 5, 6, "inbox"),
        start_on(10, 1, "y", S2, A2, 5),
        lost(11, 1, 6, S2),
        start_on(12, 1, "z", S3, A3, 7),
    ];
    let state = state_at(events, 10);
    let summary = &example(&state).summary;
    assert_eq!(summary.variance, agendar_core::model::Variance::Rework);
    assert_eq!(summary.stopped_steps, 1);
}

#[test]
fn the_stopped_count_is_omitted_when_no_step_is_stopped() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
    ];
    let state = state_at(events, 10);
    let summary = &example(&state).summary;
    assert_eq!(summary.variance, agendar_core::model::Variance::Active);
    assert_eq!(summary.stopped_steps, 0);
    let value = serde_json::to_value(summary).expect("summary");
    assert!(value.get("stopped_steps").is_none());
}

#[test]
fn session_liveness_uses_the_fifteen_minute_freshness_window() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 0),
    ];
    let state = state_at(events.clone(), 10);
    let session = example(&state)
        .sessions
        .get(&agendar_core::model::SessionId::parse(S1).expect("session"))
        .expect("session");
    assert_eq!(session.liveness, agendar_core::model::Liveness::Live);
    let state = state_at(events, 20);
    let session = example(&state)
        .sessions
        .get(&agendar_core::model::SessionId::parse(S1).expect("session"))
        .expect("session");
    assert_eq!(session.liveness, agendar_core::model::Liveness::Quiet);
}

#[test]
fn verified_progress_respects_require_human_review() {
    let policy = json!({
        "preset": "balanced",
        "cells": {
            "easy_local": "auto",
            "easy_wide": "assume",
            "costly_local": "assume",
            "costly_wide": "ask",
            "irreversible_local": "ask",
            "irreversible_wide": "ask",
        },
        "domain_floor": {},
        "budget": {"max_speculative_features": 10, "max_pending_slices": 2, "max_demo_gap_s": 14400},
        "require_human_review": true,
        "allow_timeouts": true,
    });
    let blob = json!({"host": H1, "sha256": "49b2ac2c2826cfadc7f9ba7de8f550e4800afa0be6a5f0b1b10806d86b7b9203", "ext": "txt", "bytes": 21});
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(4, 3, OP1, 2, "policy.set", json!({}), policy),
        ev(
            5,
            4,
            A1,
            3,
            "work.progress",
            json!({"node": "x", "attempt": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "status": "verified",
                "evidence": [{
                    "criterion": "continues",
                    "status": "passed",
                    "claim": "agent_says",
                    "evidence": blob,
                }],
            }),
        ),
    ];
    let state = state_at(events, 10);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        feature.progress.as_str(),
        "done",
        "require_human_review keeps agent verification at done until accepted"
    );

    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(
            5,
            4,
            A1,
            3,
            "work.progress",
            json!({"node": "x", "attempt": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "status": "verified",
                "evidence": [{
                    "criterion": "continues",
                    "status": "passed",
                    "claim": "agent_says",
                    "evidence": blob,
                }],
            }),
        ),
    ];
    let state = state_at(events, 10);
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.progress.as_str(), "verified");
}

#[test]
fn host_as_of_is_clamped_to_the_evaluation_time() {
    // A future timestamp from one host must not report a future `as of`.
    let mut future = declared(1, 0);
    future.ts = UtcTime::parse("2026-01-02T00:00:00.000Z").expect("timestamp");
    let state = state_at(vec![future], 10);
    let summary = &example(&state).summary;
    let host = agendar_core::model::HostId::parse(H1).expect("host");
    assert_eq!(
        summary.host_as_of[&host],
        UtcTime::parse(&minute_ts(10)).expect("timestamp")
    );
}

#[test]
fn unused_operator_host_constant_is_valid() {
    // The second host is exercised by the answer-race fixture and by
    // concurrent-answer tests; this keeps the shared literal honest.
    assert!(agendar_core::model::HostId::parse(H2).is_ok());
    assert!(ST1.starts_with(H1));
}

#[test]
fn fixture_files_are_newline_terminated_and_typed() {
    for name in support::fixture_names() {
        let fixture = support::load_fixture(&name);
        assert!(!fixture.events.is_empty(), "fixture {name} has events");
        let text = std::fs::read_to_string(fixture.dir.join("events.jsonl")).expect("events");
        assert!(text.ends_with('\n'));
        let expected = fixture.expected.expect("expected.json");
        assert_eq!(expected["evaluated_at"], text_ts(&fixture.as_of));
    }
    fn text_ts(ts: &UtcTime) -> Value {
        json!(ts.canonical())
    }
}

// ---------------------------------------------------------------------------
// Fixed oracle: the runnable/stalled table.
// ---------------------------------------------------------------------------

fn feature_y(event: u64, seq: u64, minute: u64, node: &str) -> Event {
    ev(
        event,
        seq,
        OP1,
        minute,
        "plan.feature",
        json!({"node": node}),
        json!({"title": node, "slice": "example", "demo_kind": "cli_transcript", "depends_on": []}),
    )
}

fn start_on(event: u64, seq: u64, node: &str, session: &str, writer: &str, minute: u64) -> Event {
    ev(
        event,
        seq,
        writer,
        minute,
        "work.progress",
        json!({"node": node}),
        json!({
            "agent": agent_json(session),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    )
}

fn done_on(
    event: u64,
    seq: u64,
    node: &str,
    session: &str,
    writer: &str,
    attempt: u64,
    minute: u64,
) -> Event {
    ev(
        event,
        seq,
        writer,
        minute,
        "work.progress",
        json!({"node": node, "attempt": uid(attempt)}),
        json!({"agent": agent_json(session), "status": "done", "evidence": []}),
    )
}

fn ask_on(event: u64, seq: u64, node: &str, session: &str, writer: &str, minute: u64) -> Event {
    ev(
        event,
        seq,
        writer,
        minute,
        "decision.ask",
        json!({"node": node}),
        json!({"agent": agent_json(session), "card": card_json(), "policy": policy_json()}),
    )
}

fn needs_rows(state: &State) -> Vec<&agendar_core::model::NeedsYouItem> {
    example(state).needs_you.iter().collect()
}

#[test]
fn acceptance_contract_2_table_is_frozen() {
    use agendar_core::reduce::attention::runnable_nodes;
    let session = agendar_core::model::SessionId::parse(S1).expect("session");
    // Row 1: all other work terminal; the ask gates the only live focus.
    let t_writer = "h-00000000000000000000000001/s-00000000000000000000000102";
    let t_session = "00000000000000000000000102";
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        feature_y(4, 4, 0, "z"),
        feature_y(5, 5, 0, "w"),
        start(6, 1, S1, A1, 1),
        start_on(7, 1, "y", t_session, t_writer, 6),
        done_on(8, 2, "y", t_session, t_writer, 7, 7),
        start_on(9, 3, "z", t_session, t_writer, 7),
        ev(
            10,
            4,
            t_writer,
            8,
            "work.progress",
            json!({"node": "z", "attempt": uid(9)}),
            json!({
                "agent": agent_json(t_session),
                "status": "verified",
                "evidence": [{
                    "criterion": "continues",
                    "status": "passed",
                    "claim": "agent_says",
                    "evidence": {"host": H1, "sha256": "49b2ac2c2826cfadc7f9ba7de8f550e4800afa0be6a5f0b1b10806d86b7b9203", "ext": "txt", "bytes": 21},
                }],
            }),
        ),
        start_on(11, 5, "w", t_session, t_writer, 8),
        ev(
            12,
            6,
            t_writer,
            9,
            "work.progress",
            json!({"node": "w", "attempt": uid(11)}),
            json!({"agent": agent_json(t_session), "status": "dropped", "evidence": []}),
        ),
        ask_on(13, 2, "x", S1, A1, 10),
    ];
    let state = state_at(events, 20);
    assert_eq!(
        runnable_nodes(&session, example(&state)),
        Vec::<NodePath>::new()
    );
    let rows = needs_rows(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].stalled_agents, 1);
    assert!(notification_eligible(rows[0]));
    assert_eq!(rows[0].score_x2, 20, "easy risk, ten waiting minutes");

    // Row 2: another live session owns the only other runnable node.
    let t_writer = "h-00000000000000000000000001/s-00000000000000000000000102";
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        start(4, 1, S1, A1, 1),
        start_on(5, 1, "y", "00000000000000000000000102", t_writer, 9),
        ask_on(6, 2, "x", S1, A1, 10),
    ];
    let state = state_at(events, 20);
    assert_eq!(
        runnable_nodes(&session, example(&state)),
        Vec::<NodePath>::new()
    );
    let row = &needs_rows(&state)[0];
    assert_eq!(row.stalled_agents, 1);
    assert!(notification_eligible(row));

    // Row 3: an independent firm planned node stays available, so no stall.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        start(4, 1, S1, A1, 1),
        ask_on(5, 2, "x", S1, A1, 10),
    ];
    let state = state_at(events, 20);
    assert_eq!(
        runnable_nodes(&session, example(&state)),
        vec![NodePath::parse("y").expect("node")]
    );
    let row = &needs_rows(&state)[0];
    assert_eq!(row.stalled_agents, 0);
    assert!(!notification_eligible(row));

    // Row 4: two asks gate one session; both rows count it, the fleet counts once.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask_on(4, 2, "x", S1, A1, 10),
        ask_on(5, 3, "x", S1, A1, 10),
    ];
    let state = state_at(events, 20);
    assert_eq!(
        runnable_nodes(&session, example(&state)),
        Vec::<NodePath>::new()
    );
    let rows = needs_rows(&state);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.stalled_agents, 1);
        assert!(notification_eligible(row));
    }
    assert_eq!(example(&state).summary.stalled_agents, 1);
    assert_eq!(example(&state).summary.needs_count, 2);

    // Row 5: only completed work and no ask: nothing needs a human and Stop exits.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        done_on(4, 2, "x", S1, A1, 3, 2),
    ];
    let state = state_at(events, 20);
    assert_eq!(
        runnable_nodes(&session, example(&state)),
        Vec::<NodePath>::new()
    );
    assert!(needs_rows(&state).is_empty());
    assert!(example(&state).summary.needs_count == 0);
}

#[test]
fn x_placeholder_node_is_not_counted_as_a_feature() {
    // Guard against accidental node fabrication: a dependency on an undeclared
    // feature blocks its dependent instead of creating a feature.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, json!({"depends_on": ["ghost"]})),
    ];
    let state = state_at(events, 5);
    let project = example(&state);
    assert_eq!(project.features.len(), 1);
    let feature = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.gate.as_str(), "awaiting_upstream");
    assert!(
        feature
            .waits_on
            .iter()
            .any(|id| id.as_str() == "feature:ghost")
    );
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("missing_upstream") })
    );
}

#[test]
fn auto_and_assumed_choices_project_receipts_without_becoming_decide_rows() {
    use agendar_core::model::ChoiceReason;
    let choice = |kind: &str,
                  event: u64,
                  seq: u64,
                  minute: u64,
                  ask: u64,
                  reason: ChoiceReason,
                  policy: Value,
                  attempt: Option<u64>| {
        let mut refs = json!({"node": "x", "ask": uid(ask)});
        if let Some(attempt) = attempt {
            refs["attempt"] = json!(uid(attempt));
        }
        ev(
            event,
            seq,
            A1,
            minute,
            kind,
            refs,
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": match reason {
                    ChoiceReason::Policy => "policy",
                    ChoiceReason::Timeout => "timeout",
                },
                "policy": policy,
            }),
        )
    };
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask_with_mode(4, 2, 2, "auto"),
        choice(
            "decision.auto",
            5,
            3,
            3,
            4,
            ChoiceReason::Policy,
            policy_mode_json("auto"),
            Some(3),
        ),
    ];
    let state = state_at(events, 10);
    let project = example(&state);
    let decision = &project.decisions[&id(&uid(4))];
    assert_eq!(decision.lifecycle.as_str(), "answered");
    assert_eq!(
        decision.effective_receipt.as_ref().map(EventId::as_str),
        Some(uid(5).as_str())
    );
    assert_eq!(project.receipts[&id(&uid(5))].source.as_str_value(), "auto");
    assert!(
        project.needs_you.is_empty(),
        "auto is history, not a decide row"
    );

    // A policy assumption leaves the decision open with mode=assume.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask_with_mode(4, 2, 2, "assume"),
        choice(
            "decision.assumed",
            5,
            3,
            3,
            4,
            ChoiceReason::Policy,
            policy_mode_json("assume"),
            Some(3),
        ),
    ];
    let state = state_at(events, 10);
    let project = example(&state);
    let decision = &project.decisions[&id(&uid(4))];
    assert_eq!(decision.lifecycle.as_str(), "open");
    assert_eq!(decision.mode, agendar_core::model::Mode::Assume);
    assert!(decision.effective_receipt.is_some());
    assert!(
        project.needs_you.is_empty(),
        "an assumption under a valid budget is not a decide row in 1c"
    );

    // A timeout assumption is only legal once the declared deadline has elapsed.
    let timeout_events = |minute: u64| {
        let mut card = card_json();
        card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 14400});
        vec![
            declared(1, 0),
            plan(2, 2, 0, feature_body()),
            start(3, 1, S1, A1, 1),
            ev(
                5,
                3,
                A1,
                2,
                "decision.ask",
                json!({"node": "x"}),
                json!({"agent": agent_json(S1), "card": card, "policy": policy_json()}),
            ),
            choice(
                "decision.assumed",
                6,
                4,
                minute,
                5,
                ChoiceReason::Timeout,
                policy_mode_json("assume"),
                Some(3),
            ),
        ]
    };
    // 00:02 + 14400s = 04:02 (minute 242). One minute early is rejected, so the
    // decision stays open with no effective receipt; the legal boundary defaults.
    let early = state_at(timeout_events(241), 250);
    let early_project = example(&early);
    let early_decision = &early_project.decisions[&id(&uid(5))];
    assert_eq!(early_decision.lifecycle.as_str(), "open");
    assert!(early_decision.effective_receipt.is_none());
    assert!(early.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("premature")
    }));

    let state = state_at(timeout_events(242), 250);
    let decision = &example(&state).decisions[&id(&uid(5))];
    assert_eq!(decision.lifecycle.as_str(), "defaulted");
    assert!(decision.assumed_at.is_some());
    assert_eq!(
        decision.default_at,
        Some(UtcTime::parse("2026-01-01T04:02:00.000Z").expect("timestamp"))
    );
    // The consumed assumption keeps the attempt out of firm/runnable work.
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.basis, agendar_core::model::Basis::Assumed);
    assert_eq!(feature.assumptions, vec![id(&uid(6))]);
}

#[test]
fn an_ask_floor_cannot_be_turned_into_a_policy_assumption() {
    use agendar_core::model::ChoiceReason;
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        ev(
            5,
            3,
            A1,
            3,
            "decision.assumed",
            json!({"node": "x", "ask": uid(4), "attempt": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        ),
    ];
    let state = state_at(events, 10);
    let project = example(&state);
    assert!(
        project.decisions[&id(&uid(4))].effective_receipt.is_none(),
        "an ask-routed decision can never be assumed away"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("assume policy evaluation")
    }));
    let _ = ChoiceReason::Policy;
}

#[test]
fn a_choice_must_cite_the_authors_unfinished_attempt() {
    use agendar_core::model::ChoiceReason;
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask_with_mode(4, 2, 2, "assume"),
        ev(
            5,
            3,
            A1,
            3,
            "decision.assumed",
            json!({"node": "x", "ask": uid(4)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        ),
    ];
    let state = state_at(events, 10);
    assert!(
        example(&state).decisions[&id(&uid(4))]
            .effective_receipt
            .is_none()
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("choice_attempt_required")
    }));
    let _ = ChoiceReason::Policy;
}

trait ReceiptSourceLabel {
    fn as_str_value(&self) -> &'static str;
}

impl ReceiptSourceLabel for agendar_core::model::ReceiptSource {
    fn as_str_value(&self) -> &'static str {
        match self {
            agendar_core::model::ReceiptSource::Human => "human",
            agendar_core::model::ReceiptSource::Assumed => "assumed",
            agendar_core::model::ReceiptSource::Auto => "auto",
        }
    }
}

// ---------------------------------------------------------------------------
// Fix round 1 regressions: causal scope, typed references, policy snapshots,
// hard floors, speculation, ownership, ETA replays, focus and application.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn ev_project(
    project: &str,
    event: u64,
    seq: u64,
    writer: &str,
    minute: u64,
    kind: &str,
    refs: Value,
    body: Value,
) -> Event {
    let value = json!({
        "v": 1,
        "id": uid(event),
        "seq": seq,
        "writer": writer,
        "ts": minute_ts(minute),
        "project": project,
        "refs": refs,
        "kind": kind,
        "body": body,
    });
    let source = support::source_for(project, writer);
    support::parse_value_with_source(&value, &source).expect("test event must decode")
}

fn digest_value() -> Value {
    json!("49b2ac2c2826cfadc7f9ba7de8f550e4800afa0be6a5f0b1b10806d86b7b9203")
}

fn blob_value() -> Value {
    json!({"host": H1, "sha256": digest_value(), "ext": "txt", "bytes": 21})
}

#[test]
fn streams_are_scoped_per_project() {
    // Two legitimate operator/steward streams with the same writer and identical
    // sequence numbers in different projects are both eligible.
    let events = vec![
        ev_project(
            "alpha",
            1,
            1,
            OP1,
            0,
            "project.declared",
            json!({}),
            json!({"title": "Alpha", "brief": "a", "goal": "a"}),
        ),
        ev_project(
            "beta",
            2,
            1,
            OP1,
            0,
            "project.declared",
            json!({}),
            json!({"title": "Beta", "brief": "b", "goal": "b"}),
        ),
        ev_project(
            "alpha",
            3,
            2,
            OP1,
            0,
            "plan.feature",
            json!({"node": "x"}),
            json!({"title": "x", "slice": "alpha", "demo_kind": "cli_transcript", "depends_on": []}),
        ),
        ev_project(
            "beta",
            4,
            2,
            OP1,
            0,
            "plan.feature",
            json!({"node": "x"}),
            json!({"title": "x", "slice": "beta", "demo_kind": "cli_transcript", "depends_on": []}),
        ),
        // An independent gap in alpha only.
        ev_project(
            "alpha",
            5,
            4,
            OP1,
            1,
            "plan.feature",
            json!({"node": "y"}),
            json!({"title": "y", "slice": "alpha", "demo_kind": "cli_transcript", "depends_on": []}),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert_eq!(state.projects.len(), 2, "both projects project");
    for slug in ["alpha", "beta"] {
        let project = state
            .projects
            .get(&agendar_core::model::ProjectSlug::parse(slug).expect("slug"))
            .expect("project");
        assert_eq!(
            project.features.len(),
            if slug == "alpha" { 2 } else { 1 },
            "project {slug} keeps its own stream"
        );
    }
    let gaps: Vec<&str> = state
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.as_deref() == Some("sequence_gap"))
        .map(|diagnostic| diagnostic.id.as_str())
        .collect();
    assert_eq!(gaps.len(), 1, "only alpha has a gap");
    assert!(gaps[0].starts_with("sequence_gap:alpha:"), "{gaps:?}");
    assert!(
        !state
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("writer_seq_fork"))
    );

    // A fork confined to alpha leaves beta untouched.
    let forked = vec![
        ev_project(
            "alpha",
            11,
            1,
            OP1,
            0,
            "project.declared",
            json!({}),
            json!({"title": "A"}),
        ),
        ev_project(
            "alpha",
            12,
            2,
            OP1,
            0,
            "plan.feature",
            json!({"node": "x"}),
            json!({"title": "x", "slice": "a", "demo_kind": "cli_transcript"}),
        ),
        ev_project(
            "alpha",
            13,
            2,
            OP1,
            1,
            "plan.feature",
            json!({"node": "z"}),
            json!({"title": "z", "slice": "a", "demo_kind": "cli_transcript"}),
        ),
        ev_project(
            "beta",
            14,
            1,
            OP1,
            0,
            "project.declared",
            json!({}),
            json!({"title": "B"}),
        ),
        ev_project(
            "beta",
            15,
            2,
            OP1,
            0,
            "plan.feature",
            json!({"node": "x"}),
            json!({"title": "x", "slice": "b", "demo_kind": "cli_transcript"}),
        ),
    ];
    let state = reduce(
        &forked,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let alpha = &state.projects[&agendar_core::model::ProjectSlug::parse("alpha").expect("slug")];
    let beta = &state.projects[&agendar_core::model::ProjectSlug::parse("beta").expect("slug")];
    assert!(
        alpha.features.is_empty(),
        "alpha's fork quarantines both sides"
    );
    assert_eq!(
        beta.features.len(),
        1,
        "beta is not affected by alpha's fork"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("writer_seq_fork") && diagnostic.id.contains(":alpha:")
    }));
}

#[test]
fn an_available_pending_predecessor_holds_later_stream_events() {
    let ask = |event: u64, seq: u64, minute: u64, supersedes: Option<u64>| {
        let refs = match supersedes {
            Some(target) => json!({"node": "x", "supersedes": uid(target)}),
            None => json!({"node": "x"}),
        };
        ev(
            event,
            seq,
            A1,
            minute,
            "decision.ask",
            refs,
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        )
    };
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ask(3, 1, 1, Some(99)),
        // A later event of the *same agent stream* must not skip the available
        // pending ask that precedes it.
        ev(
            4,
            2,
            A1,
            2,
            "plan.feature",
            json!({"node": "z"}),
            json!({"agent": agent_json(S1), "title": "z", "slice": "example", "demo_kind": "cli_transcript", "depends_on": []}),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    assert!(
        !project
            .features
            .contains_key(&NodePath::parse("z").expect("node")),
        "a later stream event cannot skip a pending predecessor"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("pending_stream_predecessor")
            && diagnostic.event.as_ref().map(EventId::as_str) == Some(uid(4).as_str())
    }));
    assert!(
        !state.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("sequence_gap") && diagnostic.id.contains(A1)
        }),
        "an available-but-ineligible predecessor is not a sequence gap"
    );

    // Supplying the missing superseded ask resolves the whole stream.
    let mut resolved = events;
    resolved.push(ev(
        99,
        1,
        "h-00000000000000000000000001/s-00000000000000000000000102",
        3,
        "decision.ask",
        json!({"node": "x"}),
        json!({"agent": agent_json("00000000000000000000000102"), "card": card_json(), "policy": policy_json()}),
    ));
    let state = reduce(
        &resolved,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    assert!(
        project
            .features
            .contains_key(&NodePath::parse("z").expect("node")),
        "the later event becomes eligible once its predecessor is"
    );
    assert!(!project.decisions.is_empty());
}

fn observation_fixture(observe_criterion: &str) -> Vec<Event> {
    observation_fixture_with_result(observe_criterion, "passed")
}

fn observation_fixture_with_result(
    observe_criterion: &str,
    observation_result: &str,
) -> Vec<Event> {
    // A verified-progress evidence set: an eligible delivery for attempt 3 plus a
    // steward observation of its `continues` criterion.
    vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        answer(5, 3, OP1, 3, 4, "adapters"),
        ev(
            6,
            3,
            A1,
            4,
            "delivery",
            json!({"node": "x", "attempt": uid(3), "receipt": uid(5)}),
            json!({
                "agent": agent_json(S1),
                "name": "demo",
                "demos": [{"kind": "cli_transcript", "body": {"blob": blob_value()}}],
                "criteria": [{
                    "criterion": "continues",
                    "status": "passed",
                    "claim": "agent_says",
                    "evidence": blob_value(),
                }],
                "disclosure": {"proven": ["local"], "not_proven_yet": [], "faked_for_now": []},
            }),
        ),
        ev(
            7,
            1,
            ST1,
            5,
            "evidence.observed",
            json!({"node": "x", "delivery": uid(6)}),
            json!({
                "criterion": observe_criterion,
                "method": {"kind": "command", "check_id": "check-1", "argv_digest": digest_value()},
                "started_at": minute_ts(5),
                "finished_at": minute_ts(5),
                "result": observation_result,
                "digest": digest_value(),
                "output": blob_value(),
                "approval_id": "approval-1",
            }),
        ),
    ]
}

#[test]
fn typed_reference_roles_survive_generic_causes() {
    // The same target in `causes` must not erase the typed observation role.
    let wrong_kind = vec![
        declared(1, 0),
        ev(
            2,
            1,
            A1,
            1,
            "decision.ask",
            json!({"node": "x", "causes": [uid(1)]}),
            json!({
                "agent": agent_json(S1),
                "card": {
                    "why_now": "Need a decision.",
                    "question": "Which path?",
                    "reversibility": "easy",
                    "reach": "local",
                    "external": false,
                    "domain": "arch",
                    "options": [
                        {"id": "a", "label": "Do a", "consequence": "a", "reversibility": "easy", "external": false},
                        {"id": "b", "label": "Do b", "consequence": "b", "reversibility": "easy", "external": false},
                    ],
                    "recommended": {"option": "a", "confidence": "med", "because": "a"},
                    "already_checked": [{"text": "Declared.", "claim": "seen", "observation": uid(1)}],
                    "done_when": [{"id": "continues", "text": "It continues."}],
                    "on_timeout": {"kind": "hold"},
                },
                "policy": policy_mode_json("ask"),
            }),
        ),
    ];
    let state = reduce(
        &wrong_kind,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert!(
        example(&state).decisions.is_empty(),
        "a project declaration is not an observation"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("reference_mismatch")
            && diagnostic.message.contains("evidence.observed")
    }));

    // A well-formed observation still cannot prove another node's attempt.
    let mut wrong_node = observation_fixture("continues");
    wrong_node.push(ev(
        10,
        5,
        OP1,
        0,
        "plan.feature",
        json!({"node": "y"}),
        json!({"title": "y", "slice": "example", "demo_kind": "cli_transcript", "depends_on": []}),
    ));
    wrong_node.push(ev(
        11,
        4,
        A1,
        6,
        "work.progress",
        json!({"node": "y"}),
        json!({
            "agent": agent_json(S1),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    ));
    wrong_node.push(ev(
        12,
        5,
        A1,
        7,
        "work.progress",
        json!({"node": "y", "attempt": uid(11), "causes": [uid(7)]}),
        json!({
            "agent": agent_json(S1),
            "status": "done",
            "evidence": [{
                "criterion": "continues",
                "status": "passed",
                "claim": "seen",
                "observation": uid(7),
            }],
        }),
    ));
    let state = reduce(
        &wrong_node,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert_eq!(
        example(&state).features[&NodePath::parse("y").expect("node")].progress,
        agendar_core::model::Progress::Doing,
        "another node's observation never completes this attempt"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("reference_mismatch")
            && diagnostic.message.contains("same node")
    }));

    // A well-formed observation still rejects a mismatched criterion.
    let mut mismatch = observation_fixture("continues");
    mismatch.push(ev(
        8,
        4,
        A1,
        6,
        "work.progress",
        json!({"node": "x", "attempt": uid(3), "causes": [uid(7)]}),
        json!({
            "agent": agent_json(S1),
            "status": "verified",
            "evidence": [{
                "criterion": "other",
                "status": "passed",
                "claim": "seen",
                "observation": uid(7),
            }],
        }),
    ));
    let state = reduce(
        &mismatch,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert_eq!(
        example(&state).features[&NodePath::parse("x").expect("node")].progress,
        agendar_core::model::Progress::Doing,
        "the wrong-criterion progress event is quarantined"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("reference_mismatch")
            && diagnostic.message.contains("same criterion")
    }));

    // The correct binding is accepted.
    let mut good = observation_fixture("continues");
    good.push(ev(
        8,
        4,
        A1,
        6,
        "work.progress",
        json!({"node": "x", "attempt": uid(3), "causes": [uid(7)]}),
        json!({
            "agent": agent_json(S1),
            "status": "verified",
            "evidence": [{
                "criterion": "continues",
                "status": "passed",
                "claim": "seen",
                "evidence": blob_value(),
                "observation": uid(7),
            }],
        }),
    ));
    let state = reduce(
        &good,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert_eq!(
        example(&state).features[&NodePath::parse("x").expect("node")].progress,
        agendar_core::model::Progress::Verified
    );
}

/// A `work.progress verified` event citing exactly one steward observation.
fn verified_event_citing(event: u64, seq: u64, minute: u64, observation: u64) -> Event {
    ev(
        event,
        seq,
        A1,
        minute,
        "work.progress",
        json!({"node": "x", "attempt": uid(3), "causes": [uid(observation)]}),
        json!({
            "agent": agent_json(S1),
            "status": "verified",
            "evidence": [{
                "criterion": "continues",
                "status": "passed",
                "claim": "seen",
                "evidence": blob_value(),
                "observation": uid(observation),
            }],
        }),
    )
}

#[test]
fn only_a_passed_observation_can_verify_progress() {
    // 1. A failed (or not-proven) observation leaves the feature unverified even
    //    when the progress event cites it with the matching criterion and binding.
    for observation_result in ["failed", "not_proven"] {
        let mut events = observation_fixture_with_result("continues", observation_result);
        events.push(verified_event_citing(8, 4, 6, 7));
        let state = state_at(events, 10);
        assert_eq!(
            example(&state).features[&NodePath::parse("x").expect("node")].progress,
            agendar_core::model::Progress::Doing,
            "a {observation_result} observation must not verify work"
        );
        assert!(state.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("reference_mismatch")
                && diagnostic.event.as_ref().map(EventId::as_str) == Some(uid(8).as_str())
        }));
    }

    // 2. The same citation with a passed observation verifies the feature.
    let mut events = observation_fixture("continues");
    events.push(verified_event_citing(8, 4, 6, 7));
    let state = state_at(events, 10);
    assert_eq!(
        example(&state).features[&NodePath::parse("x").expect("node")].progress,
        agendar_core::model::Progress::Verified,
        "a passed observation verifies work"
    );

    // 3. Failed then passed: with only the failed observation the feature stays
    //    unverified; the causally later passing observation allows verification.
    let mut events = observation_fixture_with_result("continues", "failed");
    let state = state_at(events.clone(), 10);
    assert_eq!(
        example(&state).features[&NodePath::parse("x").expect("node")].progress,
        agendar_core::model::Progress::Doing,
        "the failed observation cannot verify work"
    );
    events.push(ev(
        9,
        2,
        ST1,
        6,
        "evidence.observed",
        json!({"node": "x", "delivery": uid(6), "causes": [uid(7)]}),
        json!({
            "criterion": "continues",
            "method": {"kind": "command", "check_id": "check-2", "argv_digest": digest_value()},
            "started_at": minute_ts(6),
            "finished_at": minute_ts(6),
            "result": "passed",
            "digest": digest_value(),
            "output": blob_value(),
            "approval_id": "approval-2",
        }),
    ));
    events.push(verified_event_citing(10, 4, 7, 9));
    let state = state_at(events, 10);
    assert_eq!(
        example(&state).features[&NodePath::parse("x").expect("node")].progress,
        agendar_core::model::Progress::Verified,
        "the later passed observation verifies work"
    );
}

fn policy_set_json(preset: &str) -> Value {
    let cells = match preset {
        "strict" => json!({
            "easy_local": "ask", "easy_wide": "ask", "costly_local": "ask",
            "costly_wide": "ask", "irreversible_local": "ask", "irreversible_wide": "ask",
        }),
        _ => json!({
            "easy_local": "auto", "easy_wide": "assume", "costly_local": "assume",
            "costly_wide": "ask", "irreversible_local": "ask", "irreversible_wide": "ask",
        }),
    };
    json!({
        "preset": preset,
        "cells": cells,
        "domain_floor": {},
        "budget": {"max_speculative_features": 10, "max_pending_slices": 2, "max_demo_gap_s": 14400},
        "require_human_review": false,
        "allow_timeouts": preset != "strict",
    })
}

#[test]
fn an_ask_uses_its_declared_causal_policy_snapshot() {
    let policy = |event: u64, seq: u64, minute: u64, preset: &str| {
        ev(
            event,
            seq,
            OP1,
            minute,
            "policy.set",
            json!({}),
            policy_set_json(preset),
        )
    };
    // A strict policy that an ask never observed (same ts, earlier writer) must
    // not invalidate a default-balanced ask.
    let concurrent = vec![
        declared(1, 0),
        policy(2, 2, 0, "strict"),
        plan(3, 3, 0, feature_body()),
        ask(4, 1, 0),
    ];
    let state = reduce(
        &concurrent,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let decision = example(&state)
        .decisions
        .get(&id(&uid(4)))
        .expect("the balanced ask stays eligible");
    assert_eq!(decision.mode, agendar_core::model::Mode::Ask);
    assert_eq!(decision.lifecycle.as_str(), "open");
    assert!(state.diagnostics.is_empty(), "{:?}", state.diagnostics);

    // An explicitly observed older policy wins over a later concurrent change,
    // even when the observed policy's wall clock is earlier.
    let observed = vec![
        declared(1, 0),
        policy(2, 2, 1, "balanced"),
        policy(3, 3, 0, "strict"),
        plan(4, 4, 0, feature_body()),
        ev(
            5,
            1,
            A1,
            2,
            "decision.ask",
            json!({"node": "x", "causes": [uid(2)]}),
            json!({
                "agent": agent_json(S1),
                "card": card_json(),
                "policy": {
                    "requested": "auto",
                    "effective": "auto",
                    "reversibility": "easy",
                    "external": false,
                    "policy_event": uid(2),
                    "heuristic_hits": [],
                    "budget": {
                        "speculative_features": 0,
                        "pending_slices": 0,
                        "demo_gap_s": 0,
                        "blocked": false,
                        "reasons": [],
                    },
                },
            }),
        ),
    ];
    let state = reduce(
        &observed,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let decision = example(&state)
        .decisions
        .get(&id(&uid(5)))
        .expect("the ask revalidates against the observed balanced policy");
    assert_eq!(decision.mode, agendar_core::model::Mode::Auto);

    // A declared source that is not an observed policy.set is rejected.
    let mut wrong_source = observed.clone();
    wrong_source[4] = ev(
        5,
        1,
        A1,
        2,
        "decision.ask",
        json!({"node": "x", "causes": [uid(1)]}),
        json!({
            "agent": agent_json(S1),
            "card": card_json(),
            "policy": {
                "requested": "auto",
                "effective": "auto",
                "reversibility": "easy",
                "external": false,
                "policy_event": uid(1),
                "heuristic_hits": [],
                "budget": {
                    "speculative_features": 0,
                    "pending_slices": 0,
                    "demo_gap_s": 0,
                    "blocked": false,
                    "reasons": [],
                },
            },
        }),
    );
    let state = reduce(
        &wrong_source,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert!(!example(&state).decisions.contains_key(&id(&uid(5))));
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("reference_mismatch")
            && diagnostic.message.contains("policy.set")
    }));
}

#[test]
fn imported_asks_and_choices_enforce_the_hard_floor() {
    use agendar_core::model::ChoiceReason;
    let proceed_card = || {
        let mut card = card_json();
        card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 60});
        card
    };
    // An external card may never declare a timeout default, imported or not.
    let external = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ev(
            3,
            1,
            A1,
            1,
            "decision.ask",
            json!({"node": "x"}),
            json!({
                "agent": agent_json(S1),
                "card": {
                    "why_now": "Ship it.",
                    "question": "Ship now?",
                    "reversibility": "easy",
                    "reach": "local",
                    "external": true,
                    "domain": "arch",
                    "options": [
                        {"id": "a", "label": "Do a", "consequence": "a", "reversibility": "easy", "external": false},
                        {"id": "b", "label": "Do b", "consequence": "b", "reversibility": "easy", "external": false},
                    ],
                    "recommended": {"option": "a", "confidence": "med", "because": "a"},
                    "already_checked": [],
                    "done_when": [{"id": "continues", "text": "It continues."}],
                    "on_timeout": {"kind": "proceed_as_assumption", "after_s": 60},
                },
                "policy": {
                    "requested": "ask",
                    "effective": "ask",
                    "reversibility": "easy",
                    "external": true,
                    "heuristic_hits": [],
                    "budget": {
                        "speculative_features": 0,
                        "pending_slices": 0,
                        "demo_gap_s": 0,
                        "blocked": false,
                        "reasons": [],
                    },
                },
            }),
        ),
    ];
    let state = reduce(
        &external,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert!(example(&state).decisions.is_empty());
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("on_timeout=hold")
    }));

    // A policy that forbids timeouts refuses a replicated timeout assumption,
    // even though the ask itself was authored under a permitting snapshot.
    let no_timeouts = vec![
        declared(1, 0),
        ev(
            2,
            2,
            OP1,
            0,
            "policy.set",
            json!({}),
            policy_set_json("strict"),
        ),
        plan(3, 3, 0, feature_body()),
        start(4, 1, S1, A1, 1),
        ev(
            5,
            2,
            A1,
            1,
            "decision.ask",
            json!({"node": "x"}),
            json!({"agent": agent_json(S1), "card": proceed_card(), "policy": policy_json()}),
        ),
        ev(
            6,
            3,
            A1,
            400,
            "decision.assumed",
            json!({"node": "x", "ask": uid(5), "attempt": uid(4)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "timeout",
                "policy": policy_mode_json("assume"),
            }),
        ),
    ];
    let state = reduce(
        &no_timeouts,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(500)).expect("ts"),
        },
    );
    assert!(
        example(&state).decisions[&id(&uid(5))]
            .effective_receipt
            .is_none()
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("allow_timeouts")
    }));

    // A valid external hold ask still never defaults, and only the original
    // asking session may author a timeout assumption.
    let mut hold = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(
            4,
            2,
            A1,
            2,
            "decision.ask",
            json!({"node": "x"}),
            json!({
                "agent": agent_json(S1),
                "card": {
                    "why_now": "Ship it.",
                    "question": "Ship now?",
                    "reversibility": "easy",
                    "reach": "local",
                    "external": true,
                    "domain": "arch",
                    "options": [
                        {"id": "adapters", "label": "Do a", "consequence": "a", "reversibility": "easy", "external": false},
                        {"id": "inbox", "label": "Do b", "consequence": "b", "reversibility": "easy", "external": false},
                    ],
                    "recommended": {"option": "adapters", "confidence": "med", "because": "a"},
                    "already_checked": [],
                    "done_when": [{"id": "continues", "text": "It continues."}],
                    "on_timeout": {"kind": "hold"},
                },
                "policy": {
                    "requested": "ask",
                    "effective": "ask",
                    "reversibility": "easy",
                    "external": true,
                    "heuristic_hits": [],
                    "budget": {
                        "speculative_features": 0,
                        "pending_slices": 0,
                        "demo_gap_s": 0,
                        "blocked": false,
                        "reasons": [],
                    },
                },
            }),
        ),
        ev(
            5,
            3,
            A1,
            3,
            "decision.assumed",
            json!({"node": "x", "ask": uid(4), "attempt": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "timeout",
                "policy": {
                    "requested": "assume",
                    "effective": "ask",
                    "reversibility": "easy",
                    "external": true,
                    "heuristic_hits": [],
                    "budget": {
                        "speculative_features": 0,
                        "pending_slices": 0,
                        "demo_gap_s": 0,
                        "blocked": false,
                        "reasons": [],
                    },
                },
            }),
        ),
    ];
    let state = reduce(
        &hold,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let decision = &example(&state).decisions[&id(&uid(4))];
    assert_eq!(decision.lifecycle.as_str(), "open");
    assert!(decision.effective_receipt.is_none());
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("never default")
    }));

    // A timeout assumption by a different session is rejected even when due.
    hold.pop();
    hold.push(ev(
        6,
        1,
        "h-00000000000000000000000001/s-00000000000000000000000102",
        400,
        "decision.assumed",
        json!({"node": "x", "ask": uid(4)}),
        json!({
            "agent": agent_json("00000000000000000000000102"),
            "option": "adapters",
            "reason": "timeout",
            "policy": policy_mode_json("assume"),
        }),
    ));
    let state = reduce(
        &hold,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(500)).expect("ts"),
        },
    );
    assert!(
        example(&state).decisions[&id(&uid(4))]
            .effective_receipt
            .is_none()
    );

    // An assumption receipt is not an answer: it can never be acknowledged.
    let mut assumed = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask_with_mode(4, 2, 2, "assume"),
        ev(
            5,
            3,
            A1,
            3,
            "decision.assumed",
            json!({"node": "x", "ask": uid(4), "attempt": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        ),
        ev(
            6,
            4,
            A1,
            4,
            "decision.ack",
            json!({"node": "x", "ask": uid(4), "answer": uid(5)}),
            json!({"agent": agent_json(S1), "applied_option": "adapters", "restarted": []}),
        ),
    ];
    let state = reduce(
        &assumed,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let decision = &example(&state).decisions[&id(&uid(4))];
    assert!(decision.ack_history.is_empty());
    assert_eq!(decision.lifecycle.as_str(), "open");
    assert_eq!(
        decision.effective_receipt.as_ref().map(EventId::as_str),
        Some(uid(5).as_str())
    );
    let _ = ChoiceReason::Policy;
    assumed.clear();
}

#[test]
fn unsupported_speculation_is_never_firm_or_runnable() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        feature_y(3, 3, 0, "y"),
        feature_y(4, 4, 0, "z"),
        start(5, 1, S1, A1, 1),
        ask_with_mode(6, 2, 2, "assume"),
        ev(
            7,
            3,
            A1,
            3,
            "decision.assumed",
            json!({"node": "x", "ask": uid(6), "attempt": uid(5)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        ),
        // Downstream imported speculation: y's start cites the assumption.
        ev(
            8,
            4,
            A1,
            4,
            "work.progress",
            json!({"node": "y", "causes": [uid(7)], "basis": [uid(7)]}),
            json!({
                "agent": agent_json(S1),
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    let x = &project.features[&NodePath::parse("x").expect("node")];
    let y = &project.features[&NodePath::parse("y").expect("node")];
    assert_eq!(x.basis, agendar_core::model::Basis::Assumed);
    assert_eq!(x.assumptions, vec![id(&uid(7))]);
    assert_eq!(y.basis, agendar_core::model::Basis::Assumed);
    assert_eq!(y.assumptions, vec![id(&uid(7))]);
    assert_eq!(
        y.progress,
        agendar_core::model::Progress::Doing,
        "history is preserved"
    );
    assert!(project.receipts.contains_key(&id(&uid(7))));

    let session = agendar_core::model::SessionId::parse(S1).expect("session");
    let runnable = agendar_core::reduce::attention::runnable_nodes(&session, project);
    assert_eq!(
        runnable,
        vec![NodePath::parse("z").expect("node")],
        "only unrelated firm work stays runnable"
    );
    let error = agendar_core::reduce::policy::unsupported_speculation_error();
    assert_eq!(error.code(), agendar_core::ErrorCode::NotImplemented);
}

#[test]
fn a_restart_requires_the_current_attempt() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        // A later same-writer start that ignores the attempt it replaces.
        ev(
            4,
            2,
            A1,
            2,
            "work.progress",
            json!({"node": "x"}),
            json!({
                "agent": agent_json(S1),
                "status": "started",
                "eta": {"min_s": 900, "max_s": 900},
                "evidence": [],
            }),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        feature.attempt.as_ref().map(EventId::as_str),
        Some(uid(3).as_str()),
        "the unproven restart never replaces current work"
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic.message.contains("must cite the current attempt")
    }));
}

#[test]
fn a_pending_handoff_bypasses_only_the_handoff_gate() {
    let a2 = "h-00000000000000000000000001/s-00000000000000000000000102";
    let a3 = "h-00000000000000000000000001/s-00000000000000000000000103";
    let s2 = "00000000000000000000000102";
    let s3 = "00000000000000000000000103";
    let replacement = |event: u64, seq: u64, session: &str, writer: &str, minute: u64| {
        ev(
            event,
            seq,
            writer,
            minute,
            "work.progress",
            json!({"node": "x", "causes": [uid(3), uid(5)]}),
            json!({
                "agent": agent_json(session),
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        )
    };
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        ev(
            5,
            3,
            OP1,
            3,
            "plan.feature",
            json!({"node": "x"}),
            json!({"handoff": true}),
        ),
        replacement(6, 1, s2, a2, 5),
        replacement(7, 1, s3, a3, 5),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    let feature = &project.features[&NodePath::parse("x").expect("node")];
    assert!(
        !feature.handoff_pending,
        "the winning replacement consumes it"
    );
    assert_eq!(
        feature.handoff_consumed_by.as_ref().map(EventId::as_str),
        Some(uid(7).as_str()),
        "the O-latest valid replacement wins, not the earliest"
    );
    let session = agendar_core::model::SessionId::parse(s3).expect("session");
    assert!(
        agendar_core::reduce::attention::runnable_nodes(&session, project).is_empty(),
        "the independent open ask still blocks the replacement"
    );
    // The losing concurrent replacement stays visible as conflict history.
    assert!(project.needs_you.iter().any(|row| {
        row.id.starts_with("rescue:start-conflict:") && row.id.ends_with(uid(6).as_str())
    }));
}

#[test]
fn eta_replays_human_gate_transitions() {
    let paused = |events: &[Event], minute: u64| {
        let state = reduce(
            events,
            &ReduceContext {
                as_of: UtcTime::parse(&minute_ts(minute)).expect("ts"),
            },
        );
        let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
        (
            feature.eta.as_ref().map(|eta| eta.paused_s).unwrap_or(0),
            state,
        )
    };
    // A withdrawn ask closes the wait at the withdrawal, not at evaluation time.
    let withdrawn = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        ev(
            5,
            3,
            A1,
            3,
            "decision.withdrawn",
            json!({"node": "x", "ask": uid(4)}),
            json!({"agent": agent_json(S1), "reason": "No longer needed."}),
        ),
    ];
    let (seconds, state) = paused(&withdrawn, 10);
    assert_eq!(seconds, 60, "only the open gate interval is human wait");
    let eta = example(&state).features[&NodePath::parse("x").expect("node")]
        .eta
        .clone()
        .expect("attempt eta");
    assert_eq!(
        eta.agent_finish.earliest,
        UtcTime::parse("2026-01-01T00:12:00.000Z").expect("ts")
    );

    // A later amendment does not extend an ended wait.
    let amended = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        answer(5, 3, OP1, 3, 4, "adapters"),
        answer(6, 4, "h-00000000000000000000000002/operator", 5, 4, "inbox"),
    ];
    let (seconds, _) = paused(&amended, 10);
    assert_eq!(
        seconds, 60,
        "post-answer amendment latency is not human wait"
    );

    // A superseding ask closes the old wait and opens its own.
    let superseded = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 2),
        ev(
            5,
            3,
            A1,
            4,
            "decision.ask",
            json!({"node": "x", "supersedes": uid(4)}),
            json!({"agent": agent_json(S1), "card": card_json(), "policy": policy_json()}),
        ),
    ];
    let (seconds, _) = paused(&superseded, 10);
    assert_eq!(
        seconds, 480,
        "consecutive waits union without double counting"
    );

    // An open prerequisite ask holds the downstream attempt too.
    let upstream = vec![
        declared(1, 0),
        feature_y(2, 2, 0, "y"),
        plan(
            3,
            3,
            0,
            json!({"title": "x", "slice": "example", "demo_kind": "cli_transcript", "depends_on": ["y"]}),
        ),
        start(4, 1, S1, A1, 1),
        ask_on(5, 2, "y", S1, A1, 2),
    ];
    let state = reduce(
        &upstream,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let feature = &example(&state).features[&NodePath::parse("x").expect("node")];
    assert_eq!(feature.gate, Gate::AwaitingUpstream);
    assert_eq!(feature.eta.as_ref().map(|eta| eta.paused_s), Some(480));

    // A clock-skewed negative interval clamps to zero and is diagnosed.
    let skewed = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ask(4, 2, 5),
        answer(5, 3, OP1, 2, 4, "adapters"),
    ];
    let (seconds, state) = paused(&skewed, 10);
    assert_eq!(seconds, 0);
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("clock_skew") })
    );
}

#[test]
fn a_plan_patch_cannot_move_focus_or_hide_a_stall() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ev(
            3,
            3,
            OP1,
            0,
            "plan.feature",
            json!({"node": "z"}),
            json!({"title": "z", "slice": "example", "demo_kind": "cli_transcript", "depends_on": ["x"]}),
        ),
        start(4, 1, S1, A1, 1),
        ask(5, 2, 2),
        // An agent plan patch on another feature must not move execution focus.
        ev(
            6,
            3,
            A1,
            4,
            "plan.feature",
            json!({"node": "z"}),
            json!({"agent": agent_json(S1), "milestone": "M1"}),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    let session = &project.sessions[&agendar_core::model::SessionId::parse(S1).expect("session")];
    assert_eq!(
        session.current_node.as_ref().map(NodePath::as_str),
        Some("x"),
        "only start/work-pulse/ask events set focus"
    );
    assert_eq!(
        session.last_event.as_str(),
        uid(6),
        "freshness still follows every authored event"
    );
    let decide = project
        .needs_you
        .iter()
        .find(|row| row.id == format!("decide:{}", uid(5)))
        .expect("decide row");
    assert_eq!(decide.stalled_agents, 1, "the stall is not hidden");
    assert_eq!(session.stalled_on, vec![decide.id.clone()]);
}

#[test]
fn choice_binding_uses_the_latest_causal_attempt() {
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        start(3, 1, S1, A1, 1),
        ev(
            4,
            2,
            A1,
            2,
            "work.progress",
            json!({"node": "x", "causes": [uid(3)]}),
            json!({
                "agent": agent_json(S1),
                "status": "started",
                "eta": {"min_s": 900, "max_s": 900},
                "evidence": [],
            }),
        ),
        done_on(5, 3, "x", S1, A1, 4, 3),
        ask_with_mode(6, 4, 4, "assume"),
    ];
    let choice = |event: u64, seq: u64, attempt: Option<u64>| {
        let mut refs = json!({"node": "x", "ask": uid(6)});
        if let Some(attempt) = attempt {
            refs["attempt"] = json!(uid(attempt));
        }
        ev(
            event,
            seq,
            A1,
            5,
            "decision.assumed",
            refs,
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        )
    };
    let mut wrong = events.clone();
    wrong.push(choice(7, 5, Some(3)));
    let state = reduce(
        &wrong,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert!(
        example(&state).decisions[&id(&uid(6))]
            .effective_receipt
            .is_none()
    );
    assert!(state.diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("semantic_invalid")
            && diagnostic
                .message
                .contains("must omit the attempt reference")
    }));

    // With no unfinished attempt, the choice omits the reference and acquires
    // nothing retroactively.
    let mut right = events;
    right.push(choice(8, 5, None));
    let state = reduce(
        &right,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    assert_eq!(
        project.decisions[&id(&uid(6))]
            .effective_receipt
            .as_ref()
            .map(EventId::as_str),
        Some(uid(8).as_str())
    );
    let feature = &project.features[&NodePath::parse("x").expect("node")];
    assert!(
        feature.assumptions.is_empty(),
        "no historical attempt acquires it"
    );
    assert_eq!(feature.basis, agendar_core::model::Basis::Firm);
}

#[test]
fn receipt_application_is_derived_from_descendant_work() {
    let mut events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ev(
            3,
            3,
            OP1,
            0,
            "plan.feature",
            json!({"node": "y"}),
            json!({"title": "y", "slice": "example", "demo_kind": "cli_transcript", "depends_on": ["x"]}),
        ),
        start(4, 1, S1, A1, 1),
        done_on(5, 2, "x", S1, A1, 4, 3),
        ask(6, 3, 4),
        answer(7, 4, OP1, 5, 6, "adapters"),
    ];
    let session = agendar_core::model::SessionId::parse(S1).expect("session");
    let before = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&before);
    assert!(agendar_core::reduce::attention::runnable_nodes(&session, project).is_empty());
    assert!(project.decisions[&id(&uid(6))].applied_sessions.is_empty());

    // A start on the *descendant* that carries the receipt proves application.
    events.push(ev(
        8,
        4,
        A1,
        6,
        "work.progress",
        json!({"node": "y", "causes": [uid(7)], "basis": [uid(7)]}),
        json!({
            "agent": agent_json(S1),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    ));
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let project = example(&state);
    assert_eq!(
        project.decisions[&id(&uid(6))].applied_sessions,
        vec![session.clone()]
    );
    assert_eq!(
        agendar_core::reduce::attention::runnable_nodes(&session, project),
        vec![NodePath::parse("y").expect("node")]
    );

    // An unrelated receipt never counts as application of this one.
    let mut unrelated = events;
    unrelated.push(answer(9, 4, OP1, 7, 6, "inbox"));
    unrelated.pop();
    let state = reduce(
        &unrelated,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    assert_eq!(
        example(&state).decisions[&id(&uid(6))].applied_sessions,
        vec![session],
        "the stale second answer changes the receipt but not the applied session"
    );
}

#[test]
fn needs_are_ordered_by_cost_of_delay() {
    let irreversible_card = || {
        let mut card = card_json();
        card["reversibility"] = json!("irreversible");
        card["on_timeout"] = json!({"kind": "hold"});
        card
    };
    let s2 = "00000000000000000000000102";
    let s3 = "00000000000000000000000103";
    let s4 = "00000000000000000000000104";
    let a2 = "h-00000000000000000000000001/s-00000000000000000000000102";
    let a3 = "h-00000000000000000000000001/s-00000000000000000000000103";
    let a4 = "h-00000000000000000000000001/s-00000000000000000000000104";
    let events = vec![
        declared(1, 0),
        feature_y(2, 2, 0, "p"),
        feature_y(3, 3, 0, "q"),
        ask_on(4, 1, "p", S1, A1, 0),
        start_on(5, 2, "p", S1, A1, 1),
        ev(
            6,
            2,
            a2,
            8,
            "decision.ask",
            json!({"node": "q"}),
            json!({
                "agent": agent_json(s2),
                "card": irreversible_card(),
                "policy": {
                    "requested": "ask",
                    "effective": "ask",
                    "reversibility": "irreversible",
                    "external": false,
                    "heuristic_hits": [],
                    "budget": {
                        "speculative_features": 0,
                        "pending_slices": 0,
                        "demo_gap_s": 0,
                        "blocked": false,
                        "reasons": [],
                    },
                },
            }),
        ),
        start_on(7, 1, "q", s2, a2, 3),
        start_on(8, 1, "q", s3, a3, 4),
        start_on(9, 1, "q", s4, a4, 5),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(10)).expect("ts"),
        },
    );
    let rows = needs_rows(&state);
    let scores: Vec<u64> = rows.iter().map(|row| row.score_x2).collect();
    let mut descending = scores.clone();
    descending.sort_by(|left, right| right.cmp(left));
    assert_eq!(scores, descending, "rows sort by score descending");
    let position = |event: u64| {
        rows.iter()
            .position(|row| row.id == format!("decide:{}", uid(event)))
            .expect("decide row")
    };
    assert!(
        position(6) < position(4),
        "the higher cost of delay comes first even though it is younger"
    );
    let older = &rows[position(4)];
    let younger = &rows[position(6)];
    assert_eq!(older.score_x2, 20, "easy, 10m, one stalled agent");
    assert_eq!(older.weight_x2, 2);
    assert_eq!(older.reason, "1 agent waiting · 10m");
    assert_eq!(
        younger.score_x2, 24,
        "irreversible, 2m, three stalled agents"
    );
    assert_eq!(younger.weight_x2, 4);
    assert_eq!(younger.reason, "3 agents waiting · 2m");
    assert!(older.since < younger.since);

    // Equal score, equal age: the stable item ID breaks the tie. Two asks on the
    // same stalled node share agents, age and weight.
    let tied = vec![
        declared(1, 0),
        feature_y(2, 2, 0, "p"),
        start_on(3, 1, "p", S1, A1, 1),
        ask_on(4, 2, "p", S1, A1, 5),
        ask_on(5, 3, "p", S1, A1, 5),
    ];
    let state = reduce(
        &tied,
        &ReduceContext {
            as_of: UtcTime::parse(&minute_ts(15)).expect("ts"),
        },
    );
    let rows = needs_rows(&state);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].score_x2, rows[1].score_x2);
    assert_eq!(rows[0].since, rows[1].since);
    assert_eq!(rows[0].id, format!("decide:{}", uid(4)));
    assert_eq!(rows[1].id, format!("decide:{}", uid(5)));
}

#[test]
fn cost_of_delay_weights_and_waiting_minutes_follow_the_frozen_table() {
    use agendar_core::model::Reversibility;
    use agendar_core::reduce::policy::weight_x2;

    assert_eq!(weight_x2(Reversibility::Easy, false), 2);
    assert_eq!(weight_x2(Reversibility::Easy, true), 4);
    assert_eq!(weight_x2(Reversibility::Costly, false), 3);
    assert_eq!(weight_x2(Reversibility::Irreversible, false), 4);
}

// ---------------------------------------------------------------------------
// Assumptions, confirmation, taint and repair.
// ---------------------------------------------------------------------------

/// The approved `transitive-taint` seed without its override answer: x holds a
/// policy assumption, y inherits it through `refs.basis`, and z never sees it.
fn assumption_seed_without_override() -> (support::Seed, Vec<Event>) {
    let seed = support::load_seed("transitive-taint");
    let override_id = id(seed.alias("override"));
    let events: Vec<Event> = seed
        .events
        .iter()
        .filter(|event| event.id != override_id)
        .cloned()
        .collect();
    (seed, events)
}

#[test]
fn an_unconfirmed_assumption_stays_assumed_across_a_completed_upstream() {
    // A completed assumed upstream never makes its children
    // firm. y starts with the assumption in its basis even though x is `done`.
    let (seed, events) = assumption_seed_without_override();
    let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let choice = id(seed.alias("choice"));
    let x = &project.features[&NodePath::parse("x").expect("node")];
    let y = &project.features[&NodePath::parse("y").expect("node")];
    assert_eq!(x.progress.as_str(), "done");
    assert_eq!(x.basis.as_str(), "assumed");
    assert_eq!(y.basis.as_str(), "assumed");
    assert_eq!(y.assumptions, vec![choice.clone()]);
    assert!(y.taints.is_empty());
    let decision = &project.decisions[&id(seed.alias("ask"))];
    assert_eq!(decision.built_on.len(), 2);
}

#[test]
fn confirming_an_assumption_clears_it_without_touching_progress() {
    let (seed, mut events) = assumption_seed_without_override();
    // A human answer that matches the recommendation (`adapters`).
    events.push(answer_on(12, 5, OP1, 7, "x", 7, "adapters"));
    let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    for (node, want_progress) in [("x", "done"), ("y", "doing"), ("z", "doing")] {
        let feature = &project.features[&NodePath::parse(node).expect("node")];
        assert_eq!(feature.progress.as_str(), want_progress);
        assert_eq!(feature.basis.as_str(), "firm", "{node} is confirmed");
        assert!(feature.assumptions.is_empty());
        assert!(feature.taints.is_empty());
    }
    let decision = &project.decisions[&id(seed.alias("ask"))];
    assert!(decision.built_on.is_empty(), "nothing is built on it now");
}

#[test]
fn a_restart_that_cites_the_override_repairs_only_that_feature() {
    let seed = support::load_seed("transitive-taint");
    let mut events = seed.events.clone();
    // Session 101 restarts its own tainted x, citing the override in `basis`.
    events.push(ev(
        12,
        5,
        A1,
        7,
        "work.progress",
        json!({
            "node": "x",
            "causes": [seed.alias("attempt-x")],
            "basis": [seed.alias("override")],
        }),
        json!({
            "agent": agent_json(S1),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    ));
    let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let x = &project.features[&NodePath::parse("x").expect("node")];
    let y = &project.features[&NodePath::parse("y").expect("node")];
    assert_eq!(x.basis.as_str(), "firm", "the repaired restart is firm");
    assert!(x.taints.is_empty());
    assert_eq!(x.progress.as_str(), "doing", "a restart starts over");
    assert_eq!(y.basis.as_str(), "tainted", "y repairs on its own");
    let decision = &project.decisions[&id(seed.alias("ask"))];
    assert_eq!(decision.built_on, vec![NodePath::parse("y").expect("node")]);

    // A start that ignores the override is never a repair.
    let mut silent = seed.events.clone();
    silent.push(ev(
        12,
        5,
        A1,
        7,
        "work.progress",
        json!({"node": "x", "causes": [seed.alias("attempt-x")]}),
        json!({
            "agent": agent_json(S1),
            "status": "started",
            "eta": {"min_s": 600, "max_s": 600},
            "evidence": [],
        }),
    ));
    let state = reduce(&silent, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let x = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        x.basis.as_str(),
        "tainted",
        "a start that omits the override leaves the taint"
    );
}

#[test]
fn a_later_completion_never_repairs_a_taint() {
    // B1: only an explicit replacement start can clear a taint. A done/verified
    // event that cites the overriding answer applies the receipt to the old
    // attempt; it does not restart it, so the taint and the rework stay
    //.
    let seed = support::load_seed("transitive-taint");
    let mut events = seed.events.clone();
    events.push(ev(
        12,
        5,
        A1,
        7,
        "work.progress",
        json!({
            "node": "x",
            "attempt": seed.alias("attempt-x"),
            "basis": [seed.alias("override")],
        }),
        json!({
            "agent": agent_json(S1),
            "status": "done",
            "evidence": [],
        }),
    ));
    let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let ask = id(seed.alias("ask"));
    let attempt = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(attempt.progress.as_str(), "done");
    assert_eq!(
        attempt.attempt.as_ref().map(EventId::as_str),
        Some(seed.alias("attempt-x")),
        "the completion never replaces the attempt"
    );
    assert_eq!(attempt.basis.as_str(), "tainted", "no restart, no repair");
    assert_eq!(attempt.taints, vec![ask.clone()]);
    assert!(
        !agendar_core::reduce::attention::runnable_for(
            &agendar_core::model::SessionId::parse(S1).expect("session"),
            &NodePath::parse("x").expect("node"),
            project,
        ),
        "the tainted work still needs rework"
    );

    // An ack of the override is not a restart either: it applies the receipt
    // for delivery, never for repair.
    let mut acked = events;
    acked.push(ev(
        13,
        6,
        A1,
        8,
        "decision.ack",
        json!({
            "node": "x",
            "ask": seed.alias("ask"),
            "answer": seed.alias("override"),
        }),
        json!({"agent": agent_json(S1), "applied_option": "inbox", "restarted": []}),
    ));
    let state = reduce(&acked, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let attempt = &project.features[&NodePath::parse("x").expect("node")];
    assert_eq!(
        attempt.basis.as_str(),
        "tainted",
        "an ack never repairs a taint"
    );
    assert_eq!(attempt.taints, vec![ask]);
}

/// One policy assumption on `x` plus `dependents` starts that inherit it.
fn assumption_board(dependents: usize) -> (Vec<Event>, u64) {
    let mut events = vec![declared(1, 0), plan(2, 2, 0, feature_body())];
    let names = ["a", "b", "c", "d"];
    for (index, name) in names.iter().take(dependents).enumerate() {
        events.push(feature_y(4 + index as u64, 3 + index as u64, 0, name));
    }
    events.push(start(10, 1, S1, A1, 1));
    events.push(ask_with_mode(11, 2, 2, "assume"));
    events.push(ev(
        12,
        3,
        A1,
        3,
        "decision.assumed",
        json!({"node": "x", "ask": uid(11), "attempt": uid(10)}),
        json!({
            "agent": agent_json(S1),
            "option": "adapters",
            "reason": "policy",
            "policy": policy_mode_json("assume"),
        }),
    ));
    for (index, name) in names.iter().take(dependents).enumerate() {
        events.push(ev(
            20 + index as u64,
            4 + index as u64,
            A1,
            4 + index as u64,
            "work.progress",
            json!({"node": name, "basis": [uid(12)]}),
            json!({
                "agent": agent_json(S1),
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        ));
    }
    (events, 12)
}

#[test]
fn a_confirm_item_appears_at_the_built_on_and_age_thresholds() {
    use agendar_core::model::NeedsKind;

    // Four dependents plus the target are exactly the threshold.
    let (events, _) = assumption_board(4);
    let state = state_at(events.clone(), 10);
    let project = example(&state);
    let decision = &project.decisions[&id(&uid(11))];
    assert_eq!(decision.built_on.len(), 5);
    let row = project
        .needs_you
        .iter()
        .find(|row| row.kind == NeedsKind::Confirm)
        .expect("a confirm row at five features built on it");
    assert_eq!(row.id, format!("confirm:{}", uid(11)));
    assert_eq!(row.question, decision.card.question);
    assert_eq!(row.reason, "5 steps built on it");
    assert_eq!(row.node.as_ref().map(NodePath::as_str), Some("x"));
    assert!(!agendar_core::reduce::attention::notification_eligible(row));
    assert!(
        !project
            .needs_you
            .iter()
            .any(|row| row.kind == NeedsKind::Decide),
        "an assumption is never a decide row"
    );

    // One fewer consumer and the row is gone before the age threshold.
    let (events, _) = assumption_board(3);
    let state = state_at(events, 10);
    assert!(
        !example(&state)
            .needs_you
            .iter()
            .any(|row| row.kind == NeedsKind::Confirm)
    );

    // Age alone triggers it, even with nothing built on the choice.
    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ask_with_mode(3, 2, 2, "assume"),
        ev(
            4,
            3,
            A1,
            2,
            "decision.assumed",
            json!({"node": "x", "ask": uid(3)}),
            json!({
                "agent": agent_json(S1),
                "option": "adapters",
                "reason": "policy",
                "policy": policy_mode_json("assume"),
            }),
        ),
    ];
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse("2026-01-01T12:00:00.000Z").expect("ts"),
        },
    );
    assert!(
        !example(&state)
            .needs_you
            .iter()
            .any(|row| row.kind == NeedsKind::Confirm),
        "under 24h with nothing built on it"
    );
    let state = reduce(
        &events,
        &ReduceContext {
            as_of: UtcTime::parse("2026-01-02T03:00:00.000Z").expect("ts"),
        },
    );
    let row = example(&state)
        .needs_you
        .iter()
        .find(|row| row.kind == NeedsKind::Confirm)
        .expect("the age threshold alone");
    assert_eq!(row.id, format!("confirm:{}", uid(3)));
    assert_eq!(row.reason, "Assumed 1d");
    assert!(example(&state).decisions[&id(&uid(3))].built_on.is_empty());
}

#[test]
fn the_graph_dto_matches_the_gate_edges_and_need_ids() {
    use agendar_core::model::{GraphNodeId, GraphNodeKind};

    let events = vec![
        declared(1, 0),
        plan(2, 2, 0, feature_body()),
        ev(
            3,
            3,
            OP1,
            0,
            "plan.feature",
            json!({"node": "y"}),
            json!({
                "title": "y",
                "slice": "example",
                "demo_kind": "cli_transcript",
                "depends_on": ["x"],
            }),
        ),
        ask_on(4, 1, "y", S1, A1, 2),
    ];
    let state = state_at(events, 10);
    let project = example(&state);
    let graph = agendar_core::reduce::graph::build_graph(project);

    let node_ids: Vec<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(
        node_ids,
        vec![
            GraphNodeId::decision(&id(&uid(4))).as_str().to_owned(),
            GraphNodeId::feature(&NodePath::parse("x").expect("node"))
                .as_str()
                .to_owned(),
            GraphNodeId::feature(&NodePath::parse("y").expect("node"))
                .as_str()
                .to_owned(),
        ]
    );

    // The decision node carries its Needs-you id; features never do.
    let decision_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == GraphNodeKind::Decision)
        .expect("decision node");
    assert_eq!(
        decision_node.need_id.as_deref(),
        Some("decide:00000000000000000000000004")
    );
    assert_eq!(
        decision_node.feature,
        Some(NodePath::parse("y").expect("node"))
    );
    assert!(!decision_node.assumed);
    for feature in project.features.values() {
        let node = graph
            .nodes
            .iter()
            .find(|node| node.id == GraphNodeId::feature(&feature.node))
            .expect("feature node");
        assert!(node.need_id.is_none());
        assert!(!node.assumed);
        assert_eq!(node.state, feature.progress.as_str());
        assert_eq!(node.label, feature.title);
    }

    // Accepted dependency edges equal the gate projection's own edges, plus the
    // decision → held-feature edge.
    let edges: Vec<(String, String)> = graph
        .edges
        .iter()
        .map(|edge| (edge.from.as_str().to_owned(), edge.to.as_str().to_owned()))
        .collect();
    assert_eq!(
        edges,
        vec![
            (format!("decision:{}", uid(4)), "feature:y".to_owned()),
            ("feature:x".to_owned(), "feature:y".to_owned()),
        ]
    );
    let y = &project.features[&NodePath::parse("y").expect("node")];
    assert_eq!(
        y.accepted_depends_on,
        vec![NodePath::parse("x").expect("node")]
    );
    let dependency_edge = &graph.edges[1];
    assert_eq!(
        dependency_edge.source_event,
        y.field_sources["depends_on"].clone(),
        "the edge is attributed to the winning depends_on patch"
    );
}

#[test]
fn a_pending_review_is_a_graph_node_with_its_needs_you_id() {
    use agendar_core::model::GraphNodeId;

    // Before the accept, the review is still pending and owns a review row.
    let seed = support::load_seed("delivery-seen-review");
    let events: Vec<Event> = seed
        .events
        .iter()
        .filter(|event| event.id.as_str() < "00000000000000000000000011")
        .cloned()
        .collect();
    let state = reduce(&events, &ReduceContext { as_of: seed.as_of });
    let project = seed_project(&state, &seed);
    let delivery = id(seed.alias("delivery"));
    let review_row = project
        .needs_you
        .iter()
        .find(|row| row.kind == agendar_core::model::NeedsKind::Review)
        .expect("pending review row");
    let graph = agendar_core::reduce::graph::build_graph(project);
    let node = graph
        .nodes
        .iter()
        .find(|node| node.id == GraphNodeId::review(&delivery))
        .expect("review node");
    assert_eq!(node.need_id.as_deref(), Some(review_row.id.as_str()));
    assert_eq!(node.state, "pending");
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.from == GraphNodeId::review(&delivery)
                && edge.to == GraphNodeId::feature(&node.feature.clone().expect("feature"))),
        "the review holds the feature it reviews"
    );
}
