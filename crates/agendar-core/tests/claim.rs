//! The claim lease and the correction record: a self-correcting board from
//! self-reported evidence only (Vault events, session liveness, time). Built
//! from explicit events in memory; the approved seeds stay untouched.

mod support;

use std::collections::BTreeMap;

use agendar_core::model::{
    CLAIM_EXPIRY_S, CorrectionRule, Event, EventId, FeatureState, Gate, Lease, NeedsKind, NodePath,
    Phase, Progress, ProjectSlug, ProjectState, State, UtcTime,
};
use agendar_core::reduce::attention::{SILENT_ROW, is_correction_row, notification_eligible};
use agendar_core::reduce::nudges::{NudgeKind, for_session};
use agendar_core::reduce::{ReduceContext, reduce};
use serde_json::{Value, json};

const P: &str = "p";
const S1: &str = "00000000000000000000000101";
const S2: &str = "00000000000000000000000102";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const STEWARD: &str = "h-00000000000000000000000001/steward";
const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// Every step's ETA upper bound in these tests.
const ETA_MAX_S: i64 = 20 * MIN;

fn base() -> UtcTime {
    UtcTime::parse("2026-01-01T00:00:00.000Z").expect("timestamp")
}

/// `s` seconds after the start of the world.
fn at(s: i64) -> UtcTime {
    base().add_seconds(s)
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

fn writer(session: &str) -> String {
    format!("h-00000000000000000000000001/s-{session}")
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

fn rule(state: &State, name: &str) -> Option<CorrectionRule> {
    step(state, name).correction.as_ref().map(|c| c.rule)
}

struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with independent steps `a`, `b`, `c` and `d`
    /// depending on `a`.
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
            json!({"title": "Claims", "brief": "b", "goal": "g"}),
        );
        for (name, depends) in [
            ("a", json!([])),
            ("b", json!([])),
            ("c", json!([])),
            ("d", json!(["a"])),
        ] {
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

    fn start(&mut self, session: &str, step: &str, s: i64, causes: &[u64]) -> u64 {
        let causes: Vec<String> = causes.iter().map(|cause| uid(*cause)).collect();
        self.push(
            &writer(session),
            s,
            "work.progress",
            json!({"node": step, "causes": causes}),
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

    fn pulse(&mut self, session: &str, step: &str, attempt: u64, eta: bool, s: i64) -> u64 {
        let mut body = json!({"agent": Self::agent(session), "message": "note", "boundary": false});
        if eta {
            body["eta"] = json!({"min_s": 600, "max_s": ETA_MAX_S});
        }
        self.push(
            &writer(session),
            s,
            "pulse",
            json!({"node": step, "attempt": uid(attempt)}),
            body,
        )
    }

    /// A session pulse with no step (registration).
    fn hello(&mut self, session: &str, s: i64) -> u64 {
        self.push(
            &writer(session),
            s,
            "pulse",
            json!({}),
            json!({"agent": Self::agent(session), "message": "Session started", "boundary": false}),
        )
    }

    fn deliver(&mut self, session: &str, step: &str, attempt: u64, s: i64) -> u64 {
        self.redeliver(session, step, attempt, None, s)
    }

    fn redeliver(
        &mut self,
        session: &str,
        step: &str,
        attempt: u64,
        supersedes: Option<u64>,
        s: i64,
    ) -> u64 {
        let mut body = support::delivery_body();
        body["agent"] = Self::agent(session);
        body["criteria"] = json!([]);
        let mut refs = json!({"node": step, "attempt": uid(attempt)});
        if let Some(earlier) = supersedes {
            refs["supersedes"] = json!(uid(earlier));
        }
        self.push(&writer(session), s, "delivery", refs, body)
    }

    fn review(&mut self, step: &str, delivery: u64, verdict: &str, s: i64) -> u64 {
        let mut body = json!({"verdict": verdict});
        if verdict == "send_back" {
            body["reason"] = json!("not yet");
        }
        self.push(
            OPERATOR,
            s,
            "review.result",
            json!({"node": step, "delivery": uid(delivery), "causes": [uid(delivery)]}),
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

    fn answer(&mut self, step: &str, ask: u64, s: i64) -> u64 {
        self.push(
            OPERATOR,
            s,
            "decision.answer",
            json!({"node": step, "ask": uid(ask), "causes": [uid(ask)]}),
            json!({"option": "a", "done_when": [{"id": "continues", "text": "It continues."}]}),
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

    fn continued(&mut self, session: &str, previous: &str, s: i64) -> u64 {
        self.push(
            STEWARD,
            s,
            "session.continued",
            json!({}),
            json!({"session": session, "previous": previous}),
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
}

// ---------------------------------------------------------------------
// The lease itself
// ---------------------------------------------------------------------

#[test]
fn a_quiet_claim_is_held_inside_its_eta_stalled_after_it_and_free_t_later() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    // Quiet for 10 minutes, inside its 20-minute ETA: held.
    let state = reduce_at(&world.events, 10 * MIN);
    let claim = step(&state, "a").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Held);
    assert_eq!(claim.last_word, at(0));
    assert!(step(&state, "a").correction.is_none());
    // Past the ETA and quiet: stalled, a label with no correction.
    let state = reduce_at(&world.events, 3 * HOUR);
    let claim = step(&state, "a").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Stalled);
    assert_eq!(claim.since, at(ETA_MAX_S));
    assert!(step(&state, "a").correction.is_none());
    assert!(project(&state).needs_you.is_empty());
    // One second before max(ETA, last word) + T: still stalled.
    let expiry = ETA_MAX_S + CLAIM_EXPIRY_S;
    assert_eq!(
        lease(&reduce_at(&world.events, expiry - 1), "a"),
        Some(Lease::Stalled)
    );
    // At it: free, corrected by silence, with a rescue row that never notifies.
    let state = reduce_at(&world.events, expiry);
    let feature = step(&state, "a");
    assert_eq!(feature.progress, Progress::Doing);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    let correction = feature.correction.clone().expect("correction");
    assert_eq!(correction.rule, CorrectionRule::Silent);
    assert_eq!(correction.since, at(expiry));
    assert_eq!(correction.evidence, vec![id(attempt)]);
    let row = project(&state)
        .needs_you
        .iter()
        .find(|row| row.node == Some(node("a")))
        .expect("rescue row");
    assert_eq!(row.kind, NeedsKind::Rescue);
    assert_eq!(row.id, format!("{SILENT_ROW}{}", uid(attempt)));
    assert_eq!(
        row.question_msg.as_ref().map(|msg| msg.key.as_str()),
        Some("need.q.abandoned")
    );
    assert!(is_correction_row(row));
    assert!(!notification_eligible(row));
}

#[test]
fn a_live_word_holds_past_the_eta_and_the_clock_runs_from_it() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    world.pulse(S1, "a", attempt, false, 2 * HOUR);
    // Past the ETA, but heard 5 minutes ago: held.
    assert_eq!(
        lease(&reduce_at(&world.events, 2 * HOUR + 5 * MIN), "a"),
        Some(Lease::Held)
    );
    // Free T after the later of the ETA and the last word.
    let expiry = 2 * HOUR + CLAIM_EXPIRY_S;
    assert_eq!(
        lease(&reduce_at(&world.events, expiry - 1), "a"),
        Some(Lease::Stalled)
    );
    assert_eq!(
        lease(&reduce_at(&world.events, expiry), "a"),
        Some(Lease::Free)
    );
}

#[test]
fn g1_a_session_busy_on_another_step_does_not_hold_a_forgotten_one() {
    let mut world = World::new();
    let forgotten = world.start(S1, "b", 0, &[]);
    let busy = world.start(S1, "c", MIN, &[]);
    // The session keeps talking about c for two days.
    let mut s = MIN;
    while s < 2 * DAY {
        s += HOUR;
        world.pulse(S1, "c", busy, true, s);
    }
    let state = reduce_at(&world.events, s + MIN);
    assert_eq!(
        project(&state).sessions[&S1.parse().expect("session")]
            .liveness
            .as_str(),
        "live"
    );
    assert_eq!(lease(&state, "c"), Some(Lease::Held));
    assert_eq!(lease(&state, "b"), Some(Lease::Free));
    assert_eq!(rule(&state, "b"), Some(CorrectionRule::Silent));
    let claim = step(&state, "b").claim.clone().expect("claim");
    assert_eq!(claim.last_word, at(0));
    assert_eq!(claim.since, at(ETA_MAX_S + CLAIM_EXPIRY_S));
    let _ = forgotten;
}

#[test]
fn a_lost_session_frees_every_doing_step_with_a_row_each_and_continued_restores() {
    let mut world = World::new();
    let first = world.start(S1, "a", 0, &[]);
    let second = world.start(S1, "b", MIN, &[]);
    let lost = world.lost(S1, 5 * MIN);
    let state = reduce_at(&world.events, 6 * MIN);
    for name in ["a", "b"] {
        assert_eq!(lease(&state, name), Some(Lease::Free), "{name}");
        let correction = step(&state, name).correction.clone().expect("correction");
        assert_eq!(correction.rule, CorrectionRule::Lost);
        assert_eq!(correction.evidence, vec![id(lost)]);
        assert_eq!(correction.since, at(5 * MIN));
    }
    let rows: Vec<&str> = project(&state)
        .needs_you
        .iter()
        .filter(|row| row.kind == NeedsKind::Rescue)
        .map(|row| row.id.as_str())
        .collect();
    // The step it was on keeps the row id it always had and notifies; the
    // other step gets its own row, listed but never announced.
    let on = format!("rescue:lost:{}", uid(lost));
    let other = format!("rescue:lost-step:{}:a", uid(lost));
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.contains(&on.as_str()) && rows.contains(&other.as_str()),
        "{rows:?}"
    );
    let other_row = project(&state)
        .needs_you
        .iter()
        .find(|row| row.id == other)
        .expect("row");
    assert!(!notification_eligible(other_row));
    // The same conversation resumes: its steps are held again, no correction.
    world.hello(S2, 7 * MIN);
    world.continued(S2, S1, 8 * MIN);
    let state = reduce_at(&world.events, 9 * MIN);
    for name in ["a", "b"] {
        assert_eq!(lease(&state, name), Some(Lease::Held), "{name}");
        assert!(step(&state, name).correction.is_none());
        assert_eq!(
            step(&state, name).session.as_ref().map(|s| s.as_str()),
            Some(S2)
        );
    }
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| row.kind != NeedsKind::Rescue)
    );
    let _ = (first, second);
}

#[test]
fn rule_1b_an_accepted_step_whose_claim_expires_settles_to_done() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let delivery = world.deliver(S1, "a", attempt, 5 * MIN);
    // The agent said "more to come" after delivering, then went quiet.
    let more = world.pulse(S1, "a", attempt, true, 6 * MIN);
    let review = world.review("a", delivery, "accept", 30 * MIN);
    let state = reduce_at(&world.events, HOUR);
    assert_eq!(step(&state, "a").progress, Progress::Doing);
    assert!(step(&state, "a").correction.is_none());
    assert_ne!(step(&state, "d").gate, Gate::Open);
    // max(ETA from the pulse, last word) + T.
    let expiry = 6 * MIN + ETA_MAX_S + CLAIM_EXPIRY_S;
    let state = reduce_at(&world.events, expiry);
    let feature = step(&state, "a");
    assert_eq!(feature.progress, Progress::Done);
    assert!(feature.claim.is_none());
    let correction = feature.correction.clone().expect("correction");
    assert_eq!(correction.rule, CorrectionRule::ClosedAfterAccept);
    assert_eq!(correction.since, at(expiry));
    assert_eq!(correction.evidence, vec![id(review), id(more)]);
    // The owner's verdict is the last fact: dependents open, no rescue row.
    assert_eq!(step(&state, "d").gate, Gate::Open);
    assert!(
        project(&state)
            .needs_you
            .iter()
            .all(|row| row.kind != NeedsKind::Rescue)
    );
    // Undo: the agent's next ordinary word reopens it.
    world.pulse(S1, "a", attempt, false, expiry + MIN);
    let state = reduce_at(&world.events, expiry + 2 * MIN);
    assert_eq!(step(&state, "a").progress, Progress::Doing);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(step(&state, "a").correction.is_none());
}

#[test]
fn rule_1b_also_closes_an_accepted_step_of_a_lost_session() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let delivery = world.deliver(S1, "a", attempt, 5 * MIN);
    world.pulse(S1, "a", attempt, true, 6 * MIN);
    world.review("a", delivery, "accept", 30 * MIN);
    world.lost(S1, 40 * MIN);
    let state = reduce_at(&world.events, 41 * MIN);
    assert_eq!(step(&state, "a").progress, Progress::Done);
    assert_eq!(rule(&state, "a"), Some(CorrectionRule::ClosedAfterAccept));
}

