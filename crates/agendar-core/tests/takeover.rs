//! Free steps are open to any agent: a start by another session on a step
//! whose claim ended needs no hand-off, a step under any hold is never taken
//! this way, and a takeover that missed the old owner's last word asks the
//! owner who continues, on every host. Built from explicit events in memory.

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{
    CLAIM_EXPIRY_S, Event, FeatureState, Lease, NeedsKind, NodePath, ProjectSlug, ProjectState,
    SessionId, State, UtcTime,
};
use agendar_core::reduce::attention::{is_active_need, is_correction_row, runnable_for};
use agendar_core::reduce::brief;
use agendar_core::reduce::nudges::{NudgeKind, for_session};
use agendar_core::reduce::{ReduceContext, reduce};
use serde_json::{Value, json};

const P: &str = "p";
/// The agent that starts and goes quiet (host 1).
const S1: &str = "00000000000000000000000101";
/// The agent that takes the step over (host 2).
const S2: &str = "00000000000000000000000102";
/// A second taker (host 1).
const S3: &str = "00000000000000000000000103";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const STEWARD: &str = "h-00000000000000000000000001/steward";
const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
const ETA_MAX_S: i64 = 20 * MIN;
/// The instant a start at 0 with no later word is free by silence.
const FREE_AT: i64 = ETA_MAX_S + CLAIM_EXPIRY_S;

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

fn session(value: &str) -> SessionId {
    value.parse().expect("session")
}

/// Each session writes from its own host: S2 on host 2, the others on host 1.
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

fn lease(state: &State, name: &str) -> Option<Lease> {
    step(state, name).claim.as_ref().map(|claim| claim.state)
}

fn may_start(state: &State, who: &str, name: &str) -> bool {
    runnable_for(&session(who), &node(name), project(state))
}

fn conflict_rows(state: &State) -> Vec<String> {
    project(state)
        .needs_you
        .iter()
        .filter(|row| row.id.starts_with("rescue:start-conflict:"))
        .map(|row| row.id.clone())
        .collect()
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with steps `a`, `b` and `c`, and three registered
    /// sessions.
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
            json!({"title": "Takeover", "brief": "b", "goal": "g"}),
        );
        for name in ["a", "b", "c"] {
            world.push(
                OPERATOR,
                0,
                "plan.feature",
                json!({"node": name}),
                json!({"title": name, "slice": "s", "demo_kind": "cli_transcript", "depends_on": []}),
            );
        }
        for who in [S1, S2, S3] {
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

    /// The latest event `session` wrote so far: what a takeover cites so the
    /// old owner's words up to it are in its causal past (the service does
    /// the same).
    fn head(&self, session: &str) -> u64 {
        let writer = writer(session);
        self.events
            .iter()
            .rev()
            .find(|event| event.writer.to_string() == writer)
            .map(|event| event.id.as_str().parse().expect("numeric test id"))
            .expect("the session wrote")
    }

    fn start(&mut self, session: &str, step: &str, s: i64, causes: &[u64]) -> u64 {
        let mut causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        causes.dedup();
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "causes": causes}),
            json!({"agent": Self::agent(session), "status": "started",
                   "eta": {"min_s": 600, "max_s": ETA_MAX_S}, "evidence": []}),
        )
    }

    fn pulse(&mut self, session: &str, step: &str, attempt: u64, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "pulse",
            json!({"node": step, "attempt": uid(attempt)}),
            json!({"agent": Self::agent(session), "message": "still on it", "boundary": false}),
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

    fn ask(&mut self, session: &str, step: &str, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "decision.ask",
            json!({"node": step}),
            json!({"agent": Self::agent(session), "card": support::card(), "policy": support::policy_eval()}),
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

    fn owner_patch(&mut self, step: &str, s: i64, body: Value) -> u64 {
        self.push(OPERATOR, s, "plan.feature", json!({"node": step}), body)
    }
}

/// S1 starts `a` and goes quiet; S2 takes it over once it is free.
fn taken_over() -> (World, u64, u64) {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let head = world.head(S1);
    let new = world.start(S2, "a", FREE_AT + HOUR, &[old, head]);
    (world, old, new)
}

// ---------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------

#[test]
fn a_free_step_is_open_to_any_agent_and_a_held_or_stalled_one_is_not() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    // Held inside its ETA, then stalled: nobody else may start it.
    for (s, expected) in [(10 * MIN, Lease::Held), (3 * HOUR, Lease::Stalled)] {
        let state = reduce_at(&world.events, s);
        assert_eq!(lease(&state, "a"), Some(expected));
        assert!(!may_start(&state, S2, "a"), "{expected:?}");
    }
    // Free: any other live session may start it, with no hand-off.
    let state = reduce_at(&world.events, FREE_AT);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    assert!(may_start(&state, S2, "a"));
    assert!(may_start(&state, S3, "a"));
    assert!(!step(&state, "a").handoff_pending);
}

