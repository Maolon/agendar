//! Derived settle: an accepted delivery closes its step with no agent action,
//! and a step in review is never late. Built from explicit events in memory;
//! the approved seeds stay untouched.

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{
    Event, EventId, Gate, NodePath, Phase, Progress, ProjectSlug, ProjectState, State, UtcTime,
};
use agendar_core::reduce::{ReduceContext, reduce, reduce_with_report};
use serde_json::{Value, json};

const P: &str = "p";
const SESSION: &str = "00000000000000000000000101";
const AGENT: &str = "h-00000000000000000000000001/s-00000000000000000000000101";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const OPERATOR2: &str = "h-00000000000000000000000002/operator";

fn at(minute: u64) -> UtcTime {
    UtcTime::parse(&format!(
        "2026-01-01T{:02}:{:02}:00.000Z",
        minute / 60,
        minute % 60
    ))
    .expect("timestamp")
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

fn reduce_at(events: &[Event], minute: u64) -> State {
    reduce(events, &ReduceContext { as_of: at(minute) })
}

fn project(state: &State) -> &ProjectState {
    &state.projects[&ProjectSlug::parse(P).expect("slug")]
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with step `x` and its dependent `y`.
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
            json!({"title": "Settle", "brief": "b", "goal": "g"}),
        );
        for (name, depends) in [("x", json!([])), ("y", json!(["x"]))] {
            world.push(
                OPERATOR,
                0,
                "plan.feature",
                json!({"node": name}),
                json!({"title": name, "slice": "s", "demo_kind": "cli_transcript", "depends_on": depends}),
            );
        }
        world
    }

    fn push(&mut self, writer: &str, minute: u64, kind: &str, refs: Value, body: Value) -> u64 {
        let event = self.next_id;
        self.next_id += 1;
        let seq = self.seq.entry(writer.to_owned()).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1, "id": uid(event), "seq": *seq, "writer": writer, "ts": at(minute),
            "project": P, "refs": refs, "kind": kind, "body": body,
        });
        self.events
            .push(support::parse_value(&value).expect("test event decodes"));
        event
    }

    fn agent(&self) -> Value {
        json!({"session": SESSION, "harness": "generic"})
    }

    fn start(&mut self, minute: u64, causes: &[u64]) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        let agent = self.agent();
        self.push(
            AGENT,
            minute,
            "work.progress",
            json!({"node": "x", "causes": causes}),
            json!({"agent": agent, "status": "started", "eta": {"min_s": 600, "max_s": 1200}, "evidence": []}),
        )
    }

    fn status(&mut self, attempt: u64, status: &str, minute: u64) -> u64 {
        let agent = self.agent();
        self.push(
            AGENT,
            minute,
            "work.progress",
            json!({"node": "x", "attempt": uid(attempt)}),
            json!({"agent": agent, "status": status, "evidence": []}),
        )
    }

    fn pulse(&mut self, attempt: u64, eta: bool, minute: u64) -> u64 {
        let mut body = json!({"agent": self.agent(), "message": "note", "boundary": false});
        if eta {
            body["eta"] = json!({"min_s": 600, "max_s": 1200});
        }
        self.push(
            AGENT,
            minute,
            "pulse",
            json!({"node": "x", "attempt": uid(attempt)}),
            body,
        )
    }

    fn deliver(&mut self, attempt: u64, receipt: Option<u64>, minute: u64) -> u64 {
        let mut body = support::delivery_body();
        body["agent"] = self.agent();
        let mut refs = json!({"node": "x", "attempt": uid(attempt)});
        match receipt {
            Some(receipt) => {
                refs["receipt"] = json!(uid(receipt));
                body["criteria"][0]["criterion"] = json!("continues");
            }
            None => body["criteria"] = json!([]),
        }
        self.push(AGENT, minute, "delivery", refs, body)
    }

    fn review(
        &mut self,
        writer: &str,
        delivery: u64,
        verdict: &str,
        causes: &[u64],
        minute: u64,
    ) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        let mut body = json!({"verdict": verdict});
        if verdict == "send_back" {
            body["reason"] = json!("not yet");
        }
        self.push(
            writer,
            minute,
            "review.result",
            json!({"node": "x", "delivery": uid(delivery), "causes": causes}),
            body,
        )
    }

    fn ask(&mut self, minute: u64) -> u64 {
        let agent = self.agent();
        self.push(
            AGENT,
            minute,
            "decision.ask",
            json!({"node": "x"}),
            json!({"agent": agent, "card": support::card(), "policy": support::policy_eval()}),
        )
    }

    fn answer(&mut self, ask: u64, option: &str, causes: &[u64], minute: u64) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        self.push(
            OPERATOR,
            minute,
            "decision.answer",
            json!({"node": "x", "ask": uid(ask), "causes": causes}),
            json!({"option": option, "done_when": [{"id": "continues", "text": "It continues."}]}),
        )
    }
}