// ---------------------------------------------------------------------
// Holds: the clock stops while the owner holds the step or the session
// ---------------------------------------------------------------------

#[test]
fn a_session_waiting_on_its_own_question_holds_all_its_steps_until_the_answer() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    // The question is on another step; the session then ends its turn.
    let ask = world.ask(S1, "b", 30 * MIN);
    let state = reduce_at(&world.events, 5 * DAY);
    let claim = step(&state, "a").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Held);
    assert_eq!(claim.waiting_on, Some(id(ask)));
    assert!(step(&state, "a").correction.is_none());
    // The board says why: the Needs-you row for that question exists.
    assert!(
        project(&state)
            .needs_you
            .iter()
            .any(|row| row.kind == NeedsKind::Decide && row.source == id(ask))
    );
    // The answer restarts the clock from the answer, not from the old word.
    let answered = 5 * DAY;
    world.answer("b", ask, answered);
    assert_eq!(
        lease(&reduce_at(&world.events, answered + HOUR), "a"),
        Some(Lease::Stalled)
    );
    let expiry = answered + CLAIM_EXPIRY_S;
    assert_eq!(
        lease(&reduce_at(&world.events, expiry - 1), "a"),
        Some(Lease::Stalled)
    );
    let state = reduce_at(&world.events, expiry);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
    assert_eq!(rule(&state, "a"), Some(CorrectionRule::Silent));
}

