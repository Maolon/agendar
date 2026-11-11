//! The owner snoozes a Needs-you row (`need.snoozed`, written by the operator).
//!
//! The row carries `snoozed_until` on every host while the snooze lasts and
//! the row has not changed. The latest snooze of a row in the reducer's order
//! is the one in effect, so two hosts that snooze at once agree in whatever
//! order their events arrive. Nothing else in the state depends on a snooze,
//! and nothing an agent reads does. The reducer tests build events in memory;
//! the service tests use a temporary Vault and data root.

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use agendar_core::board::BoardView;
use agendar_core::error::ErrorCode;
use agendar_core::guard::need_revision;
use agendar_core::index::query::{Query, QueryResult, query};
use agendar_core::model::{
    DecisionCard, EtaRange, Event, EventId, Harness, Mode, NeedsYouItem, NodePath, ProjectSlug,
    ProjectState, Seconds, SessionId, State, UtcTime, canonical_json,
};
use agendar_core::owed::owed;
use agendar_core::reduce::attention::runnable_nodes;
use agendar_core::reduce::nudges::{for_session, plan_changes};
use agendar_core::reduce::snooze::BEFORE_DEFAULT_S;
use agendar_core::reduce::{ReduceContext, reduce, resolve};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::{NoopWaitObserver, Service, Snoozed, WaitOptions};
use agendar_core::vault::{Clock as _, SystemClock};
use serde_json::{Value, json};

const P: &str = "p";
const S1: &str = "00000000000000000000000101";
const S2: &str = "00000000000000000000000102";
/// The owner at the Mac (host 1) and at a mobile client (host 3).
const MAC: &str = "h-00000000000000000000000001/operator";
const MOBILE: &str = "h-00000000000000000000000003/operator";
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

fn slug() -> ProjectSlug {
    ProjectSlug::parse(P).expect("slug")
}

fn node(value: &str) -> NodePath {
    NodePath::parse(value).expect("node")
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

fn row<'a>(state: &'a State, id: &str) -> &'a NeedsYouItem {
    project(state)
        .needs_you
        .iter()
        .find(|need| need.id == id)
        .unwrap_or_else(|| panic!("no row {id} in {:?}", project(state).needs_you))
}

/// The revision a host shows for the row, and a snooze of it carries.
fn revision(state: &State, id: &str) -> String {
    let project = project(state);
    let need = row(state, id);
    need_revision(
        need,
        project.decisions.get(&need.source),
        &project.policy.revision,
    )
    .as_str()
    .to_owned()
}

#[derive(Clone)]
struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<String, u64>,
}

