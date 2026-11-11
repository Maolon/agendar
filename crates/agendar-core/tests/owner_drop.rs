//! The owner drops a step directly (`plan.dropped`, written by the operator).
//!
//! The step ends exactly as after an agent's `work.progress dropped`, plus
//! who dropped it. The owner's drop outranks whatever an agent reported
//! without having seen it, wherever the two fall in the causal order and in
//! whatever order hosts receive them; only a start that follows the drop
//! reopens the step. The reducer tests build events in memory; the service
//! tests use a temporary Vault and data root.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use agendar_core::board::BoardView;
use agendar_core::error::ErrorCode;
use agendar_core::model::{
    EtaRange, Event, EventId, FeatureState, Gate, Harness, NeedsKind, NodePath, PlanFeature,
    Progress, ProjectDeclared, ProjectSlug, ProjectState, Seconds, SessionId, State, UtcTime,
    WorkStatus, canonical_json,
};
use agendar_core::owed::owed;
use agendar_core::reduce::nudges::plan_changes;
use agendar_core::reduce::{ReduceContext, reduce};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::{Clock as _, SystemClock};
use serde_json::{Value, json};

const P: &str = "p";
/// An agent on the owner's computer (host 1).
const S1: &str = "00000000000000000000000101";
/// An agent on another computer (host 2).
const S2: &str = "00000000000000000000000102";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
/// The owner at a second computer.
const OPERATOR_3: &str = "h-00000000000000000000000003/operator";
const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn at(s: i64) -> UtcTime {
    UtcTime::parse("2026-01-01T00:00:00.000Z")
        .expect("timestamp")
        .add_seconds(s)
}

fn uid(value: u64) -> String {
    format!("{value:026}")
}

fn id(value: u64) -> EventId {
    EventId::parse(&uid(value)).expect("event id")
}

fn node(value: &str) -> NodePath {
    NodePath::parse(value).expect("node")
}

fn session(value: &str) -> SessionId {
    value.parse().expect("session")
}

fn slug() -> ProjectSlug {
    ProjectSlug::parse(P).expect("slug")
}

fn writer(session: &str) -> String {
    let host = if session == S2 { 2 } else { 1 };
    format!("h-{host:026}/s-{session}")
}

fn agent(session: &str) -> Value {
    json!({"session": session, "harness": "generic"})
}

fn reduce_at(events: &[Event], s: i64) -> State {
    reduce(events, &ReduceContext { as_of: at(s) })
}

fn project(state: &State) -> &ProjectState {
    &state.projects[&slug()]
}

fn step<'a>(state: &'a State, name: &str) -> &'a FeatureState {
    &project(state).features[&node(name)]
}

#[derive(Clone)]
struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with steps `a` and `b` (`b` depends on `a`) and
    /// two registered sessions.
    fn new() -> Self {
        let mut world = Self {
            events: Vec::new(),
            next_id: 1000,
            seq: BTreeMap::new(),
        };
        world.push(
            OPERATOR,
            0,
            "project.declared",
            json!({}),
            json!({"title": "Drop", "brief": "b", "goal": "g"}),
        );
        for (name, depends_on) in [("a", json!([])), ("b", json!(["a"]))] {
            world.push(
                OPERATOR,
                0,
                "plan.feature",
                json!({"node": name}),
                json!({"title": name, "slice": "s", "demo_kind": "cli_transcript",
                       "depends_on": depends_on}),
            );
        }
        for who in [S1, S2] {
            world.push(
                &writer(who),
                0,
                "pulse",
                json!({}),
                json!({"agent": agent(who), "message": "Session started", "boundary": false}),
            );
        }
        world
    }

    fn push(&mut self, writer: &str, s: i64, kind: &str, refs: Value, body: Value) -> u64 {
        let event = self.next_id;
        self.next_id += 1;
        let seq = self.seq.entry(writer.to_owned()).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1, "id": uid(event), "seq": *seq, "writer": writer, "ts": at(s),
            "project": P, "refs": refs, "kind": kind, "body": body,
        });
        self.events
            .push(support::parse_value(&value).expect("test event decodes"));
        event
    }

    /// The owner drops `step`.
    fn owner_drop(&mut self, step: &str, s: i64) -> u64 {
        self.owner_drop_from(OPERATOR, step, s)
    }

    fn owner_drop_from(&mut self, operator: &str, step: &str, s: i64) -> u64 {
        self.push(
            operator,
            s,
            "plan.dropped",
            json!({"node": step}),
            json!({"note": "Not needed any more."}),
        )
    }

    fn start(&mut self, by: &str, step: &str, s: i64, causes: &[u64]) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        self.push(
            &writer(by),
            s,
            "work.progress",
            json!({"node": step, "causes": causes}),
            json!({"agent": agent(by), "status": "started",
                   "eta": {"min_s": 600, "max_s": 1200}, "evidence": []}),
        )
    }

    fn status(&mut self, by: &str, step: &str, attempt: Option<u64>, status: &str, s: i64) -> u64 {
        let mut refs = json!({"node": step});
        if let Some(attempt) = attempt {
            refs["attempt"] = json!(uid(attempt));
        }
        self.push(
            &writer(by),
            s,
            "work.progress",
            refs,
            json!({"agent": agent(by), "status": status, "evidence": []}),
        )
    }

    fn pulse(&mut self, by: &str, step: &str, attempt: u64, s: i64) -> u64 {
        self.push(
            &writer(by),
            s,
            "pulse",
            json!({"node": step, "attempt": uid(attempt)}),
            json!({"agent": agent(by), "message": "Still at it", "boundary": false,
                   "eta": {"min_s": 600, "max_s": 1200}}),
        )
    }

    fn deliver(&mut self, by: &str, step: &str, attempt: u64, s: i64) -> u64 {
        let mut body = support::delivery_body();
        body["agent"] = agent(by);
        body["criteria"] = json!([]);
        self.push(
            &writer(by),
            s,
            "delivery",
            json!({"node": step, "attempt": uid(attempt)}),
            body,
        )
    }

    fn ask(&mut self, by: &str, step: &str, s: i64) -> u64 {
        self.push(
            &writer(by),
            s,
            "decision.ask",
            json!({"node": step}),
            json!({"agent": agent(by), "card": support::card(), "policy": support::policy_eval()}),
        )
    }
}