#[test]
fn a_lost_sessions_steps_are_open_at_once() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.lost(S1, 5 * MIN);
    let state = reduce_at(&world.events, 6 * MIN);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    assert!(may_start(&state, S2, "a"));
}

#[test]
fn a_step_under_any_hold_is_never_taken_without_the_owner() {
    let late = 30 * DAY;
    // An open question on the step: held, never free.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.ask(S1, "a", MIN);
    let state = reduce_at(&world.events, late);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(!may_start(&state, S2, "a"), "question on the step");

    // The session-level hold: its question on another step holds this one.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.ask(S1, "b", MIN);
    let state = reduce_at(&world.events, late);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(!may_start(&state, S2, "a"), "session-level hold");

    // In review: the owner's, no claim.
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    world.deliver(S1, "a", attempt, 5 * MIN);
    let state = reduce_at(&world.events, late);
    assert!(step(&state, "a").claim.is_none());
    assert!(!may_start(&state, S2, "a"), "in review");

    // Paused by the owner: no claim, never startable.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.owner_patch("a", MIN, json!({"paused": true}));
    let state = reduce_at(&world.events, late);
    assert!(step(&state, "a").claim.is_none());
    assert!(!may_start(&state, S2, "a"), "paused");

    // A lost session's step waiting on a decision: free (the process is
    // gone), but the open question still holds it for the owner.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.ask(S1, "a", MIN);
    world.lost(S1, 2 * MIN);
    let state = reduce_at(&world.events, late);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    assert!(!may_start(&state, S2, "a"), "decision on a lost step");

    // Handed off by the owner: works as before (the hand-off admits it).
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.owner_patch("a", MIN, json!({"handoff": true}));
    let state = reduce_at(&world.events, 2 * MIN);
    assert!(may_start(&state, S2, "a"), "hand-off");
}

// ---------------------------------------------------------------------
// The takeover and the returning agent
// ---------------------------------------------------------------------

#[test]
fn a_takeover_that_saw_the_old_owners_words_is_clean_and_tells_the_old_agent_once() {
    let (world, old, new) = taken_over();
    let state = reduce_at(&world.events, FREE_AT + HOUR + 5 * MIN);
    let feature = step(&state, "a");
    assert_eq!(
        feature.attempt.as_ref().map(|id| id.as_str()),
        Some(uid(new).as_str())
    );
    assert_eq!(feature.session.as_ref().map(|s| s.as_str()), Some(S2));
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(feature.correction.is_none());
    let from = feature.taken_from.clone().expect("taken from");
    assert_eq!(from.attempt.as_str(), uid(old));
    assert!(
        conflict_rows(&state).is_empty(),
        "{:?}",
        conflict_rows(&state)
    );
    assert!(project(&state).needs_you.is_empty());
    // The old agent is told why, with the silence reason now reachable.
    let nudges = for_session(project(&state), &session(S1));
    let nudge = nudges
        .iter()
        .find(|nudge| nudge.kind == NudgeKind::TakenOver)
        .expect("taken-over nudge");
    assert!(
        nudge
            .text
            .contains("your ETA had passed and no word came from you for a day")
            && nudge.text.contains(S2),
        "{}",
        nudge.text
    );
    assert!(
        for_session(project(&state), &session(S2))
            .iter()
            .all(|nudge| nudge.kind != NudgeKind::TakenOver)
    );
}

#[test]
fn the_free_row_is_information_not_a_request() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let state = reduce_at(&world.events, FREE_AT);
    let project = project(&state);
    let row = project
        .needs_you
        .iter()
        .find(|row| row.node == Some(node("a")))
        .expect("the free step keeps its row, to hand over or pause");
    assert_eq!(row.id, format!("rescue:silent:{}", uid(attempt)));
    assert_eq!(row.question, "a: abandoned · open to any agent");
    assert!(is_correction_row(row) && !is_active_need(row));
    // Not counted, not in the brief's Needs you.
    assert_eq!(project.summary.needs_count, 0);
    let brief = brief::build(&state, &world.events, None);
    assert!(brief.needs_you.is_empty(), "{:?}", brief.needs_you);
    // The step still shows abandoned with its correction.
    assert!(step(&state, "a").correction.is_some());

    // A lost session's own step row is information too.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.lost(S1, MIN);
    let state = reduce_at(&world.events, 2 * MIN);
    let rows = &self::project(&state).needs_you;
    assert!(
        rows.iter()
            .any(|row| row.id.starts_with("rescue:lost:") && is_correction_row(row)),
        "{rows:?}"
    );
    assert_eq!(self::project(&state).summary.needs_count, 0);
}

// ---------------------------------------------------------------------
// Contested takeovers
// ---------------------------------------------------------------------