#[test]
fn another_sessions_question_does_not_hold_this_session() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.hello(S2, MIN);
    world.ask(S2, "b", 2 * MIN);
    let state = reduce_at(&world.events, 2 * DAY);
    assert_eq!(lease(&state, "a"), Some(Lease::Free));
}

#[test]
fn a_step_waiting_on_a_decision_never_expires() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    let ask = world.ask(S1, "a", MIN);
    let state = reduce_at(&world.events, 30 * DAY);
    let claim = step(&state, "a").claim.clone().expect("claim");
    assert_eq!(claim.state, Lease::Held);
    assert_eq!(claim.waiting_on, Some(id(ask)));
    assert!(step(&state, "a").correction.is_none());
}

#[test]
fn a_step_in_review_is_yours_and_never_expires() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let delivery = world.deliver(S1, "a", attempt, 5 * MIN);
    let state = reduce_at(&world.events, 30 * DAY);
    let feature = step(&state, "a");
    assert_eq!(feature.phase, Some(Phase::InReview));
    assert!(feature.claim.is_none());
    assert!(feature.correction.is_none());
    let rows: Vec<_> = project(&state).needs_you.iter().collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, NeedsKind::Review);
    assert_eq!(rows[0].source, id(delivery));
}

#[test]
fn a_paused_or_handed_off_step_carries_no_claim() {
    let mut world = World::new();
    world.start(S1, "a", 0, &[]);
    world.handoff("a", MIN);
    let state = reduce_at(&world.events, 30 * DAY);
    assert!(step(&state, "a").claim.is_none());
    assert!(step(&state, "a").correction.is_none());
}