/// `step` is shown dropped, by the owner's `drop`.
fn assert_dropped_by_owner(state: &State, name: &str, drop: u64, label: &str) {
    let feature = step(state, name);
    assert_eq!(feature.progress, Progress::Dropped, "{label}: progress");
    let owner = feature.owner_drop.as_ref().unwrap_or_else(|| {
        panic!("{label}: the owner's drop is what the step shows");
    });
    assert_eq!(owner.event, id(drop), "{label}: which drop");
    assert_eq!(owner.note.as_deref(), Some("Not needed any more."));
    assert!(feature.claim.is_none(), "{label}: no claim");
    assert!(feature.eta.is_none(), "{label}: no ETA");
    assert!(feature.correction.is_none(), "{label}: no correction");
}

/// Every arrival order of the same events reduces to the same state.
fn assert_any_arrival_order(events: &[Event], s: i64) -> State {
    let expected = reduce_at(events, s);
    let text = canonical_json(&serde_json::to_value(&expected).expect("state"));
    let mut reversed = events.to_vec();
    reversed.reverse();
    let mut orders = vec![reversed];
    for shift in 1..events.len() {
        let mut rotated = events.to_vec();
        rotated.rotate_left(shift);
        orders.push(rotated);
    }
    for order in orders {
        let state = reduce_at(&order, s);
        assert_eq!(
            canonical_json(&serde_json::to_value(&state).expect("state")),
            text,
            "arrival order changed the state"
        );
    }
    expected
}

/// The owner's state of `name` equals the agent's, but for who dropped it.
fn assert_same_as_agent_drop(owners: &State, agents: &State, name: &str) {
    let mut expected = step(agents, name).clone();
    assert!(
        expected.owner_drop.is_none(),
        "an agent's drop names nobody"
    );
    expected.owner_drop = step(owners, name).owner_drop.clone();
    assert_eq!(step(owners, name), &expected, "{name}");
}

// ---------------------------------------------------------------------
// Reduced state
// ---------------------------------------------------------------------

#[test]
fn a_planned_step_ends_as_after_an_agents_drop() {
    let mut owners = World::new();
    let mut agents = owners.clone();
    let drop = owners.owner_drop("a", 10 * MIN);
    agents.status(S1, "a", None, "dropped", 10 * MIN);

    let late = 10 * MIN + HOUR;
    let owner_state = assert_any_arrival_order(&owners.events, late);
    let agent_state = reduce_at(&agents.events, late);
    assert_dropped_by_owner(&owner_state, "a", drop, "planned");
    assert_eq!(step(&owner_state, "a").attempt, None);
    assert_eq!(
        step(&owner_state, "a")
            .owner_drop
            .as_ref()
            .expect("drop")
            .at,
        at(10 * MIN)
    );
    assert_same_as_agent_drop(&owner_state, &agent_state, "a");
    // Its dependent, the counts (work left) and the agenda are the same.
    assert_eq!(step(&owner_state, "b"), step(&agent_state, "b"));
    assert_eq!(step(&owner_state, "b").gate, Gate::AwaitingUpstream);
    assert_eq!(
        project(&owner_state).summary.feature_counts,
        project(&agent_state).summary.feature_counts
    );
    assert_eq!(
        project(&owner_state).summary.feature_counts[&Progress::Dropped],
        1
    );
    assert_eq!(
        project(&owner_state).needs_you,
        project(&agent_state).needs_you
    );
    // A dropped step is never folded as untouched.
    assert!(!step(&reduce_at(&owners.events, 60 * DAY), "a").untouched);
}

#[test]
fn a_held_step_ends_its_attempt_and_its_agent_is_told() {
    let mut owners = World::new();
    let attempt = owners.start(S1, "a", MIN, &[]);
    let ask = owners.ask(S1, "a", 2 * MIN);
    let mut agents = owners.clone();
    let before = reduce_at(&owners.events, 5 * MIN);
    assert_eq!(step(&before, "a").progress, Progress::Doing);
    assert!(step(&before, "a").claim.is_some(), "S1 holds it");

    let drop = owners.owner_drop("a", 10 * MIN);
    agents.status(S1, "a", Some(attempt), "dropped", 10 * MIN);
    let late = 10 * MIN + HOUR;
    let owner_state = assert_any_arrival_order(&owners.events, late);
    let agent_state = reduce_at(&agents.events, late);
    assert_dropped_by_owner(&owner_state, "a", drop, "held");
    assert_eq!(step(&owner_state, "a").attempt, Some(id(attempt)));
    assert_eq!(step(&owner_state, "a").session, Some(session(S1)));
    assert_same_as_agent_drop(&owner_state, &agent_state, "a");
    assert_eq!(step(&owner_state, "b"), step(&agent_state, "b"));
    assert_eq!(
        project(&owner_state).needs_you,
        project(&agent_state).needs_you
    );

    // The agent learns it: its question carries the "on a closed step" label
    // and `settle` says so; the step is no longer its debt; the plan-change
    // nudge names the drop once.
    let row = project(&owner_state)
        .needs_you
        .iter()
        .find(|row| row.id == format!("decide:{}", uid(ask)))
        .expect("the question stays answerable");
    let closed = row.closed_step.as_ref().expect("on a closed step");
    assert_eq!(closed.since, at(10 * MIN));
    assert!(!closed.folded);
    let debt = owed(
        project(&owner_state),
        &owners.events,
        &session(S1),
        at(late),
        &BTreeMap::new(),
    );
    assert!(debt.yours.is_empty(), "nothing to keep up to date");
    assert_eq!(debt.questions.len(), 1);
    assert!(debt.questions[0].say.contains("is on a closed step"));
    assert_eq!(debt.dropped.len(), 1);
    assert_eq!(
        debt.dropped[0].say,
        "a: dropped by the owner 1h00m ago (Not needed any more.). Stop work on it; nothing to report"
    );
    let nudges = plan_changes(&owners.events, &slug(), None, at(0));
    let told: Vec<&str> = nudges
        .iter()
        .filter(|nudge| nudge.event == Some(id(drop)))
        .map(|nudge| nudge.text.as_str())
        .collect();
    assert_eq!(
        told,
        ["The owner dropped a (Not needed any more.). Stop work on it; read board."]
    );
    // Another agent is told nothing in `settle`, and after a day neither is S1.
    let other = owed(
        project(&owner_state),
        &owners.events,
        &session(S2),
        at(late),
        &BTreeMap::new(),
    );
    assert!(other.dropped.is_empty());
    let next_day = reduce_at(&owners.events, 10 * MIN + DAY);
    let debt = owed(
        project(&next_day),
        &owners.events,
        &session(S1),
        at(10 * MIN + DAY),
        &BTreeMap::new(),
    );
    assert!(debt.dropped.is_empty(), "shown for a day");
    assert!(debt.yours.is_empty());
}