impl World {
    /// A declared project with steps `a` and `b` (`b` follows `a`), two
    /// sessions, `a` started by S1 and an open question on it.
    fn new() -> (Self, String) {
        let mut world = Self {
            events: Vec::new(),
            next_id: 1000,
            seq: BTreeMap::new(),
        };
        world.push(
            MAC,
            0,
            "project.declared",
            json!({}),
            json!({"title": "Snooze", "brief": "b", "goal": "g"}),
        );
        for (name, depends_on) in [("a", json!([])), ("b", json!(["a"]))] {
            world.push(
                MAC,
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
        world.start(S1, "a", MIN);
        let ask = world.ask(S1, "a", 2 * MIN);
        (world, format!("decide:{}", uid(ask)))
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

    fn start(&mut self, by: &str, step: &str, s: i64) -> u64 {
        self.push(
            &writer(by),
            s,
            "work.progress",
            json!({"node": step}),
            json!({"agent": agent(by), "status": "started",
                   "eta": {"min_s": 600, "max_s": 1200}, "evidence": []}),
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

    /// A question whose default goes ahead `after_s` after it is asked.
    fn ask_with_default(&mut self, by: &str, step: &str, s: i64, after_s: i64) -> String {
        let mut card = support::card();
        card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": after_s});
        let ask = self.push(
            &writer(by),
            s,
            "decision.ask",
            json!({"node": step}),
            json!({"agent": agent(by), "card": card, "policy": support::policy_eval()}),
        );
        format!("decide:{}", uid(ask))
    }

    /// The owner at `operator` snoozes `need`, as that host shows it at `s`,
    /// until `until_s`.
    fn snooze(&mut self, operator: &str, need: &str, s: i64, until_s: i64) -> u64 {
        let revision = revision(&reduce_at(&self.events, s), need);
        self.snooze_with(operator, need, &revision, s, until_s)
    }

    fn snooze_with(
        &mut self,
        operator: &str,
        need: &str,
        revision: &str,
        s: i64,
        until_s: i64,
    ) -> u64 {
        self.push(
            operator,
            s,
            "need.snoozed",
            json!({}),
            json!({"need": need, "revision": revision, "until": at(until_s)}),
        )
    }
}

fn snoozed_until(state: &State, id: &str) -> Option<UtcTime> {
    row(state, id).snoozed_until
}

fn text(state: &State) -> String {
    canonical_json(&serde_json::to_value(state).expect("state"))
}

/// Every permutation of `items`.
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for index in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, first.clone());
            all.push(tail);
        }
    }
    all
}

/// The same state whatever order the events arrive in: every permutation of
/// the snoozes before, after and inside the other events, and every rotation
/// of the whole set.
fn assert_any_arrival_order(events: &[Event], s: i64) -> State {
    let expected = reduce_at(events, s);
    let expected_text = text(&expected);
    let (snoozes, others): (Vec<Event>, Vec<Event>) = events
        .iter()
        .cloned()
        .partition(|event| event.kind() == "need.snoozed");
    let mut orders: Vec<Vec<Event>> = Vec::new();
    for permutation in permutations(&snoozes) {
        orders.push([permutation.clone(), others.clone()].concat());
        orders.push([others.clone(), permutation.clone()].concat());
        let middle = others.len() / 2;
        orders.push([&others[..middle], &permutation[..], &others[middle..]].concat());
    }
    for shift in 1..events.len() {
        let mut rotated = events.to_vec();
        rotated.rotate_left(shift);
        orders.push(rotated);
    }
    for order in orders {
        assert_eq!(
            text(&reduce_at(&order, s)),
            expected_text,
            "arrival order changed the state"
        );
    }
    expected
}

// ---------------------------------------------------------------------
// Reduced state
// ---------------------------------------------------------------------

#[test]
fn a_snooze_hides_the_row_until_its_time() {
    let (mut world, need) = World::new();
    assert_eq!(
        snoozed_until(&reduce_at(&world.events, 5 * MIN), &need),
        None
    );
    world.snooze(MAC, &need, 5 * MIN, 65 * MIN);

    let during = assert_any_arrival_order(&world.events, 10 * MIN);
    assert_eq!(snoozed_until(&during, &need), Some(at(65 * MIN)));
    // The row is still there, ranked and counted as before.
    assert_eq!(project(&during).summary.needs_count, 1);
    // At its time, and after it, the row is no longer snoozed.
    for s in [65 * MIN, 66 * MIN, 2 * DAY] {
        assert_eq!(snoozed_until(&reduce_at(&world.events, s), &need), None);
    }
}

#[test]
fn a_snooze_written_past_the_default_is_cut_when_it_is_read() {
    let (mut world, without_default) = World::new();
    world.start(S2, "b", 3 * MIN);
    // Asked at 0:04, the default goes ahead at 2:04: the row is back at 1:49.
    let need = world.ask_with_default(S2, "b", 4 * MIN, 2 * HOUR);
    let back = 4 * MIN + 2 * HOUR - BEFORE_DEFAULT_S;
    // An older build wrote both snoozes with the time the owner chose.
    world.snooze(MOBILE, &need, 10 * MIN, DAY);
    world.snooze(MOBILE, &without_default, 10 * MIN, DAY);

    let during = assert_any_arrival_order(&world.events, 20 * MIN);
    assert_eq!(snoozed_until(&during, &need), Some(at(back)));
    // A row with no default time is hidden as long as the event says.
    assert_eq!(snoozed_until(&during, &without_default), Some(at(DAY)));
    assert_eq!(
        snoozed_until(&reduce_at(&world.events, back - 1), &need),
        Some(at(back))
    );
    // From then on the row is waiting again, before and after the default.
    for s in [back, back + MIN, 4 * MIN + 2 * HOUR, 3 * HOUR] {
        let state = assert_any_arrival_order(&world.events, s);
        assert_eq!(snoozed_until(&state, &need), None, "at {s}");
        assert_eq!(snoozed_until(&state, &without_default), Some(at(DAY)));
    }
    // A snooze that ends before the limit is not moved.
    world.snooze(MAC, &need, 30 * MIN, HOUR);
    assert_eq!(
        snoozed_until(&reduce_at(&world.events, 40 * MIN), &need),
        Some(at(HOUR))
    );
}

#[test]
fn two_hosts_snoozing_at_once_converge_in_every_arrival_order() {
    let (mut world, need) = World::new();
    // Both snooze in the same millisecond: the order `O` breaks the tie by
    // writer, so the mobile client's (host 3) is the later one on every host.
    world.snooze(MAC, &need, 10 * MIN, HOUR);
    world.snooze(MOBILE, &need, 10 * MIN, 3 * HOUR);
    let state = assert_any_arrival_order(&world.events, 20 * MIN);
    assert_eq!(snoozed_until(&state, &need), Some(at(3 * HOUR)));

    // The Mac ends it later: the un-snooze is the latest word.
    world.snooze(MAC, &need, 20 * MIN, 20 * MIN);
    let state = assert_any_arrival_order(&world.events, 25 * MIN);
    assert_eq!(snoozed_until(&state, &need), None);

    // The mobile client snoozes again after that.
    world.snooze(MOBILE, &need, 30 * MIN, 2 * HOUR);
    let state = assert_any_arrival_order(&world.events, 35 * MIN);
    assert_eq!(snoozed_until(&state, &need), Some(at(2 * HOUR)));

    // A snooze and an un-snooze in the same millisecond: again the later
    // writer, whichever of the two it is.
    let mut ended = world.clone();
    ended.snooze(MAC, &need, 40 * MIN, 5 * HOUR);
    ended.snooze(MOBILE, &need, 40 * MIN, 40 * MIN);
    let state = assert_any_arrival_order(&ended.events, 45 * MIN);
    assert_eq!(snoozed_until(&state, &need), None);
    let mut kept = world.clone();
    kept.snooze(MAC, &need, 40 * MIN, 40 * MIN);
    kept.snooze(MOBILE, &need, 40 * MIN, 5 * HOUR);
    let state = assert_any_arrival_order(&kept.events, 45 * MIN);
    assert_eq!(snoozed_until(&state, &need), Some(at(5 * HOUR)));
}

#[test]
fn a_host_that_has_only_one_of_two_snoozes_shows_that_one_until_the_other_arrives() {
    let (mut world, need) = World::new();
    let base = world.events.len();
    world.snooze(MAC, &need, 10 * MIN, HOUR);
    world.snooze(MOBILE, &need, 11 * MIN, 3 * HOUR);
    let mac_only: Vec<Event> = world.events[..=base].to_vec();
    let mobile_only: Vec<Event> = [&world.events[..base], &world.events[base + 1..]].concat();
    assert_eq!(
        snoozed_until(&reduce_at(&mac_only, 20 * MIN), &need),
        Some(at(HOUR))
    );
    assert_eq!(
        snoozed_until(&reduce_at(&mobile_only, 20 * MIN), &need),
        Some(at(3 * HOUR))
    );
    let both = assert_any_arrival_order(&world.events, 20 * MIN);
    assert_eq!(snoozed_until(&both, &need), Some(at(3 * HOUR)));
}

#[test]
fn seven_days_at_most_and_a_time_in_the_past_ends_the_snooze() {
    let (mut world, need) = World::new();
    let revision = revision(&reduce_at(&world.events, 5 * MIN), &need);
    let line = |until: UtcTime| {
        json!({
            "v": 1, "id": uid(9000), "seq": 9, "writer": MAC, "ts": at(5 * MIN),
            "project": P, "refs": {}, "kind": "need.snoozed",
            "body": {"need": need, "revision": revision, "until": until},
        })
    };
    // Seven days after the event's own time is the longest snooze.
    support::parse_value(&line(at(5 * MIN + 7 * DAY))).expect("seven days");
    let error = support::parse_value(&line(at(5 * MIN + 7 * DAY).add_millis(1)))
        .expect_err("more than seven days");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("snooze_too_long"));