// ---------------------------------------------------------------------
// Corrections from the settle, with their record
// ---------------------------------------------------------------------

#[test]
fn accepted_and_sent_back_carry_their_record_and_undo_with_the_next_word() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let first = world.deliver(S1, "a", attempt, 5 * MIN);
    let back = world.review("a", first, "send_back", 10 * MIN);
    let state = reduce_at(&world.events, 11 * MIN);
    let correction = step(&state, "a").correction.clone().expect("sent back");
    assert_eq!(correction.rule, CorrectionRule::SentBack);
    assert_eq!(correction.evidence, vec![id(back), id(first)]);
    assert_eq!(correction.since, at(10 * MIN));
    // A sent-back step has a claim like any open step.
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    // Undo by redelivery: in review is a label, not a correction.
    let second = world.redeliver(S1, "a", attempt, Some(first), 20 * MIN);
    let state = reduce_at(&world.events, 21 * MIN);
    assert_eq!(step(&state, "a").phase, Some(Phase::InReview));
    assert!(step(&state, "a").correction.is_none());
    // Accept: done, corrected.
    let review = world.review("a", second, "accept", 30 * MIN);
    let state = reduce_at(&world.events, 31 * MIN);
    let correction = step(&state, "a").correction.clone().expect("accepted");
    assert_eq!(step(&state, "a").progress, Progress::Done);
    assert_eq!(correction.rule, CorrectionRule::Accepted);
    assert_eq!(correction.evidence, vec![id(review), id(second)]);
    assert!(step(&state, "a").claim.is_none());
    // Undo by an ETA pulse: the agent continues.
    world.pulse(S1, "a", attempt, true, 40 * MIN);
    let state = reduce_at(&world.events, 41 * MIN);
    assert_eq!(step(&state, "a").progress, Progress::Doing);
    assert!(step(&state, "a").correction.is_none());
}