/// A step in review may be dropped: the delivery waits for the owner, so the
/// drop is their verdict on it and the review row leaves the agenda, as when
/// the agent drops the attempt that delivered.
#[test]
fn a_step_in_review_is_dropped_and_its_review_leaves_the_agenda() {
    let mut owners = World::new();
    let attempt = owners.start(S1, "a", MIN, &[]);
    owners.deliver(S1, "a", attempt, 5 * MIN);
    let mut agents = owners.clone();
    let reviews = |state: &State| {
        project(state)
            .needs_you
            .iter()
            .filter(|row| row.kind == NeedsKind::Review)
            .count()
    };
    assert_eq!(reviews(&reduce_at(&owners.events, 6 * MIN)), 1);

    let drop = owners.owner_drop("a", 10 * MIN);
    agents.status(S1, "a", Some(attempt), "dropped", 10 * MIN);
    let owner_state = assert_any_arrival_order(&owners.events, HOUR);
    let agent_state = reduce_at(&agents.events, HOUR);
    assert_dropped_by_owner(&owner_state, "a", drop, "in review");
    assert_eq!(step(&owner_state, "a").phase, None);
    assert_eq!(reviews(&owner_state), 0);
    assert_same_as_agent_drop(&owner_state, &agent_state, "a");
    assert_eq!(
        project(&owner_state).needs_you,
        project(&agent_state).needs_you
    );
}

// ---------------------------------------------------------------------
// Owner > agent, in both orders
// ---------------------------------------------------------------------

/// The agent on host 2 reports on its attempt without having seen the drop.
/// Its clock may be behind or ahead of the owner's: the reports fall before
/// or after the drop in the causal order, and the owner wins both times.
#[test]
fn the_owners_drop_wins_over_a_concurrent_pulse_and_done_in_both_orders() {
    for (label, report_at) in [("reports before", 8 * MIN), ("reports after", 12 * MIN)] {
        let mut world = World::new();
        let attempt = world.start(S2, "a", MIN, &[]);
        let drop = world.owner_drop("a", 10 * MIN);
        world.pulse(S2, "a", attempt, report_at);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, &format!("{label}: pulse"));

        world.status(S2, "a", Some(attempt), "done", report_at + MIN);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, &format!("{label}: done"));
        assert_eq!(step(&state, "a").attempt, Some(id(attempt)));
        assert_eq!(step(&state, "b").gate, Gate::AwaitingUpstream);
    }
}

#[test]
fn the_owners_drop_wins_over_a_concurrent_start_in_both_orders() {
    for (label, start_at) in [("starts before", 8 * MIN), ("starts after", 12 * MIN)] {
        let mut world = World::new();
        let drop = world.owner_drop("a", 10 * MIN);
        // Host 2 had not received the drop: its start does not cite it.
        let attempt = world.start(S2, "a", start_at, &[]);
        world.pulse(S2, "a", attempt, start_at + MIN);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, label);
        assert_eq!(step(&state, "a").session, Some(session(S2)));
        world.status(S2, "a", Some(attempt), "done", start_at + 2 * MIN);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, &format!("{label}, then done"));
        assert!(
            state
                .diagnostics
                .iter()
                .all(|note| note.code.as_deref() != Some("semantic_invalid")),
            "{label}: nothing of the agent's is rejected: {:?}",
            state.diagnostics
        );
    }
}

#[test]
fn the_owners_drop_wins_over_the_agents_own_drop_in_both_orders() {
    for (label, agent_at) in [("agent first", 8 * MIN), ("owner first", 12 * MIN)] {
        let mut world = World::new();
        let attempt = world.start(S2, "a", MIN, &[]);
        let drop = world.owner_drop("a", 10 * MIN);
        world.status(S2, "a", Some(attempt), "dropped", agent_at);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, label);
    }
    // The same for a step nobody started.
    for (label, agent_at) in [("agent first", 8 * MIN), ("owner first", 12 * MIN)] {
        let mut world = World::new();
        let drop = world.owner_drop("a", 10 * MIN);
        world.status(S2, "a", None, "dropped", agent_at);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_dropped_by_owner(&state, "a", drop, label);
    }
}

#[test]
fn two_drops_by_the_owner_show_one_and_agree_on_every_host() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", MIN, &[]);
    let first = world.owner_drop("a", 10 * MIN);
    world.owner_drop_from(OPERATOR_3, "a", 11 * MIN);
    let state = assert_any_arrival_order(&world.events, HOUR);
    assert_dropped_by_owner(&state, "a", first, "two drops");
    assert_eq!(step(&state, "a").attempt, Some(id(attempt)));
}

// ---------------------------------------------------------------------
// Starting again
// ---------------------------------------------------------------------

#[test]
fn only_a_start_that_follows_the_drop_reopens_the_step() {
    let mut world = World::new();
    let attempt = world.start(S2, "a", MIN, &[]);
    let drop = world.owner_drop("a", 10 * MIN);
    // The restart cites the attempt it replaces and the owner's drop.
    let again = world.start(S2, "a", 20 * MIN, &[attempt, drop]);
    let state = assert_any_arrival_order(&world.events, 25 * MIN);
    let feature = step(&state, "a");
    assert_eq!(feature.progress, Progress::Doing);
    assert_eq!(feature.attempt, Some(id(again)));
    assert!(feature.owner_drop.is_none(), "open again");
    assert!(feature.claim.is_some());

    // A start from a host that still had not seen the drop arrives late: it
    // is closed by the drop and does not replace the live attempt, in either
    // order.
    for late_at in [15 * MIN, 30 * MIN] {
        let mut late = world.clone();
        let stale = late.start(S1, "a", late_at, &[]);
        late.status(S1, "a", Some(stale), "done", late_at + MIN);
        let state = assert_any_arrival_order(&late.events, 40 * MIN);
        let feature = step(&state, "a");
        assert_eq!(feature.progress, Progress::Doing, "late start at {late_at}");
        assert_eq!(feature.attempt, Some(id(again)));
        assert_eq!(feature.session, Some(session(S2)));
        assert!(
            project(&state)
                .needs_you
                .iter()
                .all(|row| !row.id.starts_with("rescue:start-conflict:")),
            "the owner settled it: no conflict row"
        );
    }

    // A second drop closes the restart too.
    let second = world.owner_drop("a", 50 * MIN);
    let state = assert_any_arrival_order(&world.events, HOUR);
    assert_dropped_by_owner(&state, "a", second, "dropped again");

    // A never-started step: a start that follows the drop brings it back.
    let mut world = World::new();
    let drop = world.owner_drop("b", 10 * MIN);
    let start = world.start(S1, "b", 20 * MIN, &[drop]);
    let state = reduce_at(&world.events, 25 * MIN);
    assert_eq!(step(&state, "b").progress, Progress::Doing);
    assert_eq!(step(&state, "b").attempt, Some(id(start)));
    assert!(step(&state, "b").owner_drop.is_none());
}

