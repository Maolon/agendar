//! What an agent session owes the board (`agendar settle` and the debt line),
//! questions on closed steps (they fold after a day and stop holding), and a
//! handed-off step whose question was answered reaching the next agent with
//! the answer to apply. Built from explicit events in memory.

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{
    CLAIM_EXPIRY_S, Event, EventId, FeatureState, Gate, Lease, MOOT_FOLD_S, NeedsYouItem, NodePath,
    ProjectSlug, ProjectState, SessionId, State, UtcTime,
};
use agendar_core::owed::{CANDIDATES, Owed, SHOWN_FOR_S, owed};
use agendar_core::reduce::attention::{
    is_active_need, notification_eligible, runnable_applying, runnable_for, runnable_nodes,
};
use agendar_core::reduce::{ReduceContext, reduce};
use serde_json::{Value, json};

const P: &str = "p";
/// The agent whose debt is read.
const S1: &str = "00000000000000000000000101";
/// Another agent.
const S2: &str = "00000000000000000000000102";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const STEWARD: &str = "h-00000000000000000000000001/steward";
const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
const ETA_MAX_S: i64 = 20 * MIN;

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

fn row(state: &State, ask: u64) -> &NeedsYouItem {
    project(state)
        .needs_you
        .iter()
        .find(|row| row.id == format!("decide:{}", uid(ask)))
        .expect("decide row")
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with no steps; `declare` adds them.
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
            json!({"title": "Owed", "brief": "b", "goal": "g"}),
        );
        for who in [S1, S2] {
            world.push(
                &writer(who),
                0,
                "pulse",
                json!({}),
                json!({"agent": Self::agent(who), "message": "Session started", "boundary": false}),
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

    fn agent(session: &str) -> Value {
        json!({"session": session, "harness": "generic"})
    }

    /// Declare a step, by the owner or by an agent session.
    fn declare(&mut self, by: &str, step: &str, milestone: Option<&str>, s: i64) {
        let mut body = json!({"title": step, "slice": "s", "demo_kind": "cli_transcript"});
        if let Some(milestone) = milestone {
            body["milestone"] = json!(milestone);
        }
        let writer = if by == OPERATOR {
            OPERATOR.to_owned()
        } else {
            body["agent"] = Self::agent(by);
            writer(by)
        };
        self.push(&writer, s, "plan.feature", json!({"node": step}), body);
    }

    fn start(&mut self, session: &str, step: &str, s: i64, causes: &[u64], basis: &[u64]) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        let basis: Vec<String> = basis.iter().map(|cause| uid(*cause)).collect();
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "causes": causes, "basis": basis}),
            json!({"agent": Self::agent(session), "status": "started",
                   "eta": {"min_s": 600, "max_s": ETA_MAX_S}, "evidence": []}),
        )
    }

    fn status(&mut self, session: &str, step: &str, attempt: u64, status: &str, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "attempt": uid(attempt)}),
            json!({"agent": Self::agent(session), "status": status, "evidence": []}),
        )
    }

    fn deliver(&mut self, session: &str, step: &str, attempt: u64, s: i64) -> u64 {
        let mut body = support::delivery_body();
        body["agent"] = Self::agent(session);
        body["criteria"] = json!([]);
        self.push(
            &writer(session),
            s,
            "delivery",
            json!({"node": step, "attempt": uid(attempt)}),
            body,
        )
    }

    fn accept(&mut self, step: &str, delivery: u64, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "review.result",
            json!({"node": step, "delivery": uid(delivery), "causes": [uid(delivery)]}),
            json!({"verdict": "accept"}),
        )
    }

    fn ask(&mut self, session: &str, step: &str, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "decision.ask",
            json!({"node": step}),
            json!({"agent": Self::agent(session), "card": support::card(), "policy": support::policy_eval()}),
        )
    }

    fn answer(&mut self, step: &str, ask: u64, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "decision.answer",
            json!({"node": step, "ask": uid(ask), "causes": [uid(ask)]}),
            json!({"option": "a", "done_when": [{"id": "continues", "text": "It continues."}]}),
        )
    }

    fn ack(&mut self, session: &str, step: &str, ask: u64, answer: u64, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "decision.ack",
            json!({"node": step, "ask": uid(ask), "answer": uid(answer), "causes": [uid(answer)]}),
            json!({"agent": Self::agent(session), "applied_option": "a", "restarted": []}),
        )
    }

    fn lost(&mut self, session: &str, s: i64) -> u64 {
        self.push(
            STEWARD,
            s,
            "session.lost",
            json!({}),
            json!({"session": session, "process_start": 7, "reason": "exited"}),
        )
    }

    fn handoff(&mut self, step: &str, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "plan.feature",
            json!({"node": step}),
            json!({"handoff": true}),
        )
    }

    /// What S1 owes at `s`, with a shown-cursor.
    fn owed_at(&self, s: i64, shown: &BTreeMap<NodePath, UtcTime>) -> Owed {
        let state = reduce_at(&self.events, s);
        owed(project(&state), &self.events, &session(S1), at(s), shown)
    }
}