#[test]
fn an_agent_that_said_done_itself_is_not_corrected_by_the_accept() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let delivery = world.deliver(S1, "a", attempt, 5 * MIN);
    world.status(S1, "a", attempt, "done", 6 * MIN);
    world.review("a", delivery, "accept", 10 * MIN);
    let state = reduce_at(&world.events, 11 * MIN);
    assert_eq!(step(&state, "a").progress, Progress::Done);
    assert!(step(&state, "a").correction.is_none());
}

#[test]
fn a_silent_claim_is_renewed_by_the_holders_next_word() {
    let mut world = World::new();
    let attempt = world.start(S1, "a", 0, &[]);
    let late = 3 * DAY;
    assert_eq!(
        rule(&reduce_at(&world.events, late), "a"),
        Some(CorrectionRule::Silent)
    );
    world.pulse(S1, "a", attempt, false, late);
    let state = reduce_at(&world.events, late + MIN);
    assert_eq!(lease(&state, "a"), Some(Lease::Held));
    assert!(step(&state, "a").correction.is_none());
    assert!(project(&state).needs_you.is_empty());
}

// ---------------------------------------------------------------------
// Taken over: the returning agent is told once
// ---------------------------------------------------------------------

#[test]
fn a_returning_agent_whose_step_was_taken_over_gets_a_freed_nudge() {
    let mut world = World::new();
    let old = world.start(S1, "a", 0, &[]);
    world.lost(S1, 5 * MIN);
    let handoff = world.handoff("a", 10 * MIN);
    world.hello(S2, 11 * MIN);
    let new = world.start(S2, "a", 12 * MIN, &[old, handoff]);
    let state = reduce_at(&world.events, 13 * MIN);
    let feature = step(&state, "a");
    assert_eq!(feature.attempt, Some(id(new)));
    let from = feature.taken_from.clone().expect("taken from");
    assert_eq!(from.attempt, id(old));
    assert_eq!(from.session.as_str(), S1);
    let nudges = for_session(project(&state), &S1.parse().expect("session"));
    let nudge = nudges
        .iter()
        .find(|nudge| nudge.kind == NudgeKind::TakenOver)
        .expect("nudge");
    assert!(
        nudge.text.contains("freed") && nudge.text.contains(S2),
        "{}",
        nudge.text
    );
    assert!(
        for_session(project(&state), &S2.parse().expect("session"))
            .iter()
            .all(|nudge| nudge.kind != NudgeKind::TakenOver)
    );
}

// ---------------------------------------------------------------------
// The contract, as properties over generated histories
// ---------------------------------------------------------------------

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone)]
struct Run {
    session: &'static str,
    attempt: u64,
    deliveries: Vec<u64>,
}