// ---------------------------------------------------------------------
// The drop never makes an agent's event invalid or pending, and the state
// does not depend on where the agent's event falls in time
// ---------------------------------------------------------------------

const HELD_BACK: [&str; 5] = [
    "semantic_invalid",
    "missing_reference",
    "pending_reference",
    "pending_stream_predecessor",
    "reference_mismatch",
];

/// No event is rejected or left pending.
fn assert_nothing_held(state: &State, label: &str) {
    let held: Vec<&str> = state
        .diagnostics
        .iter()
        .filter(|note| {
            note.code
                .as_deref()
                .is_some_and(|code| HELD_BACK.contains(&code))
        })
        .map(|note| note.message.as_str())
        .collect();
    assert!(held.is_empty(), "{label}: {held:?}");
}

fn codes(state: &State) -> Vec<String> {
    let mut codes: Vec<String> = state
        .diagnostics
        .iter()
        .filter_map(|note| note.code.clone())
        .collect();
    codes.sort();
    codes
}

fn conflict_rows(state: &State) -> Vec<&str> {
    project(state)
        .needs_you
        .iter()
        .filter(|row| row.id.starts_with("rescue:start-conflict:"))
        .map(|row| row.id.as_str())
        .collect()
}

fn assume_eval() -> Value {
    let mut policy = support::policy_eval();
    policy["requested"] = json!("assume");
    policy["effective"] = json!("assume");
    policy
}

/// Review finding 1: a choice that cites an attempt the owner closed.
#[test]
fn a_choice_on_an_attempt_the_owner_closed_is_not_rejected() {
    for kind in ["decision.assumed", "decision.auto"] {
        let mut shown = Vec::new();
        for (label, choice_at) in [("choice before", 8 * MIN), ("choice after", 12 * MIN)] {
            let label = format!("{kind}, {label}");
            let mut world = World::new();
            let attempt = world.start(S2, "a", MIN, &[]);
            let policy = if kind == "decision.auto" {
                let mut policy = assume_eval();
                policy["requested"] = json!("auto");
                policy["effective"] = json!("auto");
                policy
            } else {
                assume_eval()
            };
            let ask = world.push(
                &writer(S2),
                2 * MIN,
                "decision.ask",
                json!({"node": "a"}),
                json!({"agent": agent(S2), "card": support::card(), "policy": policy}),
            );
            let drop = world.owner_drop("a", 10 * MIN);
            world.push(
                &writer(S2),
                choice_at,
                kind,
                json!({"node": "a", "ask": uid(ask), "attempt": uid(attempt)}),
                json!({"agent": agent(S2), "option": "a", "reason": "policy", "policy": policy}),
            );
            // The session goes on writing: its stream must not stall.
            world.push(
                &writer(S2),
                14 * MIN,
                "pulse",
                json!({}),
                json!({"agent": agent(S2), "message": "Next", "boundary": false}),
            );
            let state = assert_any_arrival_order(&world.events, HOUR);
            assert_nothing_held(&state, &label);
            assert_dropped_by_owner(&state, "a", drop, &label);
            let decision = &project(&state).decisions[&id(ask)];
            shown.push((
                decision.lifecycle,
                decision.effective_receipt.is_some(),
                step(&state, "a").assumptions.len(),
                step(&state, "a").taints.len(),
                codes(&state),
            ));
        }
        assert_eq!(shown[0], shown[1], "{kind}: same state in both orders");
        assert!(shown[0].1, "{kind}: the choice is folded");
    }
}

/// Review finding 5: a restart written where the drop was not readable yet
/// cites the attempt that was current there.
#[test]
fn a_restart_citing_an_attempt_the_owner_closed_is_not_rejected() {
    let mut world = World::new();
    let first = world.start(S1, "a", MIN, &[]);
    let drop = world.owner_drop("a", 2 * MIN);
    let again = world.start(S1, "a", 4 * MIN, &[first, drop]);
    let word = world.pulse(S1, "a", again, 4 * MIN + 30);
    // Host 2 cannot read the drop yet: S2 starts, drops and restarts there.
    let unaware = world.start(S2, "a", 5 * MIN, &[]);
    world.status(S2, "a", Some(unaware), "dropped", 5 * MIN + 30);
    let restart = world.start(S2, "a", 6 * MIN, &[unaware, word]);
    world.pulse(S2, "a", restart, 7 * MIN);
    let state = assert_any_arrival_order(&world.events, 10 * MIN);
    assert_nothing_held(&state, "restart on the older host");
    // It follows the drop through the word it cites: the step is open.
    assert_eq!(step(&state, "a").progress, Progress::Doing);
    assert_eq!(step(&state, "a").attempt, Some(id(restart)));
    assert!(conflict_rows(&state).is_empty());
}

/// Review finding 2: two drops by the owner, then a restart.
#[test]
fn a_restart_after_two_owner_drops_reopens_the_step() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", MIN, &[]);
    let first = world.owner_drop("a", 10 * MIN);
    let second = world.owner_drop_from(OPERATOR_3, "a", 11 * MIN);
    // Which drop is shown does not depend on when an unaware start falls.
    for unaware_at in [8 * MIN, 14 * MIN] {
        let mut late = world.clone();
        late.start(S2, "a", unaware_at, &[]);
        let state = assert_any_arrival_order(&late.events, HOUR);
        assert_dropped_by_owner(&state, "a", first, "two drops and an unaware start");
        assert_eq!(step(&state, "a").attempt, Some(id(attempt)));
    }
    let again = world.start(S1, "a", 20 * MIN, &[attempt, first, second]);
    let state = assert_any_arrival_order(&world.events, 25 * MIN);
    assert_nothing_held(&state, "restart after two drops");
    assert_eq!(step(&state, "a").progress, Progress::Doing);
    assert_eq!(step(&state, "a").attempt, Some(id(again)));
    assert!(step(&state, "a").owner_drop.is_none());
}