/// Every order of `events` that keeps each writer's own order gives the
/// same board: the hosts may receive the files in any order.
fn same_on_every_host(events: &[Event], s: i64) -> String {
    let reference = serde_json::to_string(&reduce_at(events, s)).expect("json");
    let mut rotated = events.to_vec();
    for _ in 0..events.len() {
        rotated.rotate_left(1);
        let other = serde_json::to_string(&reduce_at(&rotated, s)).expect("json");
        assert_eq!(reference, other, "arrival order changed the board");
    }
    let mut reversed = events.to_vec();
    reversed.reverse();
    assert_eq!(
        reference,
        serde_json::to_string(&reduce_at(&reversed, s)).expect("json")
    );
    reference
}

#[test]
fn two_agents_taking_the_same_free_step_give_one_conflict_row_on_every_host() {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let head = world.head(S1);
    // S2 on host 2 and S3 on host 1, neither seeing the other.
    world.start(S2, "a", FREE_AT + HOUR, &[old, head]);
    world.start(S3, "a", FREE_AT + HOUR + 1, &[old, head]);
    let s = FREE_AT + 2 * HOUR;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state).len(),
        1,
        "{:?}",
        conflict_rows(&state)
    );
    let row = project(&state)
        .needs_you
        .iter()
        .find(|row| row.id.starts_with("rescue:start-conflict:"))
        .expect("row");
    assert_eq!(row.question, "Who should continue a?");
    assert_eq!(row.kind, NeedsKind::Rescue);
    assert!(is_active_need(row));
    same_on_every_host(&world.events, s);
    // Each host on its own first: one start, no conflict yet.
    let host2: Vec<Event> = world
        .events
        .iter()
        .filter(|event| event.writer.session().map(|s| s.as_str()) != Some(S3))
        .cloned()
        .collect();
    assert!(conflict_rows(&reduce_at(&host2, s)).is_empty());
}

#[test]
fn a_late_word_from_the_old_owner_makes_the_takeover_contested_on_every_host() {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let seen = world.head(S1);
    // Host 1's agent kept working, but its file had not reached host 2 when
    // host 2 saw the step free and S2 took it.
    world.pulse(S1, "a", old, FREE_AT - HOUR);
    let late = world.events.pop().expect("late pulse");
    world.start(S2, "a", FREE_AT + HOUR, &[old, seen]);
    let s = FREE_AT + 2 * HOUR;
    // Before the late file arrives: a clean takeover.
    assert!(conflict_rows(&reduce_at(&world.events, s)).is_empty());
    // It arrives: not in the takeover's causal past, so the owner decides.
    world.events.push(late);
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state),
        vec![format!("rescue:start-conflict:{}", uid(old))]
    );
    same_on_every_host(&world.events, s);
}

#[test]
fn a_takeover_by_hand_off_is_the_owners_choice_never_contested() {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let handoff = world.owner_patch("a", MIN, json!({"handoff": true}));
    // The old agent's word the taker did not see.
    world.pulse(S1, "a", old, 2 * MIN);
    world.start(S2, "a", 3 * MIN, &[old, handoff]);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(
        conflict_rows(&state).is_empty(),
        "{:?}",
        conflict_rows(&state)
    );
}

// ---------------------------------------------------------------------
// A held step: another agent asks the owner
// ---------------------------------------------------------------------

impl World {
    /// S asks the owner to let it continue `step`, citing what it read: the
    /// attempt and its holder's last word (the service does the same).
    fn ask_takeover(&mut self, session: &str, step: &str, s: i64, read: &[u64]) -> u64 {
        let mut card = support::card();
        card["options"][0]["id"] = json!("takeover");
        card["options"][1]["id"] = json!("leave");
        card["recommended"]["option"] = json!("takeover");
        let causes: Vec<String> = read.iter().map(|cause| uid(*cause)).collect();
        self.push(
            &writer(session),
            s,
            "decision.ask",
            json!({"node": step, "causes": causes}),
            json!({"agent": Self::agent(session), "card": card, "policy": support::policy_eval()}),
        )
    }

    fn owner_answers(&mut self, step: &str, ask: u64, option: &str, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "decision.answer",
            json!({"node": step, "ask": uid(ask), "causes": [uid(ask)]}),
            json!({"option": option, "done_when": [{"id": "continues", "text": "It continues."}]}),
        )
    }

    /// A start by `session` that replaces `old` by applying `answer`.
    fn start_applying(&mut self, session: &str, step: &str, s: i64, old: u64, answer: u64) -> u64 {
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "causes": [uid(old)], "basis": [uid(answer)]}),
            json!({"agent": Self::agent(session), "status": "started",
                   "eta": {"min_s": 600, "max_s": ETA_MAX_S}, "evidence": []}),
        )
    }
}