    world.snooze(MAC, &need, 5 * MIN, 5 * MIN + 7 * DAY);
    for s in [6 * MIN, 5 * MIN + 7 * DAY - 1] {
        assert_eq!(
            snoozed_until(&reduce_at(&world.events, s), &need),
            Some(at(5 * MIN + 7 * DAY))
        );
    }
    assert_eq!(
        snoozed_until(&reduce_at(&world.events, 5 * MIN + 7 * DAY), &need),
        None
    );

    // A time that has passed (here: long before the event) ends it at once.
    world.snooze(MOBILE, &need, HOUR, 0);
    let state = assert_any_arrival_order(&world.events, 2 * HOUR);
    assert_eq!(snoozed_until(&state, &need), None);
}

#[test]
fn a_changed_row_is_shown_again() {
    let (mut world, need) = World::new();
    let before = revision(&reduce_at(&world.events, 5 * MIN), &need);
    world.snooze(MAC, &need, 5 * MIN, 6 * HOUR);
    assert!(snoozed_until(&reduce_at(&world.events, 10 * MIN), &need).is_some());

    // The owner changes the decision policy: the card's revision moves (an
    // answer is bound to it), so the snooze of the old one hides nothing.
    let mut changed = world.clone();
    changed.push(
        MAC,
        20 * MIN,
        "policy.set",
        json!({}),
        support::policy_set(),
    );
    let state = assert_any_arrival_order(&changed.events, 30 * MIN);
    assert_ne!(revision(&state, &need), before);
    assert_eq!(snoozed_until(&state, &need), None);
    // Snoozed again as it is now, it is hidden again.
    changed.snooze(MOBILE, &need, 40 * MIN, 6 * HOUR);
    let state = assert_any_arrival_order(&changed.events, 50 * MIN);
    assert_eq!(snoozed_until(&state, &need), Some(at(6 * HOUR)));

    // A snooze that names another revision of the row never hides it, and
    // it is still the latest word: it replaces the one that did.
    let mut stale = world.clone();
    stale.snooze_with(MOBILE, &need, support::DIGEST, 20 * MIN, 6 * HOUR);
    let state = assert_any_arrival_order(&stale.events, 30 * MIN);
    assert_eq!(snoozed_until(&state, &need), None);
}