/// Review finding 4: a start that had not seen the drop, beside the attempt
/// the owner dropped.
#[test]
fn an_unaware_start_neither_takes_the_step_over_nor_asks_who_continues() {
    let mut shown = Vec::new();
    for (label, unaware_at) in [("start before", 8 * MIN), ("start after", 12 * MIN)] {
        let mut world = World::new();
        let attempt = world.start(S1, "a", MIN, &[]);
        let drop = world.owner_drop("a", 10 * MIN);
        world.start(S2, "a", unaware_at, &[]);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_nothing_held(&state, label);
        assert_dropped_by_owner(&state, "a", drop, label);
        assert!(
            conflict_rows(&state).is_empty(),
            "{label}: {:?}",
            conflict_rows(&state)
        );
        assert_eq!(step(&state, "a").attempt, Some(id(attempt)), "{label}");
        assert_eq!(step(&state, "a").session, Some(session(S1)), "{label}");
        let debt = owed(
            project(&state),
            &world.events,
            &session(S1),
            at(HOUR),
            &BTreeMap::new(),
        );
        assert_eq!(debt.dropped.len(), 1, "{label}: S1 is still told");
        shown.push((step(&state, "a").clone(), project(&state).needs_you.clone()));
    }
    assert_eq!(shown[0], shown[1], "same step and agenda in both orders");

    // A restart that its agent then dropped stays what the step shows.
    for unaware_at in [8 * MIN, 25 * MIN] {
        let mut world = World::new();
        let attempt = world.start(S1, "a", MIN, &[]);
        let drop = world.owner_drop("a", 10 * MIN);
        let again = world.start(S1, "a", 20 * MIN, &[attempt, drop]);
        world.status(S1, "a", Some(again), "dropped", 22 * MIN);
        world.start(S2, "a", unaware_at, &[]);
        let state = assert_any_arrival_order(&world.events, HOUR);
        assert_nothing_held(&state, "unaware start after a restart");
        let feature = step(&state, "a");
        assert_eq!(feature.progress, Progress::Dropped);
        assert_eq!(feature.attempt, Some(id(again)), "start at {unaware_at}");
        assert!(feature.owner_drop.is_none(), "its agent dropped it");
        assert!(conflict_rows(&state).is_empty());
    }
}

/// Review finding 3: what a report on an attempt the owner closed
/// contributes is the same wherever it falls in time.
#[test]
fn a_report_on_an_attempt_the_owner_closed_counts_the_same_in_both_orders() {
    for status in ["done", "dropped"] {
        let mut shown = Vec::new();
        for report_at in [8 * MIN, 12 * MIN] {
            let mut world = World::new();
            let attempt = world.start(S2, "a", MIN, &[]);
            let ask = world.ask(S2, "a", 2 * MIN);
            let answer = world.push(
                OPERATOR,
                3 * MIN,
                "decision.answer",
                json!({"node": "a", "ask": uid(ask), "causes": [uid(ask)]}),
                json!({"option": "a", "done_when": [{"id": "continues", "text": "It continues."}]}),
            );
            let drop = world.owner_drop("a", 10 * MIN);
            world.push(
                &writer(S2),
                report_at,
                "work.progress",
                json!({"node": "a", "attempt": uid(attempt), "causes": [uid(answer)]}),
                json!({"agent": agent(S2), "status": status, "evidence": []}),
            );
            let state = assert_any_arrival_order(&world.events, HOUR);
            assert_nothing_held(&state, status);
            assert_dropped_by_owner(&state, "a", drop, status);
            let decision = &project(&state).decisions[&id(ask)];
            shown.push((
                codes(&state),
                decision.applied_sessions.clone(),
                decision.lifecycle,
                step(&state, "a").assumptions.clone(),
                step(&state, "a").taints.clone(),
                project(&state).needs_you.len(),
            ));
        }
        assert_eq!(shown[0], shown[1], "{status}: same in both orders");
        assert_eq!(
            shown[0].1,
            vec![session(S2)],
            "{status}: the answer counts as applied"
        );
    }
    // The agent's drop of a step nobody started, beside the owner's.
    let mut shown = Vec::new();
    for agent_at in [8 * MIN, 12 * MIN] {
        let mut world = World::new();
        world.owner_drop("a", 10 * MIN);
        world.status(S2, "a", None, "dropped", agent_at);
        shown.push(codes(&reduce_at(&world.events, HOUR)));
    }
    assert_eq!(shown[0], shown[1], "same diagnostics in both orders");
}

/// Review finding 6: the header statistics after the step was restarted.
#[test]
fn a_done_on_an_attempt_the_owner_dropped_is_never_a_finished_step() {
    for done_at in [5 * MIN, 8 * MIN] {
        let mut world = World::new();
        let attempt = world.start(S1, "a", MIN, &[]);
        let drop = world.owner_drop("a", 6 * MIN);
        world.status(S1, "a", Some(attempt), "done", done_at);
        let count = |world: &World| {
            let mut events = world.events.clone();
            events.sort_by_key(|event| (event.ts, event.id.clone()));
            let state = reduce_at(&events, HOUR);
            agendar_core::reduce::stats::build(project(&state), &events, at(HOUR)).finished_24h
        };
        assert_eq!(count(&world), 0, "dropped, done at {done_at}");
        let again = world.start(S1, "a", 10 * MIN, &[attempt, drop]);
        assert_eq!(count(&world), 0, "restarted, done at {done_at}");
        world.status(S1, "a", Some(again), "done", 20 * MIN);
        assert_eq!(count(&world), 1, "only the restart's own done counts");
    }
    // Finished and started again before the owner's drop: that work stays
    // finished history; only the attempt the owner dropped does not count.
    let mut world = World::new();
    let first = world.start(S1, "a", MIN, &[]);
    world.status(S1, "a", Some(first), "done", 5 * MIN);
    let second = world.start(S1, "a", 10 * MIN, &[first]);
    let drop = world.owner_drop("a", 12 * MIN);
    world.status(S1, "a", Some(second), "done", 15 * MIN);
    let state = assert_any_arrival_order(&world.events, HOUR);
    assert_dropped_by_owner(&state, "a", drop, "restarted, then dropped");
    let stats = agendar_core::reduce::stats::build(project(&state), &world.events, at(HOUR));
    assert_eq!(stats.finished_24h, 1);
}