/// Startable once the answers on the step are applied (`--applies`).
fn may_start_applying(state: &State, who: &str, name: &str) -> bool {
    agendar_core::reduce::attention::runnable_except_receipts(
        &session(who),
        &node(name),
        project(state),
    )
}

#[test]
fn a_held_step_goes_to_the_agent_that_asked_when_the_owner_says_so_on_every_host() {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    // Held: nobody else may start it, with or without an answer to apply.
    let state = reduce_at(&world.events, 5 * MIN);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(!may_start_applying(&state, S2, "a"));

    // S2 asks on the step. While the question is open nobody starts it.
    let ask = world.ask_takeover(S2, "a", 6 * MIN, &[old]);
    let state = reduce_at(&world.events, 7 * MIN);
    assert!(!may_start_applying(&state, S2, "a"));
    assert!(step(&state, "a").takeover_grants.is_empty());

    // The owner lets it: S2 may start by applying the answer; S3 may not.
    let answer = world.owner_answers("a", ask, "takeover", 8 * MIN);
    let state = reduce_at(&world.events, 9 * MIN);
    let grants = &step(&state, "a").takeover_grants;
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(grants[0].session.as_str(), S2);
    assert_eq!(grants[0].answer.as_str(), uid(answer));
    assert!(may_start_applying(&state, S2, "a"));
    assert!(
        !may_start(&state, S2, "a"),
        "the answer is applied by the start"
    );
    assert!(!may_start_applying(&state, S3, "a"));
    // The step is still S1's until S2 starts: its own conversation goes on.
    assert_eq!(
        step(&state, "a").session.as_ref().map(|s| s.as_str()),
        Some(S1)
    );
    same_on_every_host(&world.events, 9 * MIN);

    // S1 wrote a word S2 never saw; the start is the owner's choice, so
    // nobody is asked who continues.
    world.pulse(S1, "a", old, 9 * MIN);
    let new = world.start_applying(S2, "a", 10 * MIN, old, answer);
    let s = 11 * MIN;
    let state = reduce_at(&world.events, s);
    let feature = step(&state, "a");
    assert_eq!(feature.session.as_ref().map(|s| s.as_str()), Some(S2));
    assert_eq!(
        feature.attempt.as_ref().map(|id| id.as_str()),
        Some(uid(new).as_str())
    );
    let from = feature.taken_from.clone().expect("taken from");
    assert_eq!(from.attempt.as_str(), uid(old));
    assert_eq!(
        from.granted_by.as_ref().map(|id| id.as_str()),
        Some(uid(answer).as_str())
    );
    assert!(feature.takeover_grants.is_empty(), "spent by the start");
    assert!(
        conflict_rows(&state).is_empty(),
        "{:?}",
        conflict_rows(&state)
    );
    assert!(project(&state).needs_you.is_empty());
    let nudges = for_session(project(&state), &session(S1));
    let nudge = nudges
        .iter()
        .find(|nudge| nudge.kind == NudgeKind::TakenOver)
        .expect("the old agent is told");
    assert!(
        nudge
            .text
            .contains("the owner let another agent continue it")
            && nudge.text.contains(S2),
        "{}",
        nudge.text
    );
    same_on_every_host(&world.events, s);
}

#[test]
fn only_the_owners_takeover_answer_about_this_attempt_admits_the_asker() {
    // The owner chose to leave it: nothing changes.
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    world.owner_answers("a", ask, "leave", 2 * MIN);
    let state = reduce_at(&world.events, 3 * MIN);
    assert!(step(&state, "a").takeover_grants.is_empty());
    assert!(!may_start_applying(&state, S2, "a"));

    // The holder's own question with such an option lets nobody in.
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S1, "a", MIN, &[old]);
    world.owner_answers("a", ask, "takeover", 2 * MIN);
    let state = reduce_at(&world.events, 3 * MIN);
    assert!(step(&state, "a").takeover_grants.is_empty());
    assert!(!may_start_applying(&state, S2, "a"));

    // A question that did not read the attempt (asked before it started, or
    // without seeing it) does not cover it.
    let mut world = World::new();
    let ask = world.ask_takeover(S2, "a", 0, &[]);
    let answer = world.owner_answers("a", ask, "takeover", MIN);
    world.start(S1, "a", 2 * MIN, &[]);
    let state = reduce_at(&world.events, 3 * MIN);
    assert!(step(&state, "a").takeover_grants.is_empty());
    assert!(!may_start_applying(&state, S2, "a"));
    let _ = answer;

    // The holder restarted the step after the answer: the answer was about
    // the attempt it replaced.
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    world.owner_answers("a", ask, "takeover", 2 * MIN);
    world.start(S1, "a", 3 * MIN, &[old]);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(step(&state, "a").takeover_grants.is_empty());
    assert!(!may_start_applying(&state, S2, "a"));

    // A pause is the owner's later word and still holds.
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    world.owner_answers("a", ask, "takeover", 2 * MIN);
    world.owner_patch("a", 3 * MIN, json!({"paused": true}));
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(!may_start_applying(&state, S2, "a"));
}