/// One random history over two sessions and three independent steps, plus
/// `d` downstream of `a`. Returns the events and the last timestamp.
fn history(seed: u64) -> (Vec<Event>, i64) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut world = World::new();
    let mut s = 0_i64;
    let sessions = [S1, S2];
    world.hello(S1, s);
    world.hello(S2, s);
    let mut runs: BTreeMap<&str, Run> = BTreeMap::new();
    let mut asks: Vec<(&str, u64)> = Vec::new();
    let mut lost = [false, false];
    for _ in 0..(6 + rng.below(18)) {
        s += i64::try_from(rng.below(4 * 60)).unwrap_or(0) * MIN;
        let name = ["a", "b", "c", "d"][rng.below(4) as usize];
        let who = rng.below(2) as usize;
        match rng.below(10) {
            0 | 1 => {
                if !runs.contains_key(name) && !lost[who] {
                    let attempt = world.start(sessions[who], name, s, &[]);
                    runs.insert(
                        name,
                        Run {
                            session: sessions[who],
                            attempt,
                            deliveries: Vec::new(),
                        },
                    );
                }
            }
            2 | 3 => {
                if let Some(run) = runs.get(name) {
                    let eta = rng.below(2) == 0;
                    world.pulse(run.session, name, run.attempt, eta, s);
                }
            }
            4 => {
                if let Some(run) = runs.get_mut(name) {
                    let delivery = world.deliver(run.session, name, run.attempt, s);
                    run.deliveries.push(delivery);
                }
            }
            5 => {
                if let Some(delivery) = runs.get(name).and_then(|run| run.deliveries.last()) {
                    let verdict = if rng.below(3) == 0 {
                        "send_back"
                    } else {
                        "accept"
                    };
                    world.review(name, *delivery, verdict, s);
                }
            }
            6 => {
                if !lost[who] {
                    asks.push((name, world.ask(sessions[who], name, s)));
                }
            }
            7 => {
                if !asks.is_empty() {
                    let (step, ask) = asks.remove(rng.below(asks.len() as u64) as usize);
                    world.answer(step, ask, s);
                }
            }
            8 => {
                if !lost[who] && rng.below(3) == 0 {
                    world.lost(sessions[who], s);
                    lost[who] = true;
                }
            }
            _ => {
                if let Some(run) = runs.get(name).cloned()
                    && rng.below(3) == 0
                {
                    world.status(run.session, name, run.attempt, "done", s);
                }
            }
        }
    }
    (world.events, s)
}

/// The agent's own last word on the current attempt, as a progress.
fn self_report(feature: &FeatureState, events: &[Event]) -> Progress {
    let Some(attempt) = &feature.attempt else {
        return feature.progress;
    };
    let mut status = Progress::Doing;
    for event in events {
        if let agendar_core::model::EventPayload::WorkProgress(body) = &event.payload
            && event.refs.attempt.as_ref() == Some(attempt)
        {
            status = match body.status {
                agendar_core::model::WorkStatus::Done
                | agendar_core::model::WorkStatus::Verified => Progress::Done,
                agendar_core::model::WorkStatus::Dropped => Progress::Dropped,
                agendar_core::model::WorkStatus::Started => Progress::Doing,
            };
        }
    }
    status
}

#[test]
fn property_record_present_iff_shown_differs_from_the_last_self_report() {
    for seed in 1..=300 {
        let (events, last) = history(seed);
        for as_of in [last + MIN, last + 3 * HOUR, last + 2 * DAY] {
            let state = reduce_at(&events, as_of);
            for feature in project(&state).features.values() {
                if feature.attempt.is_none() {
                    assert!(feature.correction.is_none());
                    continue;
                }
                let said = self_report(feature, &events);
                let differs = (feature.progress != said
                    && !(said == Progress::Done && feature.progress == Progress::Verified))
                    || feature
                        .claim
                        .as_ref()
                        .is_some_and(|claim| claim.state == Lease::Free)
                    || feature.phase == Some(Phase::SentBack);
                assert_eq!(
                    feature.correction.is_some(),
                    differs,
                    "seed {seed} as_of +{} {}: {:?} said {:?}, shown {:?} {:?} {:?}",
                    as_of - last,
                    feature.node,
                    feature.correction,
                    said,
                    feature.progress,
                    feature.phase,
                    feature.claim
                );
                if let Some(correction) = &feature.correction {
                    assert!(!correction.evidence.is_empty() && correction.evidence.len() <= 2);
                }
            }
        }
    }
}