// ---------------------------------------------------------------------
// The service call
// ---------------------------------------------------------------------

struct Host {
    service: Service,
    vault: PathBuf,
}

fn init(root: &Path, name: &str, manifest_from: Option<&Path>) -> Host {
    let vault = root.join(format!("vault-{name}"));
    if let Some(from) = manifest_from {
        fs::create_dir_all(vault.join("projects")).expect("projects");
        fs::copy(from.join("vault.json"), vault.join("vault.json")).expect("manifest");
    }
    let (service, _) = Service::init(&vault, &root.join(format!("data-{name}"))).expect("init");
    Host { service, vault }
}

fn files(root: &Path, current: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
}

/// Copy every Vault event file from one copy to another in the given order.
fn sync(from: &Path, to: &Path, reverse: bool) {
    let mut paths = Vec::new();
    files(from, &from.join("projects"), &mut paths);
    paths.sort();
    if reverse {
        paths.reverse();
    }
    for relative in paths {
        let target = to.join(&relative);
        fs::create_dir_all(target.parent().expect("parent")).expect("dir");
        fs::copy(from.join(&relative), &target).expect("copy");
    }
}

fn register(service: &Service, repo: &Path, name: &str) -> SessionId {
    let pid = std::process::id();
    register_session(
        service,
        SessionRegistration {
            project: slug(),
            harness: Harness::Generic,
            harness_session_id: name.to_owned(),
            pid,
            process_start: process_identity(pid).expect("process identity"),
            repo_root: repo.to_path_buf(),
        },
    )
    .expect("register")
}

fn declare(service: &Service, by: &SessionId, steps: &[&str]) {
    let patch: ProjectDeclared = serde_json::from_value(json!({
        "title": "Drop", "brief": "The owner drops steps.", "goal": "A true board."
    }))
    .expect("project");
    service
        .declare_project(&slug(), by, patch, None)
        .expect("declare project");
    for name in steps {
        let patch: PlanFeature = serde_json::from_value(json!({
            "title": name, "slice": "core", "demo_kind": "cli_transcript"
        }))
        .expect("feature");
        service
            .declare_feature(&slug(), by, node(name), patch, None)
            .expect("declare feature");
    }
}

fn eta() -> EtaRange {
    EtaRange {
        min_s: Seconds::new(600).expect("min"),
        max_s: Seconds::new(1_200).expect("max"),
    }
}

fn start(service: &Service, by: &SessionId, name: &str) -> EventId {
    let started = service
        .progress_started(&slug(), by, &node(name), eta(), None, None)
        .expect("start");
    EventId::parse(started["attempt"].as_str().expect("attempt")).expect("attempt id")
}

fn events(service: &Service) -> Vec<Event> {
    service
        .refresh(SystemClock.now(), false)
        .expect("refresh")
        .events
}

fn feature(service: &Service, name: &str) -> FeatureState {
    let state = service.export_state(SystemClock.now()).expect("state");
    state.projects[&slug()].features[&node(name)].clone()
}