#[test]
fn a_question_withdrawn_and_asked_again_is_a_new_row() {
    let (mut world, need) = World::new();
    world.snooze(MAC, &need, 5 * MIN, 6 * HOUR);
    let ask = need.trim_start_matches("decide:").to_owned();
    world.push(
        &writer(S1),
        10 * MIN,
        "decision.withdrawn",
        json!({"node": "a", "ask": ask}),
        json!({"agent": agent(S1), "reason": "Asked the wrong thing."}),
    );
    let again = world.ask(S1, "a", 11 * MIN);
    let state = assert_any_arrival_order(&world.events, 20 * MIN);
    let rows: Vec<&str> = project(&state)
        .needs_you
        .iter()
        .map(|need| need.id.as_str())
        .collect();
    assert_eq!(rows, [format!("decide:{}", uid(again))]);
    assert_eq!(snoozed_until(&state, rows[0]), None, "the new question");
}

#[test]
fn a_rescue_row_is_snoozed_like_a_question_and_another_rescue_is_not() {
    let (mut world, _) = World::new();
    // Two agents start `b`'s sibling at once: "Who should continue?".
    world.push(
        MAC,
        3 * MIN,
        "plan.feature",
        json!({"node": "c"}),
        json!({"title": "c", "slice": "s", "demo_kind": "cli_transcript", "depends_on": []}),
    );
    world.start(S1, "c", 4 * MIN);
    world.start(S2, "c", 4 * MIN);
    let state = reduce_at(&world.events, 5 * MIN);
    let rescue = project(&state)
        .needs_you
        .iter()
        .find(|need| need.id.starts_with("rescue:start-conflict:"))
        .expect("a contested step")
        .id
        .clone();
    world.snooze(MOBILE, &rescue, 6 * MIN, HOUR);
    let state = assert_any_arrival_order(&world.events, 10 * MIN);
    assert_eq!(snoozed_until(&state, &rescue), Some(at(HOUR)));
    // Only that row: every other row of the project is as it was.
    for need in &project(&state).needs_you {
        assert_eq!(
            need.snoozed_until.is_some(),
            need.id == rescue,
            "{}",
            need.id
        );
    }
    // The same id under another revision (a rescue whose cause changed).
    let mut other = world.clone();
    other.snooze_with(MAC, &rescue, support::DIGEST, 7 * MIN, HOUR);
    let state = assert_any_arrival_order(&other.events, 10 * MIN);
    assert_eq!(snoozed_until(&state, &rescue), None);
}