#[test]
fn a_stopped_step_is_never_a_needs_you_question() {
    // Held, stalled, free by silence, free by a lost session: no active row,
    // nothing counted, at any time.
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    for s in [10 * MIN, 3 * HOUR, FREE_AT, FREE_AT + 30 * DAY] {
        let state = reduce_at(&world.events, s);
        let project = project(&state);
        assert!(
            project.needs_you.iter().all(|row| !is_active_need(row)),
            "{s}: {:?}",
            project.needs_you
        );
        assert_eq!(project.summary.needs_count, 0, "{s}");
    }
    world.lost(S1, 5 * MIN);
    let state = reduce_at(&world.events, 6 * MIN);
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| !is_active_need(row))
    );
    assert_eq!(project(&state).summary.needs_count, 0);
}

#[test]
fn a_hand_off_recorded_by_an_older_version_still_reduces_and_stays_takeable() {
    // An existing Vault: the owner pushed the step with "Hand to next
    // session". The line is read as before: no claim, no Needs-you row, any
    // other agent starts it by citing the hand-off, on every host.
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    world.lost(S1, MIN);
    let handoff = world.owner_patch("a", 2 * MIN, json!({"handoff": true}));
    let state = reduce_at(&world.events, 30 * DAY);
    let feature = step(&state, "a");
    assert!(feature.handoff_pending);
    assert!(feature.claim.is_none());
    assert!(may_start(&state, S2, "a"));
    assert!(may_start(&state, S3, "a"));
    assert!(
        project(&state).needs_you.is_empty(),
        "{:?}",
        project(&state).needs_you
    );
    same_on_every_host(&world.events, 30 * DAY);

    world.start(S2, "a", 30 * DAY + MIN, &[old, handoff]);
    let s = 30 * DAY + 2 * MIN;
    let state = reduce_at(&world.events, s);
    let feature = step(&state, "a");
    assert_eq!(feature.session.as_ref().map(|s| s.as_str()), Some(S2));
    assert!(!feature.handoff_pending);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(project(&state).needs_you.is_empty());
    same_on_every_host(&world.events, s);
}

// ---------------------------------------------------------------------
// The grant keeps every other hold, follows the owner's current answer, is
// applied by the start itself and survives a session continuation
// ---------------------------------------------------------------------

/// A session on host 1 that continues a lost one.
const S4: &str = "00000000000000000000000104";

impl World {
    /// A world with S4 registered too.
    fn with_s4() -> Self {
        let mut world = Self::new();
        world.push(
            &writer(S4),
            0,
            "pulse",
            json!({}),
            json!({"agent": Self::agent(S4), "message": "Session started", "boundary": false}),
        );
        world
    }

    /// The owner changes their answer: a later answer that follows `previous`.
    fn owner_changes(&mut self, step: &str, ask: u64, previous: u64, option: &str, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "decision.answer",
            json!({"node": step, "ask": uid(ask), "causes": [uid(ask), uid(previous)]}),
            json!({"option": option, "done_when": [{"id": "continues", "text": "It continues."}]}),
        )
    }

    fn withdraw(&mut self, session: &str, step: &str, ask: u64, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "decision.withdrawn",
            json!({"node": step, "ask": uid(ask)}),
            json!({"agent": Self::agent(session), "reason": "No longer needed."}),
        )
    }

    fn continued(&mut self, session: &str, previous: &str, s: i64) -> u64 {
        self.push(
            STEWARD,
            s,
            "session.continued",
            json!({}),
            json!({"session": session, "previous": previous}),
        )
    }

    fn owner_drops(&mut self, step: &str, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "plan.dropped",
            json!({"node": step}),
            json!({"note": "Not needed any more."}),
        )
    }

    /// `done` on `attempt`, citing `basis`.
    fn done_citing(&mut self, session: &str, step: &str, attempt: u64, s: i64, basis: u64) -> u64 {
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "attempt": uid(attempt), "basis": [uid(basis)]}),
            json!({"agent": Self::agent(session), "status": "done", "evidence": []}),
        )
    }

    /// Remove the event `id` (a file that has not arrived yet) and return it.
    fn hold_back(&mut self, id: u64) -> Event {
        let at = self
            .events
            .iter()
            .position(|event| event.id.as_str() == uid(id))
            .expect("event");
        self.events.remove(at)
    }
}

fn grants(state: &State, name: &str) -> Vec<String> {
    step(state, name)
        .takeover_grants
        .iter()
        .map(|grant| grant.session.as_str().to_owned())
        .collect()
}

fn owner_let_nudge(state: &State, who: &str) -> bool {
    for_session(project(state), &session(who))
        .iter()
        .any(|nudge| {
            nudge
                .text
                .contains("the owner let another agent continue it")
        })
}