#[test]
fn one_call_drops_a_list_and_writes_nothing_when_one_node_is_refused() {
    let temp = tempfile::tempdir().expect("temp");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let host = init(temp.path(), "a", None);
    let service = &host.service;
    let mine = register(service, &repo, "agent-one");
    let other = register(service, &repo, "agent-two");
    declare(
        service,
        &mine,
        &[
            "plan/idle",
            "plan/held",
            "plan/done",
            "plan/gone",
            "plan/keep",
        ],
    );
    let held = start(service, &mine, "plan/held");
    let done = start(service, &mine, "plan/done");
    service
        .progress_finished(
            &slug(),
            &mine,
            &node("plan/done"),
            &done,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    service
        .drop_unstarted(&slug(), &mine, &node("plan/gone"), None, None)
        .expect("agent drop");

    // One bad node refuses the whole list, before anything is written.
    let before = events(service).len();
    let refused = |nodes: &[&str]| {
        let nodes: Vec<NodePath> = nodes.iter().map(|name| node(name)).collect();
        service
            .drop_steps(&slug(), &nodes, Some("Tidy up"))
            .expect_err("refused")
    };
    let unknown = refused(&["plan/idle", "plan/nowhere"]);
    assert_eq!(unknown.code(), ErrorCode::NotFound);
    assert_eq!(unknown.detail(), Some("feature_not_found"));
    let dropped = refused(&["plan/idle", "plan/held", "plan/gone"]);
    assert_eq!(dropped.code(), ErrorCode::Validation);
    assert_eq!(dropped.detail(), Some("already_dropped"));
    assert_eq!(dropped.field(), Some("/nodes/2"));
    let finished = refused(&["plan/done", "plan/idle"]);
    assert_eq!(finished.detail(), Some("already_finished"));
    assert_eq!(finished.field(), Some("/nodes/0"));
    assert_eq!(
        refused(&["plan/idle", "plan/idle"]).detail(),
        Some("duplicate_node")
    );
    assert_eq!(refused(&[]).detail(), Some("nodes_required"));
    let long = service
        .drop_steps(&slug(), &[node("plan/idle")], Some(&"n".repeat(400)))
        .expect_err("note limit");
    assert_eq!(long.code(), ErrorCode::Validation);
    assert_eq!(events(service).len(), before, "nothing was written");
    assert_eq!(feature(service, "plan/idle").progress, Progress::Planned);

    // The list: one operator line per step.
    let ids = service
        .drop_steps(
            &slug(),
            &[node("plan/idle"), node("plan/held")],
            Some("  Tidy up  "),
        )
        .expect("drop");
    assert_eq!(ids.len(), 2);
    let all = events(service);
    assert_eq!(all.len(), before + 2);
    for (id, name) in ids.iter().zip(["plan/idle", "plan/held"]) {
        let event = all.iter().find(|event| &event.id == id).expect("written");
        assert_eq!(event.kind(), "plan.dropped");
        assert!(event.writer.is_operator());
        let value = serde_json::to_value(event).expect("event");
        assert_eq!(value["refs"], json!({"node": name}));
        assert_eq!(value["body"], json!({"note": "Tidy up"}));
        let state = feature(service, name);
        assert_eq!(state.progress, Progress::Dropped);
        assert_eq!(state.owner_drop.expect("owner").event, *id);
    }
    assert_eq!(feature(service, "plan/held").attempt, Some(held.clone()));
    assert!(feature(service, "plan/gone").owner_drop.is_none());
    assert_eq!(
        refused(&["plan/keep", "plan/held"]).detail(),
        Some("already_dropped")
    );

    // The board and `settle` say who dropped it; the nudge is shown once.
    let board = service
        .board(&slug(), Some(&mine), false, &BoardView::All)
        .expect("board");
    let on_board = |name: &str| {
        board["features"]
            .as_array()
            .expect("features")
            .iter()
            .find(|step| step["node"] == name)
            .cloned()
            .expect("step")
    };
    assert_eq!(on_board("plan/held")["progress"], "dropped");
    assert_eq!(on_board("plan/held")["dropped_by"], "owner");
    assert_eq!(on_board("plan/held")["drop_note"], "Tidy up");
    assert!(on_board("plan/gone").get("dropped_by").is_none());
    let page = service
        .board(&slug(), Some(&mine), false, &BoardView::Milestone(None))
        .expect("milestone page");
    let short = page["features"]
        .as_array()
        .expect("features")
        .iter()
        .find(|step| step["node"] == "plan/idle")
        .expect("step");
    assert_eq!(short["dropped_by"], "owner");
    let (settle, _) = service.settle(&slug(), &mine, false).expect("settle");
    let told = settle["dropped_by_owner"].as_array().expect("told");
    assert_eq!(told.len(), 1, "only the step this agent was on");
    assert_eq!(told[0]["node"], "plan/held");
    assert!(
        told[0]["say"]
            .as_str()
            .expect("say")
            .starts_with("plan/held: dropped by the owner"),
        "{told:?}"
    );
    assert!(settle.get("yours").is_none(), "no longer its debt");
    let (settle, _) = service.settle(&slug(), &other, false).expect("settle");
    assert!(settle.get("dropped_by_owner").is_none());
    let (nudges, cursor) = service.plan_nudges(&slug(), &mine, false).expect("nudges");
    let texts: Vec<&str> = nudges.iter().map(|nudge| nudge.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "The owner dropped plan/idle (Tidy up). Stop work on it; read board.",
            "The owner dropped plan/held (Tidy up). Stop work on it; read board.",
        ]
    );
    service
        .mark_plan_seen(&mine, &cursor.expect("cursor"))
        .expect("seen");
    let (nudges, _) = service.plan_nudges(&slug(), &mine, false).expect("nudges");
    assert!(nudges.is_empty(), "told once");

    // Starting again follows the rules after an agent's drop: only the
    // session whose attempt was dropped may restart it, and a step nobody
    // started is not startable. Its start cites the owner's drop.
    for (who, name) in [(&other, "plan/held"), (&mine, "plan/idle")] {
        let error = service
            .progress_started(&slug(), who, &node(name), eta(), None, None)
            .expect_err("not startable");
        assert_eq!(error.code(), ErrorCode::Policy);
        assert_eq!(error.detail(), Some("not_runnable"));
    }
    let again = start(service, &mine, "plan/held");
    let state = feature(service, "plan/held");
    assert_eq!(state.progress, Progress::Doing);
    assert_eq!(state.attempt, Some(again.clone()));
    assert!(state.owner_drop.is_none());
    let restart = events(service)
        .into_iter()
        .find(|event| event.id == again)
        .expect("restart");
    assert_eq!(restart.refs.causes, vec![held, ids[1].clone()]);
}

/// The owner drops the same step at two computers; the agent's host receives
/// both. Its restart cites both drops and reopens the step on every host.
#[test]
fn a_restart_through_the_service_follows_every_drop_it_has_seen() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let agent_host = init(root, "b", None);
    let mine = register(&agent_host.service, &repo, "agent-one");
    declare(&agent_host.service, &mine, &["core/wake"]);
    let attempt = start(&agent_host.service, &mine, "core/wake");
    let mut drops = Vec::new();
    let mut owners = Vec::new();
    for name in ["a", "c"] {
        let owner = init(root, name, Some(&agent_host.vault));
        sync(&agent_host.vault, &owner.vault, false);
        drops.extend(
            owner
                .service
                .drop_steps(&slug(), &[node("core/wake")], None)
                .expect("drop"),
        );
        owners.push(owner);
    }
    for owner in &owners {
        sync(&owner.vault, &agent_host.vault, false);
    }
    assert_eq!(
        feature(&agent_host.service, "core/wake").progress,
        Progress::Dropped
    );
    let again = start(&agent_host.service, &mine, "core/wake");
    let restart = events(&agent_host.service)
        .into_iter()
        .find(|event| event.id == again)
        .expect("restart");
    assert!(restart.refs.causes.contains(&attempt));
    for drop in &drops {
        assert!(restart.refs.causes.contains(drop), "cites {drop}");
    }
    // The agent host holds every file in full: copy from it last.
    for owner in &owners {
        sync(&agent_host.vault, &owner.vault, true);
    }
    for service in [&agent_host.service, &owners[0].service, &owners[1].service] {
        let state = feature(service, "core/wake");
        assert_eq!(state.progress, Progress::Doing);
        assert_eq!(state.attempt, Some(again.clone()));
        assert!(state.owner_drop.is_none());
    }
}