fn says<T>(items: &[T], say: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|item| say(item).to_owned()).collect()
}

// ---------------------------------------------------------------------
// settle: one line per item, with its default and deadline
// ---------------------------------------------------------------------

#[test]
fn settle_lists_held_steps_with_their_deadline_or_past_eta() {
    let mut world = World::new();
    for name in ["core/a", "core/b", "core/c"] {
        world.declare(OPERATOR, name, None, 0);
    }
    let a = world.start(S1, "core/a", 0, &[], &[]);
    let b = world.start(S1, "core/b", 2 * HOUR + 50 * MIN, &[], &[]);
    // In review: counted, nothing to do.
    let c = world.start(S1, "core/c", 0, &[], &[]);
    world.deliver(S1, "core/c", c, 5 * MIN);

    // core/a is 2h40m past its 20m ETA (stalled, freed 24h after the ETA);
    // core/b is inside its ETA.
    let owed = world.owed_at(3 * HOUR, &BTreeMap::new());
    assert_eq!(
        says(&owed.yours, |step| &step.say),
        [
            format!(
                "core/a: 2h40m past ETA, freed in 21h20m if no word: pulse --node core/a --attempt {} --eta <range>, deliver, or progress done|dropped --node core/a --attempt {}",
                uid(a),
                uid(a)
            ),
            "core/b: ETA in 10m, then freed in 1d0h if no word".to_owned(),
        ]
    );
    assert!(owed.yours[0].past_eta && !owed.yours[1].past_eta);
    assert_eq!(owed.in_review, 1);
    assert_eq!(
        owed.line().as_deref(),
        Some("open: 2 yours (1 past ETA) · agendar settle")
    );

    // A day after core/a's ETA it is free; S1 is told it may keep it.
    let owed = world.owed_at(ETA_MAX_S + CLAIM_EXPIRY_S + HOUR, &BTreeMap::new());
    assert!(
        owed.yours[0]
            .say
            .starts_with("core/a: freed 1h00m ago (no word); open to any agent. Keep it: pulse"),
        "{}",
        owed.yours[0].say
    );
    let _ = b;
}

