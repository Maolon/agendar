//! The `untouched` label: a planned step that only agents wrote, nobody
//! started, with no question on it and no word for 14 days. Each condition
//! alone keeps the step unfolded; all together fold it; the owner's Keep (an
//! operator patch on the step) unfolds it for good; hosts agree in any event
//! order. Built from explicit events in memory.

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{
    Event, FeatureState, NodePath, Progress, ProjectSlug, ProjectState, State, UNTOUCHED_AFTER_S,
    UtcTime, canonical_json,
};
use agendar_core::reduce::{ReduceContext, reduce};
use serde_json::{Value, json};

const P: &str = "p";
const S1: &str = "00000000000000000000000101";
const S2: &str = "00000000000000000000000102";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const DAY: i64 = 24 * 60 * 60;
const STEP: &str = "u/a";

fn at(s: i64) -> UtcTime {
    UtcTime::parse("2026-01-01T00:00:00.000Z")
        .expect("timestamp")
        .add_seconds(s)
}

fn uid(value: u64) -> String {
    format!("{value:026}")
}

fn node(value: &str) -> NodePath {
    NodePath::parse(value).expect("node")
}

fn writer(session: &str) -> String {
    let host = if session == S2 { 2 } else { 1 };
    format!("h-{host:026}/s-{session}")
}

fn reduce_at(events: &[Event], s: i64) -> State {
    reduce(events, &ReduceContext { as_of: at(s) })
}

fn project(state: &State) -> &ProjectState {
    &state.projects[&ProjectSlug::parse(P).expect("slug")]
}

fn step<'a>(state: &'a State, name: &str) -> &'a FeatureState {
    &project(state).features[&node(name)]
}

fn untouched(events: &[Event], s: i64) -> bool {
    step(&reduce_at(events, s), STEP).untouched
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
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
            json!({"title": "U", "brief": "b", "goal": "g"}),
        );
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

    /// An agent declares a step.
    fn declare(&mut self, by: &str, name: &str, s: i64) {
        let body = json!({"title": name, "slice": "s", "demo_kind": "cli_transcript",
                          "agent": agent(by)});
        self.push(&writer(by), s, "plan.feature", json!({"node": name}), body);
    }

    /// An agent patches a field of a step (a word on it).
    fn agent_patch(&mut self, by: &str, name: &str, s: i64) {
        self.push(
            &writer(by),
            s,
            "plan.feature",
            json!({"node": name}),
            json!({"agent": agent(by), "priority": 1}),
        );
    }

    /// The owner's Keep: an operator patch on the step.
    fn keep(&mut self, name: &str, s: i64) {
        self.push(
            OPERATOR,
            s,
            "plan.feature",
            json!({"node": name}),
            json!({"priority": 1}),
        );
    }

    fn start(&mut self, by: &str, name: &str, s: i64) -> u64 {
        self.push(
            &writer(by),
            s,
            "work.progress",
            json!({"node": name}),
            json!({"agent": agent(by), "status": "started",
                   "eta": {"min_s": 600, "max_s": 1200}, "evidence": []}),
        )
    }

    fn ask(&mut self, by: &str, name: &str, s: i64) -> u64 {
        self.push(
            &writer(by),
            s,
            "decision.ask",
            json!({"node": name}),
            json!({"agent": agent(by), "card": support::card(), "policy": support::policy_eval()}),
        )
    }
}

fn agent(session: &str) -> Value {
    json!({"session": session, "harness": "generic"})
}

/// Day 0: an agent declares the step. Nothing else.
fn declared() -> World {
    let mut world = World::new();
    world.declare(S1, STEP, 0);
    world
}

#[test]
fn the_window_is_fourteen_days_of_event_time() {
    assert_eq!(UNTOUCHED_AFTER_S, 14 * DAY);
    let world = declared();
    assert!(!untouched(&world.events, 14 * DAY - 1));
    assert!(untouched(&world.events, 14 * DAY));
    // Still planned and still there: only the label differs.
    let state = reduce_at(&world.events, 30 * DAY);
    assert_eq!(step(&state, STEP).progress, Progress::Planned);
}