// ---------------------------------------------------------------------
// Display only
// ---------------------------------------------------------------------

/// The state without the one thing a snooze may change, and without the
/// digest of the event set (which names the events themselves).
fn without_snoozes(state: &State) -> String {
    let mut state = state.clone();
    state.event_set_digest = agendar_core::model::Sha256::from_bytes(b"");
    for project in state.projects.values_mut() {
        for need in &mut project.needs_you {
            need.snoozed_until = None;
        }
    }
    text(&state)
}

/// What the reducer hands an agent, as text: what it owes, its nudges, the
/// steps it may start and the owner's plan edits.
fn agent_reads(events: &[Event], who: &str, s: i64) -> String {
    let state = reduce_at(events, s);
    let ordered = resolve(events).ordered;
    let session: SessionId = who.parse().expect("session");
    let owes = owed(project(&state), &ordered, &session, at(s), &BTreeMap::new());
    format!(
        "{owes:?}\n{:?}\n{:?}\n{:?}",
        for_session(project(&state), &session),
        runnable_nodes(&session, project(&state)),
        plan_changes(&ordered, &slug(), None, at(0)),
    )
}

#[test]
fn a_snooze_changes_nothing_but_the_rows_own_mark() {
    let (plain, need) = World::new();
    let mut snoozed = plain.clone();
    // Both hosts snooze and un-snooze, long after the agents' last word: a
    // snooze must not count as a host's sign of life or the project's last
    // event, nor move ranking, gates, stalls, defaults or folds.
    snoozed.snooze(MAC, &need, 5 * MIN, 6 * HOUR);
    snoozed.snooze(MOBILE, &need, 3 * HOUR, 3 * HOUR);
    snoozed.snooze(MOBILE, &need, 4 * HOUR, 5 * DAY);
    snoozed.snooze(MAC, &need, DAY, 3 * DAY);

    for s in [
        10 * MIN,
        HOUR,
        3 * HOUR + MIN,
        5 * HOUR,
        DAY + HOUR,
        2 * DAY,
        4 * DAY,
        30 * DAY,
    ] {
        let with = reduce_at(&snoozed.events, s);
        let without = reduce_at(&plain.events, s);
        assert_eq!(
            without_snoozes(&with),
            without_snoozes(&without),
            "the state at {s}s"
        );
        assert_eq!(with.diagnostics, without.diagnostics);
        for who in [S1, S2] {
            assert_eq!(
                agent_reads(&snoozed.events, who, s),
                agent_reads(&plain.events, who, s),
                "what {who} reads at {s}s"
            );
        }
    }
    // And the mark itself is there when it should be.
    assert!(snoozed_until(&reduce_at(&snoozed.events, 2 * DAY), &need).is_some());
    assert!(snoozed_until(&reduce_at(&snoozed.events, 4 * DAY), &need).is_none());
}

#[test]
fn a_snooze_alone_does_not_make_a_project() {
    let (world, need) = World::new();
    let revision = revision(&reduce_at(&world.events, 5 * MIN), &need);
    let stray = json!({
        "v": 1, "id": uid(9000), "seq": 1, "writer": MOBILE, "ts": at(5 * MIN),
        "project": "elsewhere", "refs": {}, "kind": "need.snoozed",
        "body": {"need": need, "revision": revision, "until": at(HOUR)},
    });
    let mut events = world.events.clone();
    events.push(support::parse_value(&stray).expect("decodes"));
    let state = reduce_at(&events, 10 * MIN);
    assert_eq!(state.projects.len(), 1, "no project `elsewhere`");
    // A snooze written in another project hides nothing here.
    assert_eq!(snoozed_until(&state, &need), None);
}

// ---------------------------------------------------------------------
// The service call
// ---------------------------------------------------------------------

struct Desk {
    _dir: tempfile::TempDir,
    service: Service,
    session: SessionId,
    ask: EventId,
    need: String,
}