fn x(state: &State) -> &agendar_core::model::FeatureState {
    &project(state).features[&node("x")]
}

fn y(state: &State) -> &agendar_core::model::FeatureState {
    &project(state).features[&node("y")]
}

#[test]
fn an_accept_with_no_later_agent_event_is_done_and_unblocks_dependents() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    let before = reduce_at(&world.events, 300);
    assert_eq!(x(&before).progress, Progress::Doing);
    assert_eq!(x(&before).phase, Some(Phase::InReview));
    world.review(OPERATOR, delivery, "accept", &[], 200);
    let state = reduce_at(&world.events, 300);
    assert_eq!(x(&state).progress, Progress::Done);
    assert_eq!(x(&state).phase, None);
    let eta = x(&state).eta.as_ref().expect("eta");
    assert_eq!(eta.late_by_s, 0);
    assert_eq!(y(&state).gate, Gate::Open, "{:?}", y(&state).waits_on);
    assert!(y(&state).waits_on.is_empty());
    // The finished step is no review row any more.
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| row.source != id(delivery))
    );
    // The serialized feature has no phase once it is done.
    let json = serde_json::to_value(x(&state)).expect("json");
    assert!(json.get("phase").is_none());
}

#[test]
fn a_message_only_pulse_after_delivering_still_settles_on_accept() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    world.pulse(attempt, false, 6);
    world.review(OPERATOR, delivery, "accept", &[], 30);
    assert_eq!(x(&reduce_at(&world.events, 60)).progress, Progress::Done);
}

#[test]
fn an_eta_pulse_after_delivering_keeps_the_step_doing_until_the_next_accepted_delivery() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let first = world.deliver(attempt, None, 5);
    world.pulse(attempt, true, 6);
    world.review(OPERATOR, first, "accept", &[], 30);
    let state = reduce_at(&world.events, 40);
    assert_eq!(x(&state).progress, Progress::Doing);
    assert_eq!(x(&state).phase, None);
    let second = world.deliver(attempt, None, 50);
    let state = reduce_at(&world.events, 60);
    assert_eq!(x(&state).phase, Some(Phase::InReview));
    world.review(OPERATOR, second, "accept", &[], 70);
    assert_eq!(x(&reduce_at(&world.events, 80)).progress, Progress::Done);
}

#[test]
fn a_re_answered_receipt_reverts_the_derived_done() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let ask = world.ask(2);
    let answer = world.answer(ask, "a", &[], 3);
    let delivery = world.deliver(attempt, Some(answer), 5);
    world.review(OPERATOR, delivery, "accept", &[answer], 10);
    assert_eq!(x(&reduce_at(&world.events, 20)).progress, Progress::Done);
    world.answer(ask, "b", &[answer], 15);
    let state = reduce_at(&world.events, 20);
    assert_eq!(x(&state).progress, Progress::Doing);
    assert_eq!(x(&state).phase, Some(Phase::InReview));
}

#[test]
fn an_accepted_delivery_of_an_old_attempt_never_closes_a_restart() {
    let mut world = World::new();
    let first = world.start(1, &[]);
    let delivery = world.deliver(first, None, 5);
    world.review(OPERATOR, delivery, "accept", &[], 10);
    world.start(20, &[first]);
    let state = reduce_at(&world.events, 30);
    assert_eq!(x(&state).progress, Progress::Doing);
    assert_eq!(x(&state).phase, None);
}

#[test]
fn a_send_back_after_the_derived_done_reopens_the_step() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    let accept = world.review(OPERATOR, delivery, "accept", &[], 10);
    assert_eq!(x(&reduce_at(&world.events, 15)).progress, Progress::Done);
    world.review(OPERATOR, delivery, "send_back", &[accept], 20);
    let state = reduce_at(&world.events, 30);
    assert_eq!(x(&state).progress, Progress::Doing);
    assert_eq!(x(&state).phase, Some(Phase::SentBack));
    assert_ne!(y(&state).gate, Gate::Open);
}

#[test]
fn a_send_back_is_open_and_its_review_wait_is_off_the_clock() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    world.review(OPERATOR, delivery, "send_back", &[], 10);
    let state = reduce_at(&world.events, 20);
    assert_eq!(x(&state).progress, Progress::Doing);
    assert_eq!(x(&state).phase, Some(Phase::SentBack));
    assert_ne!(y(&state).gate, Gate::Open);
    // The review wait is taken out of the clock: anchored at the start, the
    // ten-minute ETA bound moves by the five minutes in review.
    let eta = x(&state).eta.as_ref().expect("eta");
    assert_eq!(eta.paused_s, 300);
    assert!(!eta.paused);
    // Back at work with a new estimate: plain doing again.
    world.pulse(attempt, true, 21);
    assert_eq!(x(&reduce_at(&world.events, 22)).phase, None);
}