#[test]
fn settle_offers_free_steps_and_counts_planned_with_five_candidates_in_rotation() {
    let mut world = World::new();
    // S2's abandoned step, free a day after its ETA.
    world.declare(OPERATOR, "core/wake", Some("MVP"), 0);
    world.start(S2, "core/wake", 0, &[], &[]);
    // S1 touched milestone "Next" by declaring one of its steps.
    world.declare(S1, "next/mine", Some("Next"), 10 * MIN);
    // Eight more planned steps, touched at different times; one is the owner's.
    for (index, name) in ["p/1", "p/2", "p/3", "p/4", "p/5", "p/6", "p/7"]
        .iter()
        .enumerate()
    {
        world.declare(S2, name, None, (index as i64) * MIN);
    }
    world.declare(OPERATOR, "next/owners", Some("Next"), 30 * MIN);

    let now = ETA_MAX_S + CLAIM_EXPIRY_S + 2 * HOUR;
    let owed = world.owed_at(now, &BTreeMap::new());
    assert_eq!(
        says(&owed.take, |take| &take.say),
        [
            "you may take core/wake (abandoned 2h00m ago): progress started --node core/wake --eta <range>"
        ]
    );
    assert_eq!(owed.take[0].why, "free");
    assert_eq!(owed.planned, 9);
    // Agent-written steps first, the oldest touch first; the owner's step
    // last (no stronger signal applies here).
    let first: Vec<&str> = owed
        .candidates
        .iter()
        .take(CANDIDATES)
        .map(|candidate| candidate.node.as_str())
        .collect();
    assert_eq!(first, ["p/1", "p/2", "p/3", "p/4", "p/5"]);
    assert!(owed.candidates[8].owners, "the owner declared it");
    assert_eq!(
        owed.candidates[8].say,
        "next/owners (untouched 1d1h): leave, or patch it; owner's step: ask via a card to drop it"
    );
    assert_eq!(
        owed.candidates[7].say,
        "next/mine (untouched 1d2h): leave, patch (feature declare), or progress dropped --node next/mine --note <reason>"
    );
    assert_eq!(
        owed.line().as_deref(),
        Some("open: 1 free to take, 9 planned · agendar settle")
    );

    // The shown five rest for a week: the next settle shows the rest.
    let shown: BTreeMap<NodePath, UtcTime> =
        first.iter().map(|name| (node(name), at(now))).collect();
    let later = world.owed_at(now + DAY, &shown);
    let next: Vec<&str> = later
        .candidates
        .iter()
        .map(|candidate| candidate.node.as_str())
        .collect();
    assert_eq!(next, ["p/6", "p/7", "next/mine", "next/owners"]);
    // After seven days they come back.
    let week = world.owed_at(now + SHOWN_FOR_S, &shown);
    assert_eq!(week.candidates.len(), 9);
}

#[test]
fn candidates_follow_the_q4_signals_then_the_oldest_touch() {
    let mut world = World::new();
    // The owner's step, touched first of all.
    world.declare(OPERATOR, "o/x", None, 0);
    // Agent steps, oldest touch first within a signal class.
    world.declare(S2, "a/old", None, 5 * MIN);
    world.declare(S2, "a/new", None, 10 * MIN);
    // Rest of its milestone closed: S2 declares and finishes m/y, m/x waits.
    world.declare(S2, "m/y", Some("M"), 20 * MIN);
    world.declare(S2, "m/x", Some("M"), 40 * MIN);
    let y = world.start(S2, "m/y", 21 * MIN, &[], &[]);
    world.status(S2, "m/y", y, "done", 22 * MIN);
    // Every step that depends on it is finished.
    world.declare(S2, "d/x", None, 50 * MIN);
    world.push(
        &writer(S2),
        23 * MIN,
        "plan.feature",
        json!({"node": "d/y"}),
        json!({"agent": World::agent(S2), "title": "d/y", "slice": "s",
               "demo_kind": "cli_transcript", "depends_on": ["d/x"]}),
    );
    let d = world.start(S2, "d/y", 24 * MIN, &[], &[]);
    world.status(S2, "d/y", d, "done", 25 * MIN);

    let owed = world.owed_at(2 * HOUR, &BTreeMap::new());
    let order: Vec<&str> = owed
        .candidates
        .iter()
        .map(|candidate| candidate.node.as_str())
        .collect();
    assert_eq!(order, ["d/x", "m/x", "a/old", "a/new", "o/x"]);
}

#[test]
fn settle_is_zero_when_nothing_is_owed_and_review_needs_nothing() {
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    let attempt = world.start(S1, "core/a", 0, &[], &[]);
    let delivery = world.deliver(S1, "core/a", attempt, 5 * MIN);
    let owed = world.owed_at(10 * MIN, &BTreeMap::new());
    assert!(owed.is_zero());
    assert_eq!(owed.in_review, 1);
    assert_eq!(owed.line(), None);
    // Accepted: done, nothing at all.
    world.accept("core/a", delivery, 20 * MIN);
    let owed = world.owed_at(30 * MIN, &BTreeMap::new());
    assert!(owed.is_zero() && owed.in_review == 0);
    assert_eq!(owed.summary(), "open: nothing");
}