/// Three copies of one Vault. The owner drops the step at host A while the
/// agent at host B, which has not received the drop, reports on its attempt.
/// Whatever order the files arrive in, every host shows the owner's drop.
#[test]
fn hosts_converge_on_the_owners_drop_in_any_arrival_order() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let agent_host = init(root, "b", None);
    let mine = register(&agent_host.service, &repo, "agent-one");
    declare(&agent_host.service, &mine, &["core/wake", "core/other"]);
    let attempt = start(&agent_host.service, &mine, "core/wake");

    let owner_host = init(root, "a", Some(&agent_host.vault));
    sync(&agent_host.vault, &owner_host.vault, false);
    let drop = owner_host
        .service
        .drop_steps(&slug(), &[node("core/wake")], None)
        .expect("drop")
        .remove(0);

    // Host B goes on without the drop: a pulse, then done.
    agent_host
        .service
        .pulse(
            &slug(),
            &mine,
            &node("core/wake"),
            &attempt,
            "Almost there".to_owned(),
            true,
            Some(eta()),
            None,
        )
        .expect("pulse");
    agent_host
        .service
        .progress_finished(
            &slug(),
            &mine,
            &node("core/wake"),
            &attempt,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    assert_eq!(
        feature(&agent_host.service, "core/wake").progress,
        Progress::Done,
        "host B has not received the drop"
    );

    // A third copy receives B's files first, then A's, in reverse order.
    let third = init(root, "c", Some(&agent_host.vault));
    sync(&agent_host.vault, &third.vault, true);
    assert_eq!(
        feature(&third.service, "core/wake").progress,
        Progress::Done
    );
    sync(&owner_host.vault, &third.vault, true);
    sync(&agent_host.vault, &owner_host.vault, false);
    sync(&owner_host.vault, &agent_host.vault, false);

    let now = SystemClock.now();
    let states: Vec<State> = [&owner_host, &agent_host, &third]
        .iter()
        .map(|host| host.service.export_state(now).expect("state"))
        .collect();
    assert_eq!(states[0], states[1], "owner host and agent host");
    assert_eq!(states[0], states[2], "third host");
    let shown = &states[0].projects[&slug()].features[&node("core/wake")];
    assert_eq!(shown.progress, Progress::Dropped);
    assert_eq!(shown.owner_drop.as_ref().expect("owner").event, drop);
    assert_eq!(shown.attempt, Some(attempt));
    // Host B's agent is told on its own host.
    let (settle, _) = agent_host
        .service
        .settle(&slug(), &mine, false)
        .expect("settle");
    assert_eq!(settle["dropped_by_owner"][0]["node"], "core/wake");
}

/// Prints the demo transcript:
/// `cargo test -p agendar-core --test owner_drop demo_transcript -- --ignored --nocapture`.
/// With `AGENDAR_OD_DEMO_DIR` set, the Vault is left in that directory.
#[test]
#[ignore = "demo transcript"]
fn demo_transcript() {
    let temp = tempfile::tempdir().expect("temp");
    let root = std::env::var_os("AGENDAR_OD_DEMO_DIR")
        .map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let host = init(&root, "owner", None);
    let service = &host.service;
    let one = register(service, &repo, "agent-one");
    let two = register(service, &repo, "agent-two");
    declare(
        service,
        &one,
        &["plan/idle", "plan/held", "plan/done", "plan/keep"],
    );
    let held = start(service, &one, "plan/held");
    let done = start(service, &two, "plan/done");
    service
        .progress_finished(
            &slug(),
            &two,
            &node("plan/done"),
            &done,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    let board = |label: &str| {
        let board = service
            .board(&slug(), Some(&one), false, &BoardView::All)
            .expect("board");
        println!("\n$ board --all ({label})");
        for step in board["features"].as_array().expect("features") {
            let mut line = format!("  {} {}", step["node"], step["progress"]);
            if let Some(by) = step.get("dropped_by") {
                line.push_str(&format!(" dropped_by={by}"));
            }
            if let Some(note) = step.get("drop_note") {
                line.push_str(&format!(" drop_note={note}"));
            }
            println!("{line}");
        }
    };
    println!("temp Vault: 4 steps; agent-one holds plan/held, agent-two finished plan/done");
    board("before");

    println!("\n$ drop_steps [plan/idle, plan/done]   (one bad node)");
    let before = events(service).len();
    let error = service
        .drop_steps(
            &slug(),
            &[node("plan/idle"), node("plan/done")],
            Some("Not needed after the redesign"),
        )
        .expect_err("refused");
    println!(
        "  refused: {} {:?} at {:?}: {}",
        error.code().as_str(),
        error.detail(),
        error.field(),
        error.message()
    );
    println!("  events written: {}", events(service).len() - before);

    println!("\n$ drop_steps [plan/idle, plan/held] note=\"Not needed after the redesign\"");
    let ids = service
        .drop_steps(
            &slug(),
            &[node("plan/idle"), node("plan/held")],
            Some("Not needed after the redesign"),
        )
        .expect("drop");
    println!("  event_ids: {ids:?}");
    println!("  Vault lines (operator file):");
    let mut paths = Vec::new();
    files(&host.vault, &host.vault.join("projects"), &mut paths);
    for path in paths {
        if !path.to_string_lossy().ends_with("operator.jsonl") {
            continue;
        }
        let text = fs::read_to_string(host.vault.join(&path)).expect("operator file");
        for line in text.lines().filter(|line| line.contains("plan.dropped")) {
            println!("  {line}");
        }
    }
    board("after");
    let state = feature(service, "plan/held");
    println!(
        "\nstate JSON of plan/held: progress={:?} attempt={} owner_drop={}",
        state.progress,
        state.attempt.as_ref().expect("attempt"),
        serde_json::to_string(&state.owner_drop).expect("owner drop")
    );

    println!("\n$ settle   (agent-one, who held plan/held)");
    let (settle, _) = service.settle(&slug(), &one, false).expect("settle");
    println!("  {settle}");
    println!("\n$ plan nudges for agent-one (shown once)");
    let (nudges, _) = service.plan_nudges(&slug(), &one, false).expect("nudges");
    for nudge in nudges {
        println!("  {}", nudge.text);
    }

    println!("\n$ progress started plan/held   (agent-two: not its attempt)");
    let error = service
        .progress_started(&slug(), &two, &node("plan/held"), eta(), None, None)
        .expect_err("not runnable");
    println!("  refused: {} {:?}", error.code().as_str(), error.detail());
    println!("$ progress started plan/held   (agent-one: its own dropped attempt)");
    let again = start(service, &one, "plan/held");
    let restart = events(service)
        .into_iter()
        .find(|event| event.id == again)
        .expect("restart");
    println!(
        "  started {again}; refs.causes = {:?} (the old attempt {held}, the owner's drop)",
        restart.refs.causes
    );
    let state = feature(service, "plan/held");
    println!(
        "  plan/held: progress={:?} owner_drop={:?}",
        state.progress, state.owner_drop
    );
    println!("$ drop_steps [plan/held]   (the owner drops it again)");
    service
        .drop_steps(&slug(), &[node("plan/held")], None)
        .expect("drop again");
    board("final");
}