fn eta() -> EtaRange {
    EtaRange {
        min_s: Seconds::new(600).expect("min"),
        max_s: Seconds::new(1_200).expect("max"),
    }
}

/// One host with a registered agent that started `a` and asked on it. The
/// plan is the owner's, written days ago at another computer, so that the
/// texts that count time ("untouched 3d") do not move while a test runs.
fn desk() -> Desk {
    desk_asking(support::card())
}

/// [`desk`] with a question whose default goes ahead `after_s` from now.
fn desk_with_default(after_s: i64) -> Desk {
    let mut card = support::card();
    card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": after_s});
    desk_asking(card)
}

fn desk_asking(card: Value) -> Desk {
    let dir = tempfile::tempdir().expect("tempdir");
    let (service, _) =
        Service::init(&dir.path().join("vault"), &dir.path().join("data")).expect("init");
    let days_ago = SystemClock.now().add_seconds(-(3 * DAY + 12 * HOUR));
    let plan = [
        (
            "project.declared",
            json!({}),
            json!({"title": "Snooze", "brief": "b", "goal": "A quiet list."}),
        ),
        (
            "plan.feature",
            json!({"node": "a"}),
            json!({"title": "a", "slice": "core", "demo_kind": "cli_transcript"}),
        ),
        (
            "plan.feature",
            json!({"node": "b"}),
            json!({"title": "b", "slice": "core", "demo_kind": "cli_transcript"}),
        ),
    ];
    for (index, (kind, refs, body)) in plan.into_iter().enumerate() {
        let value = json!({
            "v": 1, "id": uid(index as u64 + 1), "seq": index + 1, "writer": MOBILE,
            "ts": days_ago, "project": P, "refs": refs, "kind": kind, "body": body,
        });
        let event = support::parse_value(&value).expect("plan event");
        support::append_event_to_vault(service.vault(), &event, 1).expect("append");
    }
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let session = register(&service, &repo);
    service
        .progress_started(&slug(), &session, &node("a"), eta(), None, None)
        .expect("start");
    let card: DecisionCard = serde_json::from_value(card).expect("card");
    let asked = service
        .ask(&slug(), &session, &node("a"), card, Mode::Ask, None)
        .expect("ask");
    assert_eq!(asked.effective_mode, Mode::Ask);
    Desk {
        need: format!("decide:{}", asked.ask_id.as_str()),
        ask: asked.ask_id,
        _dir: dir,
        service,
        session,
    }
}

fn register(service: &Service, repo: &Path) -> SessionId {
    let pid = std::process::id();
    register_session(
        service,
        SessionRegistration {
            project: slug(),
            harness: Harness::Generic,
            harness_session_id: "agent".to_owned(),
            pid,
            process_start: process_identity(pid).expect("process identity"),
            repo_root: repo.to_path_buf(),
        },
    )
    .expect("register")
}

impl Desk {
    fn snooze(&self, need: &str, until: UtcTime) -> agendar_core::Result<EventId> {
        self.snoozed(need, until).map(|snoozed| snoozed.event)
    }

    fn snoozed(&self, need: &str, until: UtcTime) -> agendar_core::Result<Snoozed> {
        let fresh = self.service.refresh(SystemClock.now(), false)?;
        self.service.snooze_need(&fresh, need, until)
    }

    /// When the question's default goes ahead.
    fn default_at(&self) -> UtcTime {
        let state = self.service.export_state(SystemClock.now()).expect("state");
        project(&state).decisions[&self.ask]
            .default_at
            .expect("a default time")
    }

    /// The `until` of the latest snooze line in the Vault.
    fn stored_until(&self) -> Value {
        let fresh = self
            .service
            .refresh(SystemClock.now(), false)
            .expect("refresh");
        let event = fresh
            .resolution
            .ordered
            .iter()
            .rev()
            .find(|event| event.kind() == "need.snoozed")
            .expect("a snooze line");
        serde_json::to_value(event).expect("event")["body"]["until"].clone()
    }

    fn until(&self) -> Option<UtcTime> {
        let state = self.service.export_state(SystemClock.now()).expect("state");
        row(&state, &self.need).snoozed_until
    }