#[test]
fn a_step_in_review_is_never_late_and_leaves_work_left() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    world.deliver(attempt, None, 5);
    let state = reduce_at(&world.events, 600);
    let eta = x(&state).eta.as_ref().expect("eta");
    assert_eq!(eta.late_by_s, 0);
    assert!(eta.paused);
    let durations = [600_000, 600_000, 600_000];
    let forecast = agendar_core::reduce::forecast::forecast(project(&state), &durations, at(600))
        .expect("y is still open");
    assert_eq!(forecast.open_steps, 1, "only y is work left");
}

#[test]
fn a_confirmation_after_the_accept_keeps_the_calibration_sample() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 9);
    world.review(OPERATOR, delivery, "accept", &[], 60);
    let (_, without) = reduce_with_report(&world.events, &ReduceContext { as_of: at(120) });
    world.status(attempt, "done", 90);
    let (state, with) = reduce_with_report(&world.events, &ReduceContext { as_of: at(120) });
    assert_eq!(x(&state).progress, Progress::Done);
    let slug = ProjectSlug::parse(P).expect("slug");
    let without = &without.calibration[&slug];
    let with = &with.calibration[&slug];
    assert_eq!(without.samples.len(), 1);
    assert_eq!(
        without.samples[0].actual_ms,
        8 * 60_000,
        "start to delivery"
    );
    assert_eq!(without.samples, with.samples);
    assert_eq!(without.finished_features, with.finished_features);
}

#[test]
fn a_drop_after_the_accept_is_history_only_and_a_drop_before_stays() {
    // An old CLI on another host drops after the owner accepted.
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    world.review(OPERATOR, delivery, "accept", &[], 10);
    let drop = world.status(attempt, "dropped", 20);
    let state = reduce_at(&world.events, 30);
    assert_eq!(x(&state).progress, Progress::Done);
    assert!(
        state
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.event.as_ref() == Some(&id(drop))
                && diagnostic.code.as_deref() == Some("semantic_history"))
    );

    // A drop before the verdict stays a drop, with no review row left.
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    world.status(attempt, "dropped", 8);
    let state = reduce_at(&world.events, 9);
    assert_eq!(x(&state).progress, Progress::Dropped);
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| row.source != id(delivery)),
        "no review row for a dropped attempt"
    );
    world.review(OPERATOR, delivery, "accept", &[], 10);
    assert_eq!(x(&reduce_at(&world.events, 30)).progress, Progress::Dropped);
}

/// Every permutation of `tail` appended to `head` reduces to one state.
fn converges(head: &[Event], tail: &[Event], minute: u64) -> State {
    let mut tail = tail.to_vec();
    let mut states = Vec::new();
    permute(&mut tail, 0, &mut |order| {
        let mut events: Vec<Event> = order.to_vec();
        events.extend_from_slice(head);
        events.reverse();
        states.push(serde_json::to_string(&reduce_at(&events, minute)).expect("json"));
    });
    assert!(states.windows(2).all(|pair| pair[0] == pair[1]));
    serde_json::from_str(&states[0]).expect("state")
}

fn permute(items: &mut Vec<Event>, k: usize, visit: &mut impl FnMut(&[Event])) {
    if k == items.len() {
        visit(items);
        return;
    }
    for index in k..items.len() {
        items.swap(k, index);
        permute(items, k + 1, visit);
        items.swap(k, index);
    }
}

#[test]
fn a_drop_and_a_concurrent_accept_give_one_state_on_every_host() {
    let mut world = World::new();
    let attempt = world.start(1, &[]);
    let delivery = world.deliver(attempt, None, 5);
    let head = world.events.clone();
    // Same minute, two hosts, neither saw the other.
    world.status(attempt, "dropped", 10);
    world.review(OPERATOR2, delivery, "accept", &[], 10);
    let tail = world.events[head.len()..].to_vec();
    let state = converges(&head, &tail, 30);
    assert!(matches!(
        x(&state).progress,
        Progress::Done | Progress::Dropped
    ));
}

#[test]
fn accept_and_pulse_from_two_hosts_give_one_state_in_every_interleaving() {
    for eta in [false, true] {
        let mut world = World::new();
        let attempt = world.start(1, &[]);
        let delivery = world.deliver(attempt, None, 5);
        let head = world.events.clone();
        world.pulse(attempt, eta, 10);
        world.review(OPERATOR2, delivery, "accept", &[], 10);
        world.pulse(attempt, false, 11);
        let tail = world.events[head.len()..].to_vec();
        let state = converges(&head, &tail, 30);
        let expected = if eta { Progress::Doing } else { Progress::Done };
        assert_eq!(x(&state).progress, expected, "eta pulse: {eta}");
    }
}