/// S1 holds `a`, S2 asked to continue it and the owner let it.
fn granted() -> (World, u64, u64, u64) {
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    let answer = world.owner_answers("a", ask, "takeover", 2 * MIN);
    (world, old, ask, answer)
}

#[test]
fn a_grant_never_passes_a_review_a_finished_step_a_pause_a_question_or_a_drop() {
    // The holder delivers before the granted agent starts: the step is in
    // review, the owner's; the grant is gone and the review stays.
    let (mut world, old, _, _) = granted();
    assert_eq!(grants(&reduce_at(&world.events, 3 * MIN), "a"), [S2]);
    let delivery = world.deliver(S1, "a", old, 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "in review");
    assert!(!may_start_applying(&state, S2, "a"), "in review");
    assert!(
        project(&state)
            .needs_you
            .iter()
            .any(|row| row.kind == NeedsKind::Review
                && row.id == format!("review:{}", uid(delivery))),
        "{:?}",
        project(&state).needs_you
    );
    same_on_every_host(&world.events, 4 * MIN);

    // The holder finished it.
    let (mut world, old, _, answer) = granted();
    world.done_citing(S1, "a", old, 3 * MIN, answer);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "done");
    assert!(!may_start_applying(&state, S2, "a"), "done");

    // The owner paused it.
    let (mut world, _, _, _) = granted();
    world.owner_patch("a", 3 * MIN, json!({"paused": true}));
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "paused");
    assert!(!may_start_applying(&state, S2, "a"), "paused");

    // Another question on the step is open.
    let (mut world, _, _, _) = granted();
    world.ask(S1, "a", 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "open question");
    assert!(!may_start_applying(&state, S2, "a"), "open question");

    // The owner dropped it.
    let (mut world, _, _, _) = granted();
    world.owner_drops("a", 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "dropped");
    assert!(!may_start_applying(&state, S2, "a"), "dropped");

    // An old hand-off on the step: the hand-off rule applies, not the grant.
    let (mut world, _, _, _) = granted();
    world.owner_patch("a", 3 * MIN, json!({"handoff": true}));
    assert!(grants(&reduce_at(&world.events, 4 * MIN), "a").is_empty());
}

#[test]
fn a_granted_start_that_missed_the_holders_delivery_asks_who_continues_on_every_host() {
    // Host 1: the holder delivers. Host 2, which has not seen it: the granted
    // agent starts. The owner's answer was not about a delivered step.
    let (mut world, old, _, answer) = granted();
    let delivery = world.deliver(S1, "a", old, 3 * MIN);
    world.start_applying(S2, "a", 4 * MIN, old, answer);
    let s = 5 * MIN;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state),
        vec![format!("rescue:start-conflict:{}", uid(old))]
    );
    assert!(
        project(&state)
            .needs_you
            .iter()
            .any(|row| row.id == format!("review:{}", uid(delivery))),
        "the review stays: {:?}",
        project(&state).needs_you
    );
    same_on_every_host(&world.events, s);
}

#[test]
fn only_the_owners_current_answer_counts_on_every_host() {
    // The owner said takeover, then changed to leave; the holder wrote on.
    // On another host the asker started on the first answer.
    let (mut world, old, ask, first) = granted();
    world.owner_changes("a", ask, first, "leave", 3 * MIN);
    world.pulse(S1, "a", old, 4 * MIN);
    world.start_applying(S2, "a", 5 * MIN, old, first);
    let s = 6 * MIN;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state),
        vec![format!("rescue:start-conflict:{}", uid(old))],
        "contested like any ungranted takeover"
    );
    let from = step(&state, "a").taken_from.clone().expect("taken from");
    assert_eq!(from.granted_by, None);
    assert!(!owner_let_nudge(&state, S1));
    same_on_every_host(&world.events, s);

    // Before the start: the changed answer lets nobody in.
    let (mut world, _, ask, first) = granted();
    world.owner_changes("a", ask, first, "leave", 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty());
    assert!(!may_start_applying(&state, S2, "a"));
    same_on_every_host(&world.events, 4 * MIN);

    // The asker withdrew its question after the answer.
    let (mut world, old, ask, first) = granted();
    world.withdraw(S2, "a", ask, 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty(), "withdrawn");
    assert!(!may_start_applying(&state, S2, "a"), "withdrawn");
    world.pulse(S1, "a", old, 4 * MIN);
    world.start_applying(S2, "a", 5 * MIN, old, first);
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state),
        vec![format!("rescue:start-conflict:{}", uid(old))],
        "withdrawn"
    );
    assert_eq!(
        step(&state, "a")
            .taken_from
            .clone()
            .expect("taken from")
            .granted_by,
        None
    );
    assert!(!owner_let_nudge(&state, S1));
    same_on_every_host(&world.events, s);

    // The question was withdrawn before the owner's answer reached it.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    world.withdraw(S2, "a", ask, 2 * MIN);
    let late = world.owner_answers("a", ask, "takeover", 3 * MIN);
    world.pulse(S1, "a", old, 4 * MIN);
    world.start_applying(S2, "a", 5 * MIN, old, late);
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state).len(),
        1,
        "answer to a withdrawn question"
    );
    assert!(!owner_let_nudge(&state, S1));
    same_on_every_host(&world.events, s);
}