#[test]
fn each_condition_alone_keeps_the_step_unfolded() {
    let late = 30 * DAY;

    // The owner wrote it.
    let mut world = World::new();
    world.push(
        OPERATOR,
        0,
        "plan.feature",
        json!({"node": STEP}),
        json!({"title": STEP, "slice": "s", "demo_kind": "cli_transcript"}),
    );
    assert!(!untouched(&world.events, late), "owner declared it");

    // The owner edited an agent's step.
    let mut world = declared();
    world.keep(STEP, DAY);
    assert!(!untouched(&world.events, late), "owner patched it");

    // Started once (even if later dropped).
    let mut world = declared();
    let attempt = world.start(S1, STEP, DAY);
    assert!(!untouched(&world.events, late), "started");
    world.push(
        &writer(S1),
        2 * DAY,
        "work.progress",
        json!({"node": STEP, "attempt": uid(attempt)}),
        json!({"agent": agent(S1), "status": "dropped", "evidence": []}),
    );
    assert!(!untouched(&world.events, late), "started then dropped");

    // An open question on it.
    let mut world = declared();
    world.ask(S1, STEP, DAY);
    assert!(!untouched(&world.events, late), "open question");

    // Touched 13 days before as_of.
    let mut world = declared();
    world.agent_patch(S2, STEP, 2 * DAY);
    assert!(!untouched(&world.events, 15 * DAY), "touched 13 days ago");
    assert!(untouched(&world.events, 16 * DAY));

    // Kept.
    let mut world = declared();
    world.keep(STEP, DAY);
    assert!(!untouched(&world.events, 365 * DAY), "kept");
}

#[test]
fn all_conditions_together_fold_the_step_and_keep_unfolds_it_for_good() {
    let mut world = World::new();
    world.declare(S1, STEP, 0);
    world.agent_patch(S2, STEP, DAY);
    // Another step in the same project is unaffected.
    world.declare(S2, "u/b", 10 * DAY);

    let state = reduce_at(&world.events, 15 * DAY);
    assert!(step(&state, STEP).untouched, "all agents, 14 days");
    assert!(!step(&state, "u/b").untouched, "touched 5 days ago");

    world.keep(STEP, 15 * DAY);
    for days in [15, 16, 40, 400] {
        assert!(!untouched(&world.events, days * DAY), "kept, day {days}");
    }
}

#[test]
fn an_owner_answer_on_the_step_clears_it() {
    let mut world = declared();
    let ask = world.ask(S1, STEP, 0);
    world.push(
        OPERATOR,
        DAY,
        "decision.answer",
        json!({"node": STEP, "ask": uid(ask), "causes": [uid(ask)]}),
        json!({"option": "a", "done_when": [{"id": "x", "text": "It continues."}]}),
    );
    assert!(!untouched(&world.events, 60 * DAY));
}

#[test]
fn two_hosts_agree_in_any_event_order() {
    let mut world = World::new();
    world.declare(S1, STEP, 0);
    world.agent_patch(S2, STEP, DAY);
    world.keep(STEP, 20 * DAY);
    world.declare(S2, "u/b", 2 * DAY);

    let canonical = |events: &[Event], s: i64| {
        canonical_json(&serde_json::to_value(reduce_at(events, s)).expect("state serializes"))
    };
    for s in [16 * DAY, 19 * DAY, 21 * DAY, 90 * DAY] {
        let forward = canonical(&world.events, s);
        let mut reversed = world.events.clone();
        reversed.reverse();
        assert_eq!(forward, canonical(&reversed, s), "reversed, day {s}");
        let mut rotated = world.events.clone();
        rotated.rotate_left(3);
        let mut duplicated = rotated.clone();
        duplicated.extend(world.events.iter().take(4).cloned());
        assert_eq!(forward, canonical(&duplicated, s), "rotated, day {s}");
    }
    // The event set decides, not the event times: with the Keep present the
    // step is never folded, whatever `as_of` is; without it, it is.
    assert!(!untouched(&world.events, 19 * DAY));
    let without_keep: Vec<Event> = world
        .events
        .iter()
        .filter(|event| !event.writer.is_operator() || event.refs.node.is_none())
        .cloned()
        .collect();
    assert!(untouched(&without_keep, 19 * DAY));
}