    fn snooze_lines(&self) -> usize {
        self.service
            .refresh(SystemClock.now(), false)
            .expect("refresh")
            .events
            .iter()
            .filter(|event| event.kind() == "need.snoozed")
            .count()
    }

    /// Everything the agent's commands print, as text: `board` (the default
    /// page and `--all`), `settle`, `inbox`, the facts its hooks read
    /// (`loop_status`, nudges, unseen packets), what it owes, the project's
    /// history, and a `wait` on its question that runs into its timeout.
    fn agent_outputs(&self) -> String {
        let service = &self.service;
        let (project, session) = (slug(), &self.session);
        let board = |view| {
            service
                .board(&project, Some(session), false, &view)
                .expect("board")
                .to_string()
        };
        let (settle, shown) = service.settle(&project, session, false).expect("settle");
        let (packets, rules, _generation) = service
            .inbox(&project, session, None, false)
            .expect("inbox");
        let fresh = service.refresh(SystemClock.now(), false).expect("refresh");
        let history = match query(
            &fresh.index,
            Query::History {
                project: project.clone(),
                after: None,
                limit: 200,
            },
            fresh.now,
        )
        .expect("history")
        {
            QueryResult::History { events, .. } => serde_json::to_string(&events).expect("json"),
            _ => panic!("history"),
        };
        let waited = service
            .wait_for_answer(
                &project,
                session,
                &self.ask,
                &WaitOptions {
                    timeout: Some(Duration::from_millis(150)),
                    ..WaitOptions::default()
                },
                &mut NoopWaitObserver,
            )
            .expect_err("nobody answered");
        [
            board(BoardView::Open),
            board(BoardView::All),
            settle.to_string(),
            format!("{shown:?}"),
            serde_json::to_string(&packets).expect("packets"),
            serde_json::to_string(&rules).expect("rules"),
            format!(
                "{:?}",
                service.loop_status(&project, session).expect("loop")
            ),
            format!("{:?}", service.nudges(&project, session, false).expect("n")),
            format!(
                "{:?}",
                service.plan_nudges(&project, session, false).expect("p")
            ),
            format!(
                "{:?}",
                service.unseen_packets(&project, session).expect("u")
            ),
            format!(
                "{:?}",
                service.owed(&project, session, false).expect("owed")
            ),
            history,
            format!(
                "{:?} {:?} {}",
                waited.code(),
                waited.detail(),
                waited.message()
            ),
        ]
        .join("\n")
    }
}

#[test]
fn what_an_agent_reads_is_byte_equal_with_and_without_a_snooze() {
    let desk = desk();
    let before = desk.agent_outputs();
    assert!(before.contains("untouched 3d"), "the outputs are not empty");

    let hour = SystemClock.now().add_seconds(HOUR);
    desk.snooze(&desk.need, hour).expect("snooze");
    assert_eq!(desk.until(), Some(hour));
    assert_eq!(desk.agent_outputs(), before, "while snoozed");

    desk.snooze(&desk.need, SystemClock.now().add_seconds(-1))
        .expect("end the snooze");
    assert_eq!(desk.until(), None);
    assert_eq!(desk.agent_outputs(), before, "after the snooze ended");
    assert_eq!(desk.snooze_lines(), 2);
}