// ---------------------------------------------------------------------
// Questions on closed steps
// ---------------------------------------------------------------------

#[test]
fn a_question_on_a_dropped_step_folds_after_a_day_and_settle_says_so_before() {
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    let attempt = world.start(S1, "core/a", 0, &[], &[]);
    let ask = world.ask(S1, "core/a", 5 * MIN);
    world.status(S1, "core/a", attempt, "dropped", HOUR);

    // Inside the window: active, labelled, and the asker sees one line.
    let state = reduce_at(&world.events, 4 * HOUR);
    let decide = row(&state, ask);
    let closed = decide.closed_step.as_ref().expect("closed step");
    assert_eq!(closed.since, at(HOUR));
    assert!(!closed.folded);
    assert!(is_active_need(decide));
    let owed = world.owed_at(4 * HOUR, &BTreeMap::new());
    assert_eq!(
        says(&owed.questions, |moot| &moot.say),
        [format!(
            "your question {} on core/a is on a closed step: withdraw {} --reason <why>, or it folds in 21h00m",
            uid(ask),
            uid(ask)
        )]
    );
    assert_eq!(
        owed.line().as_deref(),
        Some("open: 1 question on a closed step · agendar settle")
    );

    // After T: folded out of the active list, never notified, still there.
    let state = reduce_at(&world.events, HOUR + MOOT_FOLD_S);
    let decide = row(&state, ask);
    assert!(
        decide
            .closed_step
            .as_ref()
            .is_some_and(|closed| closed.folded)
    );
    assert!(!is_active_need(decide));
    assert!(!notification_eligible(decide));
    assert_eq!(project(&state).summary.needs_count, 0);
    assert!(
        world
            .owed_at(HOUR + MOOT_FOLD_S, &BTreeMap::new())
            .questions
            .is_empty()
    );

    // A restart reopens the step: the question is active again.
    world.start(S1, "core/a", HOUR + MOOT_FOLD_S + MIN, &[attempt], &[]);
    let state = reduce_at(&world.events, HOUR + MOOT_FOLD_S + 2 * MIN);
    let decide = row(&state, ask);
    assert!(decide.closed_step.is_none());
    assert!(is_active_need(decide));
}

#[test]
fn only_a_drop_or_the_owners_accept_closes_a_step_for_its_questions() {
    // Done by the agent alone: the question stays active.
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    let attempt = world.start(S1, "core/a", 0, &[], &[]);
    let ask = world.ask(S1, "core/a", 5 * MIN);
    world.status(S1, "core/a", attempt, "done", HOUR);
    let state = reduce_at(&world.events, 3 * DAY);
    assert!(row(&state, ask).closed_step.is_none());
    assert!(is_active_need(row(&state, ask)));

    // Delivered, accepted: folds a day after the accept.
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    let attempt = world.start(S1, "core/a", 0, &[], &[]);
    let ask = world.ask(S1, "core/a", 5 * MIN);
    let delivery = world.deliver(S1, "core/a", attempt, 10 * MIN);
    world.accept("core/a", delivery, 2 * HOUR);
    let state = reduce_at(&world.events, 2 * HOUR + MOOT_FOLD_S);
    let closed = row(&state, ask).closed_step.clone().expect("closed");
    assert_eq!(closed.since, at(2 * HOUR));
    assert!(closed.folded);

    // An answer ends the question as usual: no row at all.
    world.answer("core/a", ask, 3 * DAY);
    let state = reduce_at(&world.events, 3 * DAY + MIN);
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| row.id != format!("decide:{}", uid(ask)))
    );
}