#[test]
fn an_answer_cited_after_the_start_does_not_make_it_the_owners_choice() {
    // S2 took the step when it looked free, without the holder's last word:
    // contested. The owner's answer came later and a later event of the
    // attempt cites it.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let seen = world.head(S1);
    let word = world.pulse(S1, "a", old, FREE_AT - HOUR);
    let late = world.hold_back(word);
    let ask = world.ask_takeover(S2, "a", FREE_AT, &[old]);
    let new = world.start(S2, "a", FREE_AT + HOUR, &[old, seen]);
    world.events.push(late);
    let s = FREE_AT + 3 * HOUR;
    let contested = vec![format!("rescue:start-conflict:{}", uid(old))];
    assert_eq!(conflict_rows(&reduce_at(&world.events, s)), contested);

    let answer = world.owner_answers("a", ask, "takeover", FREE_AT + 2 * HOUR);
    world.pulse(S2, "a", new, FREE_AT + 2 * HOUR + MIN);
    world.done_citing(S2, "a", new, FREE_AT + 2 * HOUR + 2 * MIN, answer);
    let state = reduce_at(&world.events, s);
    assert_eq!(conflict_rows(&state), contested, "still contested");
    assert_eq!(
        step(&state, "a")
            .taken_from
            .clone()
            .map(|from| from.granted_by),
        Some(None)
    );
    assert!(!owner_let_nudge(&state, S1));
    same_on_every_host(&world.events, s);
}

#[test]
fn an_answer_about_a_later_attempt_does_not_cover_an_earlier_takeover() {
    // S3 took `a` from S1 without S1's last word (contested). S2 then asked
    // about S3's attempt and was let in; it started from S3's attempt. The
    // answer says nothing about S1's attempt.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let seen = world.head(S1);
    let word = world.pulse(S1, "a", old, FREE_AT - HOUR);
    let late = world.hold_back(word);
    let third = world.start(S3, "a", FREE_AT + HOUR, &[old, seen]);
    world.events.push(late);
    let ask = world.ask_takeover(S2, "a", FREE_AT + 2 * HOUR, &[third]);
    let answer = world.owner_answers("a", ask, "takeover", FREE_AT + 2 * HOUR + MIN);
    world.start_applying(S2, "a", FREE_AT + 2 * HOUR + 2 * MIN, third, answer);
    let s = FREE_AT + 3 * HOUR;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        conflict_rows(&state),
        vec![format!("rescue:start-conflict:{}", uid(old))]
    );
    assert_eq!(
        step(&state, "a")
            .taken_from
            .clone()
            .and_then(|from| from.granted_by)
            .map(|id| id.as_str().to_owned()),
        Some(uid(answer))
    );
    same_on_every_host(&world.events, s);
}

#[test]
fn the_holders_own_question_is_never_a_grant_after_its_session_is_continued() {
    // S1 holds `a` and asked a question with a `takeover` option itself; the
    // owner chose it. S1 is lost and continued as S4: the same conversation.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S1, "a", MIN, &[old]);
    world.owner_answers("a", ask, "takeover", 2 * MIN);
    world.lost(S1, 3 * MIN);
    world.continued(S4, S1, 4 * MIN);
    let s = 5 * MIN;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        step(&state, "a").session.as_ref().map(|s| s.as_str()),
        Some(S4)
    );
    assert!(grants(&state, "a").is_empty(), "{:?}", grants(&state, "a"));
    for who in [S1, S2, S3, S4] {
        assert!(!may_start_applying(&state, who, "a") || who == S4, "{who}");
    }
    same_on_every_host(&world.events, s);
}