#[test]
fn the_call_checks_the_row_and_the_time_and_repeats_write_nothing() {
    let desk = desk();
    let now = SystemClock.now();

    let too_long = desk
        .snooze(&desk.need, now.add_seconds(7 * DAY + MIN))
        .expect_err("more than seven days");
    assert_eq!(too_long.code(), ErrorCode::Validation);
    assert_eq!(too_long.detail(), Some("snooze_too_long"));
    let unknown = desk
        .snooze("decide:not-waiting", now.add_seconds(HOUR))
        .expect_err("only a row that is waiting");
    assert_eq!(unknown.code(), ErrorCode::NotFound);
    assert_eq!(unknown.detail(), Some("need_not_waiting"));
    // Ending the snooze of a row that is gone is refused the same way.
    let gone = desk
        .snooze("decide:not-waiting", now.add_seconds(-MIN))
        .expect_err("nothing to end");
    assert_eq!(gone.detail(), Some("need_not_waiting"));
    assert_eq!(desk.snooze_lines(), 0, "a refusal writes nothing");

    let until = now.add_seconds(7 * DAY - MIN);
    let first = desk.snooze(&desk.need, until).expect("just under a week");
    let again = desk.snooze(&desk.need, until).expect("the same again");
    assert_eq!(again, first, "the snooze in effect is returned");
    assert_eq!(desk.snooze_lines(), 1);
    assert_eq!(desk.until(), Some(until));

    // The operator wrote it, in the row's project, with the row's revision.
    let fresh = desk
        .service
        .refresh(SystemClock.now(), false)
        .expect("refresh");
    let event = fresh
        .events
        .iter()
        .find(|event| event.id == first)
        .expect("the event");
    assert_eq!(event.kind(), "need.snoozed");
    assert_eq!(event.project, slug());
    assert!(event.writer.to_string().ends_with("/operator"));
    let body = serde_json::to_value(event).expect("event")["body"].clone();
    assert_eq!(body["need"], json!(desk.need));
    assert_eq!(body["revision"], json!(revision(&fresh.state, &desk.need)));
    assert_eq!(body["until"], json!(until));

    // A different time is a new event, and the latest one holds.
    let shorter = now.add_seconds(HOUR);
    let second = desk.snooze(&desk.need, shorter).expect("shorter");
    assert_ne!(second, first);
    assert_eq!(desk.until(), Some(shorter));
    assert_eq!(desk.snooze_lines(), 2);
}

#[test]
fn a_snooze_past_the_default_is_written_with_the_cut_time() {
    let desk = desk_with_default(2 * HOUR);
    let now = SystemClock.now();
    let back = desk.default_at().add_seconds(-BEFORE_DEFAULT_S);

    // Tomorrow is past the default: the row returns 15 minutes before it.
    let cut = desk.snoozed(&desk.need, now.add_seconds(DAY)).expect("cut");
    assert_eq!(cut.until, back);
    assert_eq!(desk.stored_until(), json!(back));
    assert_eq!(desk.until(), Some(back));
    // Another choice past the default is the snooze in effect: nothing new.
    let again = desk
        .snoozed(&desk.need, now.add_seconds(2 * DAY))
        .expect("the same cut");
    assert_eq!(again, cut);
    assert_eq!(desk.snooze_lines(), 1);

    // A time before the limit is kept as it is.
    let sooner = now.add_seconds(HOUR);
    let kept = desk.snoozed(&desk.need, sooner).expect("an hour");
    assert_eq!(kept.until, sooner);
    assert_eq!(desk.stored_until(), json!(sooner));
    assert_eq!(desk.until(), Some(sooner));

    // The seven-day limit is checked as before, default or not.
    let too_long = desk
        .snooze(&desk.need, now.add_seconds(7 * DAY + MIN))
        .expect_err("more than seven days");
    assert_eq!(too_long.detail(), Some("snooze_too_long"));
    assert_eq!(desk.snooze_lines(), 2);

    // A need with no default is written as asked.
    let plain = self::desk();
    let until = now.add_seconds(DAY);
    assert_eq!(
        plain.snoozed(&plain.need, until).expect("a day").until,
        until
    );
    assert_eq!(plain.until(), Some(until));
}

#[test]
fn a_snooze_is_refused_when_the_default_is_under_fifteen_minutes_away() {
    let desk = desk_with_default(10 * MIN);
    let now = SystemClock.now();

    for until in [now.add_seconds(MIN), now.add_seconds(HOUR)] {
        let near = desk
            .snooze(&desk.need, until)
            .expect_err("the default is too near");
        assert_eq!(near.code(), ErrorCode::Validation);
        assert_eq!(near.detail(), Some("snooze_default_near"));
        assert_eq!(
            agendar_core::error::details::listed("snooze_default_near"),
            Some("snooze_default_near")
        );
    }
    assert_eq!(desk.snooze_lines(), 0, "a refusal writes nothing");
    // The seven-day limit answers first.
    let too_long = desk
        .snooze(&desk.need, now.add_seconds(8 * DAY))
        .expect_err("more than seven days");
    assert_eq!(too_long.detail(), Some("snooze_too_long"));
    // A time that has passed still ends a snooze (here: none to end).
    desk.snooze(&desk.need, now.add_seconds(-MIN))
        .expect("ending is never refused for the default");
    assert_eq!(desk.until(), None);
}