#[test]
fn property_g_claim_after_t_nothing_is_held_without_an_open_question() {
    for seed in 1..=300 {
        let (events, last) = history(seed);
        let as_of = last + ETA_MAX_S + CLAIM_EXPIRY_S;
        let state = reduce_at(&events, as_of);
        let project = project(&state);
        for feature in project.features.values() {
            if feature.progress.is_terminal() || feature.progress == Progress::Planned {
                continue;
            }
            let rows: Vec<_> = project
                .needs_you
                .iter()
                .filter(|row| row.node.as_ref() == Some(&feature.node))
                .collect();
            match &feature.claim {
                // Yours: a delivery waits for your review (its review row).
                None => assert!(
                    rows.iter().any(|row| row.kind == NeedsKind::Review),
                    "seed {seed} {}: no claim, rows {rows:?}",
                    feature.node
                ),
                Some(claim) => match claim.state {
                    Lease::Free => assert!(
                        rows.iter().any(|row| row.kind == NeedsKind::Rescue),
                        "seed {seed} {}: free with no rescue row",
                        feature.node
                    ),
                    Lease::Held => {
                        let ask = claim.waiting_on.as_ref().unwrap_or_else(|| {
                            panic!("seed {seed} {}: held with no question", feature.node)
                        });
                        assert!(
                            project
                                .needs_you
                                .iter()
                                .any(|row| row.kind == NeedsKind::Decide && &row.source == ask),
                            "seed {seed} {}: held on {ask:?} with no Needs-you row",
                            feature.node
                        );
                    }
                    Lease::Stalled => panic!("seed {seed} {}: stalled after T", feature.node),
                },
            }
        }
    }
}

#[test]
fn property_time_only_weakens() {
    for seed in 1..=200 {
        let (events, last) = history(seed);
        let mut previous: Option<State> = None;
        for as_of in [last, last + HOUR, last + DAY, last + 3 * DAY] {
            let state = reduce_at(&events, as_of);
            if let Some(before) = &previous {
                for (name, now) in &project(&state).features {
                    let then = &project(before).features[name];
                    // Progress moves only by Rule 1b, which rests on the
                    // owner's accept.
                    if then.progress != now.progress {
                        assert_eq!(
                            (then.progress, now.progress),
                            (Progress::Doing, Progress::Done),
                            "seed {seed} {name}"
                        );
                        assert_eq!(
                            now.correction.as_ref().map(|c| c.rule),
                            Some(CorrectionRule::ClosedAfterAccept),
                            "seed {seed} {name}"
                        );
                    }
                    // A claim only weakens: held → stalled → free.
                    let rank = |claim: Option<Lease>| match claim {
                        Some(Lease::Held) => 0,
                        Some(Lease::Stalled) => 1,
                        Some(Lease::Free) | None => 2,
                    };
                    if then.claim.is_some() {
                        assert!(
                            rank(now.claim.as_ref().map(|c| c.state))
                                >= rank(then.claim.as_ref().map(|c| c.state)),
                            "seed {seed} {name}: {:?} then {:?}",
                            then.claim,
                            now.claim
                        );
                    }
                }
            }
            previous = Some(state);
        }
    }
}

#[test]
fn property_purity_same_events_and_as_of_give_the_same_board() {
    for seed in 1..=100 {
        let (events, last) = history(seed);
        let as_of = last + DAY + 7 * HOUR;
        let one = serde_json::to_string(&reduce_at(&events, as_of)).expect("json");
        let mut shuffled = events.clone();
        let mut rng = Rng(seed | 1);
        for index in (1..shuffled.len()).rev() {
            let other = rng.below(index as u64 + 1) as usize;
            shuffled.swap(index, other);
        }
        let two = serde_json::to_string(&reduce_at(&shuffled, as_of)).expect("json");
        assert_eq!(one, two, "seed {seed}");
    }
}