#[test]
fn a_dead_session_with_one_folded_question_frees_its_steps_after_t() {
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    world.declare(OPERATOR, "core/b", None, 0);
    // S1 asks on core/a and goes silent on core/b; two days later it drops
    // core/a and is never heard again.
    let a = world.start(S1, "core/a", 0, &[], &[]);
    world.start(S1, "core/b", 0, &[], &[]);
    let ask = world.ask(S1, "core/a", MIN);
    world.status(S1, "core/a", a, "dropped", 2 * DAY);

    // Before the fold the open question holds core/b for the owner.
    let state = reduce_at(&world.events, 2 * DAY + MOOT_FOLD_S - MIN);
    let claim = step(&state, "core/b").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Held);
    assert_eq!(claim.waiting_on, Some(id(ask)));

    // Folded: it no longer holds; core/b is free at once (its ETA + T is
    // long past) and any other agent may take it.
    let state = reduce_at(&world.events, 2 * DAY + MOOT_FOLD_S);
    let claim = step(&state, "core/b").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Free);
    assert!(runnable_for(&session(S2), &node("core/b"), project(&state)));
}

// ---------------------------------------------------------------------
// A handed-off step whose question was answered
// ---------------------------------------------------------------------

/// S1 asks on core/a, the owner answers, S1 acks and is lost.
fn answered_then_lost() -> (World, u64, u64, u64) {
    let mut world = World::new();
    world.declare(OPERATOR, "core/a", None, 0);
    let attempt = world.start(S1, "core/a", 0, &[], &[]);
    let ask = world.ask(S1, "core/a", MIN);
    let answer = world.answer("core/a", ask, 10 * MIN);
    world.ack(S1, "core/a", ask, answer, 11 * MIN);
    world.lost(S1, 12 * MIN);
    (world, attempt, ask, answer)
}

#[test]
fn a_handed_off_step_with_an_answer_is_runnable_for_a_new_session_that_applies_it() {
    let (mut world, attempt, _, answer) = answered_then_lost();
    let handoff = world.handoff("core/a", 13 * MIN);
    let state = reduce_at(&world.events, 14 * MIN);
    assert_eq!(step(&state, "core/a").gate, Gate::AwaitingDecision);
    // Not in the strict list (the answer is not applied by S2) ...
    assert!(!runnable_nodes(&session(S2), project(&state)).contains(&node("core/a")));
    // ... but runnable once it applies the answer, with the answer named.
    assert_eq!(
        runnable_applying(&session(S2), project(&state)),
        [(node("core/a"), vec![id(answer)])]
    );
    let owed = owed(
        project(&state),
        &world.events,
        &session(S2),
        at(14 * MIN),
        &BTreeMap::new(),
    );
    assert_eq!(
        says(&owed.take, |take| &take.say),
        [format!(
            "you may continue core/a (handed off by the owner); apply its answer: progress started --node core/a --eta <range> --applies {}",
            uid(answer)
        )]
    );
    assert_eq!(owed.take[0].why, "handed_off");

    // S2 starts it citing the hand-off and applying the answer: the gate clears.
    world.start(S2, "core/a", 15 * MIN, &[attempt, handoff], &[answer]);
    let state = reduce_at(&world.events, 16 * MIN);
    let feature = step(&state, "core/a");
    assert_eq!(feature.gate, Gate::Open);
    assert_eq!(feature.session, Some(session(S2)));
    assert!(runnable_applying(&session(S2), project(&state)).is_empty());
}

#[test]
fn a_free_step_with_an_answer_is_offered_the_same_way_without_a_hand_off() {
    // The takeover gate does not block it: once answered, the gate is open;
    // only the unapplied answer kept it off the list.
    let (world, _, _, answer) = answered_then_lost();
    let state = reduce_at(&world.events, 13 * MIN);
    assert_eq!(step(&state, "core/a").gate, Gate::Open);
    assert_eq!(
        runnable_applying(&session(S2), project(&state)),
        [(node("core/a"), vec![id(answer)])]
    );
    let owed = owed(
        project(&state),
        &world.events,
        &session(S2),
        at(13 * MIN),
        &BTreeMap::new(),
    );
    assert_eq!(owed.take[0].why, "free");
    assert_eq!(owed.take[0].applies, [id(answer)]);
}