#[test]
fn a_grant_belongs_to_the_session_that_asked_and_its_continuation_asks_again() {
    // S3 asked and was let in, then was lost and continued as S4 before it
    // started. The grant is not inherited: S4 asks again.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S3, "a", MIN, &[old]);
    let answer = world.owner_answers("a", ask, "takeover", 2 * MIN);
    world.lost(S3, 3 * MIN);
    world.continued(S4, S3, 4 * MIN);
    let s = 6 * MIN;
    let state = reduce_at(&world.events, s);
    assert_eq!(grants(&state, "a"), [S3]);
    assert!(
        !may_start_applying(&state, S3, "a"),
        "a lost session starts nothing"
    );
    assert!(!may_start_applying(&state, S4, "a"), "not inherited");
    same_on_every_host(&world.events, s);
    // A start by the continuation on that answer is an ungranted takeover.
    world.pulse(S1, "a", old, 4 * MIN);
    world.start_applying(S4, "a", 5 * MIN, old, answer);
    let state = reduce_at(&world.events, s);
    assert_eq!(conflict_rows(&state).len(), 1);
    same_on_every_host(&world.events, s);

    // The asker started by its grant and was continued afterwards: the start
    // stays the owner's choice.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S3, "a", MIN, &[old]);
    let answer = world.owner_answers("a", ask, "takeover", 2 * MIN);
    world.pulse(S1, "a", old, 3 * MIN);
    world.start_applying(S3, "a", 4 * MIN, old, answer);
    world.lost(S3, 5 * MIN);
    world.continued(S4, S3, 6 * MIN);
    let s = 7 * MIN;
    let state = reduce_at(&world.events, s);
    assert_eq!(
        step(&state, "a").session.as_ref().map(|s| s.as_str()),
        Some(S4)
    );
    assert!(
        conflict_rows(&state).is_empty(),
        "{:?}",
        conflict_rows(&state)
    );
    assert_eq!(
        step(&state, "a")
            .taken_from
            .clone()
            .and_then(|from| from.granted_by)
            .map(|id| id.as_str().to_owned()),
        Some(uid(answer))
    );
    same_on_every_host(&world.events, s);
}

#[test]
fn a_grant_beside_a_free_step_two_grants_and_a_restart() {
    // The step became free while S2 holds a grant: S3 takes it as any free
    // step. S2's grant was about the attempt S3 replaced.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    let answer = world.owner_answers("a", ask, "takeover", 2 * MIN);
    // (The wait on S2's question moved the ETA bound by a minute.)
    let free = FREE_AT + HOUR;
    let state = reduce_at(&world.events, free);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    assert!(may_start_applying(&state, S3, "a"));
    assert!(may_start_applying(&state, S2, "a"));
    let head = world.head(S1);
    world.start(S3, "a", free + MIN, &[old, head]);
    let s = free + 2 * MIN;
    let state = reduce_at(&world.events, s);
    assert!(grants(&state, "a").is_empty());
    assert!(!may_start_applying(&state, S2, "a"));
    assert!(conflict_rows(&state).is_empty());
    same_on_every_host(&world.events, s);
    // Both started without seeing each other: one row asks who continues.
    world.start_applying(S2, "a", free + MIN + 1, old, answer);
    assert_eq!(conflict_rows(&reduce_at(&world.events, s)).len(), 1);
    same_on_every_host(&world.events, s);

    // Two askers, both let in: the first start spends the other's grant; two
    // starts that did not see each other give one row.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask2 = world.ask_takeover(S2, "a", MIN, &[old]);
    let ask3 = world.ask_takeover(S3, "a", MIN + 1, &[old]);
    let answer2 = world.owner_answers("a", ask2, "takeover", 2 * MIN);
    let answer3 = world.owner_answers("a", ask3, "takeover", 2 * MIN + 1);
    let state = reduce_at(&world.events, 3 * MIN);
    assert_eq!(grants(&state, "a"), [S2, S3]);
    world.start_applying(S2, "a", 4 * MIN, old, answer2);
    let state = reduce_at(&world.events, 5 * MIN);
    assert!(grants(&state, "a").is_empty());
    assert!(!may_start_applying(&state, S3, "a"));
    assert!(conflict_rows(&state).is_empty());
    world.start_applying(S3, "a", 4 * MIN + 1, old, answer3);
    assert_eq!(conflict_rows(&reduce_at(&world.events, 5 * MIN)).len(), 1);
    same_on_every_host(&world.events, 5 * MIN);

    // The holder restarted its own attempt before the owner answered: the
    // question was about the attempt it replaced.
    let mut world = World::with_s4();
    let old = world.start(S1, "a", 0, &[]);
    let ask = world.ask_takeover(S2, "a", MIN, &[old]);
    let again = world.start(S1, "a", 2 * MIN, &[old]);
    let answer = world.owner_answers("a", ask, "takeover", 3 * MIN);
    let state = reduce_at(&world.events, 4 * MIN);
    assert!(grants(&state, "a").is_empty());
    assert!(!may_start_applying(&state, S2, "a"));
    same_on_every_host(&world.events, 4 * MIN);
    // A start on that answer over the new attempt is not the owner's choice.
    world.pulse(S1, "a", again, 4 * MIN);
    world.start_applying(S2, "a", 5 * MIN, again, answer);
    assert_eq!(conflict_rows(&reduce_at(&world.events, 6 * MIN)).len(), 1);
    same_on_every_host(&world.events, 6 * MIN);
}
