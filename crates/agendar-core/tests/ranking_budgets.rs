//! Cost-of-delay ranking, speculation budgets, calibrated
//! ETA, agenda edits and the six-section Brief.
//!
//! The approved seeds under `tests/oracle/seeds/` stay the oracle for the eta-* and
//! ranking cases; this file adapts them without rewriting them. The twelve-project
//! brief and the budget cases are built from explicit events in memory.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use agendar_core::model::{
    Event, EventId, Gate, NodePath, ProjectSlug, ProjectState, Sha256, State, UtcTime, Variance,
};
use agendar_core::reduce::brief::{BriefData, BriefTargetKind};
use agendar_core::reduce::{
    ReduceContext, budget, calibration, eta, ordering, reduce, reduce_with_report,
};
use serde_json::{Value, json};

fn slug(value: &str) -> ProjectSlug {
    ProjectSlug::parse(value).expect("project slug")
}

fn node(value: &str) -> NodePath {
    NodePath::parse(value).expect("node path")
}

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

fn reduce_at(events: &[Event], as_of: UtcTime) -> State {
    reduce(events, &ReduceContext { as_of })
}

fn project<'a>(state: &'a State, name: &str) -> &'a ProjectState {
    state.projects.get(&slug(name)).expect("project")
}

// ---------------------------------------------------------------------------
// Approved eta-* seeds
// ---------------------------------------------------------------------------

#[test]
fn approved_eta_one_feature_five_attempts_stays_ineligible() {
    let seed = support::load_seed("eta-one-feature-five-attempts");
    let (state, report) = reduce_with_report(&seed.events, &ReduceContext { as_of: seed.as_of });
    let assertions = seed.assertions();
    let project_state = project(&state, "example");
    let calibration = &report.calibration[&slug("example")];
    assert_eq!(
        calibration.finished_features as u64,
        assertions["finished_feature_count"]
            .as_u64()
            .expect("count"),
        "five attempts on one feature count as one finished feature"
    );
    assert_eq!(
        calibration.samples.len() as u64,
        assertions["attempt_samples"].as_u64().expect("samples"),
        "every completed attempt is a sample"
    );
    assert!(!calibration.eligible());
    assert_eq!(calibration.calibration, Vec::new());
    assert!(project_state.summary.calibration.is_empty());
    for feature in project_state.features.values() {
        assert!(
            feature
                .eta
                .as_ref()
                .is_some_and(|eta| eta.likely_finish.is_none()),
            "an ineligible project keeps the agent's own range"
        );
    }
    assert_eq!(assertions["eligible"], json!(false));
}

#[test]
fn approved_eta_five_features_mixed_calibrates_each_harness() {
    use agendar_core::model::Harness;

    let seed = support::load_seed("eta-five-features-mixed");
    let (state, report) = reduce_with_report(&seed.events, &ReduceContext { as_of: seed.as_of });
    let assertions = seed.assertions();
    let project_state = project(&state, "example");
    let calibration = &report.calibration[&slug("example")];
    assert_eq!(
        calibration.finished_features as u64,
        assertions["finished_feature_count"]
            .as_u64()
            .expect("count")
    );
    assert_eq!(
        calibration.samples.len() as u64,
        assertions["attempt_samples"].as_u64().expect("samples")
    );
    assert!(calibration.eligible());
    let expected = assertions["calibration"].as_array().expect("calibration");
    assert_eq!(project_state.summary.calibration.len(), expected.len());
    for (entry, want) in project_state.summary.calibration.iter().zip(expected) {
        assert_eq!(json!(entry.harness), want["harness"].clone(), "harness");
        assert_eq!(
            u64::from(entry.samples),
            want["samples"].as_u64().expect("samples")
        );
        assert_eq!(
            u64::from(entry.ratio_milli),
            want["ratio_milli"].as_u64().expect("ratio")
        );
    }
    // Eligible project, but generic (3) and codex (2) are below the five timed
    // samples a ratio needs, so neither is adjusted and none borrows another.
    assert_eq!(calibration.ratio_for(Harness::Generic), None);
    assert_eq!(calibration.ratio_for(Harness::Codex), None);
    assert_eq!(calibration.ratio_for(Harness::ClaudeCode), None);
}

#[test]
fn approved_eta_overlapping_pauses_union_and_measure_active_time() {
    let seed = support::load_seed("eta-overlapping-pauses");
    let (state, report) = reduce_with_report(&seed.events, &ReduceContext { as_of: seed.as_of });
    let assertions = seed.assertions();
    let project_state = project(&state, "example");
    let feature = &project_state.features[&node("x")];
    let eta = feature.eta.as_ref().expect("eta");
    assert_eq!(
        eta.paused_s,
        assertions["paused_s"].as_u64().expect("paused seconds"),
        "the union of [2,6] and [4,8] minutes is six minutes"
    );
    assert!(!eta.paused);
    assert_eq!(eta.anchor, at(0));
    // 600 s estimate + 360 s wait from the anchor.
    assert_eq!(eta.agent_finish.latest, at(16));
    let samples = &report.calibration[&slug("example")].samples;
    assert_eq!(samples.len(), 1);
    assert_eq!(
        samples[0].actual_ms,
        assertions["actual_active_s"].as_u64().expect("active") * 1000
    );
    assert_eq!(
        samples[0].ratio_milli,
        assertions["sample_ratio_milli"].as_u64().expect("ratio")
    );
    // The interval arithmetic itself: sorted, merged, no double count.
    assert_eq!(
        eta::union_seconds(
            &[(at(2), Some(at(6))), (at(4), Some(at(8)))],
            at(0),
            at(16),
            at(30)
        ),
        assertions["paused_s"].as_u64().expect("paused")
    );
}

// ---------------------------------------------------------------------------
// Calibration arithmetic
// ---------------------------------------------------------------------------

#[test]
fn calibration_uses_even_medians_and_clamps_the_displayed_ratio() {
    assert_eq!(calibration::median_milli(&[2000, 2500]), Some(2250));
    assert_eq!(calibration::median_milli(&[300, 100, 200]), Some(200));
    assert_eq!(calibration::median_milli(&[1000, 1001]), Some(1001));
    assert_eq!(calibration::median_milli(&[]), None);
    assert_eq!(calibration::clamp_ratio(1), 100);
    assert_eq!(calibration::clamp_ratio(2500), 2500);
    assert_eq!(calibration::clamp_ratio(50_000), 10_000);
    assert_eq!(
        calibration::sample_ratio_milli(600_000, 600_000),
        Some(1000)
    );
    assert_eq!(
        calibration::sample_ratio_milli(900_000, 600_000),
        Some(1500)
    );
    assert_eq!(calibration::sample_ratio_milli(1, 0), None);
}

// ---------------------------------------------------------------------------
// ETA lateness and calibration application
// ---------------------------------------------------------------------------

/// A world of projects built from explicit events, with deterministic IDs.
struct World {
    events: Vec<Event>,
    next_id: u64,
    seq: BTreeMap<(String, String), u64>,
    /// Harness of the progress events that follow (generic by default).
    harness: &'static str,
}

impl World {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            next_id: 1000,
            seq: BTreeMap::new(),
            harness: "generic",
        }
    }

    fn push(
        &mut self,
        project: &str,
        writer: &str,
        minute: u64,
        kind: &str,
        refs: Value,
        body: Value,
    ) -> u64 {
        let event = self.next_id;
        self.next_id += 1;
        let key = (project.to_owned(), writer.to_owned());
        let seq = self.seq.entry(key).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1,
            "id": uid(event),
            "seq": *seq,
            "writer": writer,
            "ts": at(minute),
            "project": project,
            "refs": refs,
            "kind": kind,
            "body": body,
        });
        self.events
            .push(support::parse_value(&value).expect("test event decodes"));
        event
    }

    fn declare(&mut self, project: &str, title: &str, minute: u64) -> u64 {
        self.push(
            project,
            support::OPERATOR,
            minute,
            "project.declared",
            json!({}),
            json!({"title": title, "brief": "Spec fixture", "goal": "Ship the slice"}),
        )
    }

    fn feature(&mut self, project: &str, name: &str, minute: u64) -> u64 {
        self.push(
            project,
            support::OPERATOR,
            minute,
            "plan.feature",
            json!({"node": name}),
            json!({
                "title": name,
                "slice": format!("slice-{name}"),
                "demo_kind": "cli_transcript",
                "depends_on": [],
            }),
        )
    }

    fn feature_depending(&mut self, project: &str, name: &str, on: &str, minute: u64) -> u64 {
        self.push(
            project,
            support::OPERATOR,
            minute,
            "plan.feature",
            json!({"node": name}),
            json!({
                "title": name,
                "slice": format!("slice-{name}"),
                "demo_kind": "cli_transcript",
                "depends_on": [on],
            }),
        )
    }

    /// An operator dependency edit that names no other field.
    fn depends(&mut self, project: &str, name: &str, on: &[&str], minute: u64) {
        self.push(
            project,
            support::OPERATOR,
            minute,
            "plan.feature",
            json!({"node": name}),
            json!({"depends_on": on}),
        );
    }

    fn start(
        &mut self,
        project: &str,
        name: &str,
        session: &str,
        min_s: u64,
        max_s: u64,
        minute: u64,
    ) -> u64 {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "work.progress",
            json!({"node": name}),
            json!({
                "agent": {"session": session, "harness": self.harness},
                "status": "started",
                "eta": {"min_s": min_s, "max_s": max_s},
                "evidence": [],
            }),
        )
    }

    fn done(&mut self, project: &str, name: &str, session: &str, attempt: u64, minute: u64) {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "work.progress",
            json!({"node": name, "attempt": uid(attempt)}),
            json!({
                "agent": {"session": session, "harness": self.harness},
                "status": "done",
                "evidence": [],
            }),
        );
    }

    /// A restart must cite the attempt it replaces.
    fn restart(
        &mut self,
        project: &str,
        name: &str,
        session: &str,
        previous: u64,
        minute: u64,
    ) -> u64 {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "work.progress",
            json!({"node": name, "causes": [uid(previous)]}),
            json!({
                "agent": {"session": session, "harness": "generic"},
                "status": "started",
                "eta": {"min_s": 600, "max_s": 600},
                "evidence": [],
            }),
        )
    }

    /// A boundary pulse that re-estimates the attempt.
    fn reestimate(
        &mut self,
        project: &str,
        name: &str,
        session: &str,
        attempt: u64,
        max_s: u64,
        minute: u64,
    ) {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "pulse",
            json!({"node": name, "attempt": uid(attempt)}),
            json!({
                "agent": {"session": session, "harness": self.harness},
                "message": "re-estimated",
                "boundary": true,
                "eta": {"min_s": max_s / 2, "max_s": max_s},
            }),
        );
    }

    fn dropped(&mut self, project: &str, name: &str, session: &str, attempt: u64, minute: u64) {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "work.progress",
            json!({"node": name, "attempt": uid(attempt)}),
            json!({
                "agent": {"session": session, "harness": "generic"},
                "status": "dropped",
                "evidence": [],
            }),
        );
    }

    fn ask(&mut self, project: &str, name: &str, session: &str, mode: &str, minute: u64) -> u64 {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "decision.ask",
            json!({"node": name}),
            json!({
                "agent": {"session": session, "harness": "generic"},
                "card": support::card(),
                "policy": policy_eval(mode),
            }),
        )
    }

    fn assume(
        &mut self,
        project: &str,
        name: &str,
        session: &str,
        ask: u64,
        attempt: u64,
        minute: u64,
    ) -> u64 {
        self.push(
            project,
            &format!("h-00000000000000000000000001/s-{session}"),
            minute,
            "decision.assumed",
            json!({"node": name, "ask": uid(ask), "attempt": uid(attempt)}),
            json!({
                "agent": {"session": session, "harness": "generic"},
                "option": "a",
                "reason": "policy",
                "policy": policy_eval("assume"),
            }),
        )
    }

    fn answer(&mut self, project: &str, name: &str, ask: u64, option: &str, minute: u64) -> u64 {
        self.push(
            project,
            support::OPERATOR,
            minute,
            "decision.answer",
            json!({"node": name, "ask": uid(ask)}),
            json!({
                "option": option,
                "done_when": [{"id": "continues", "text": "It continues."}],
            }),
        )
    }
}

fn policy_eval(mode: &str) -> Value {
    let mut policy = support::policy_eval();
    policy["requested"] = json!(mode);
    policy["effective"] = json!(mode);
    policy
}

#[test]
fn eta_is_late_past_the_upper_bound_and_on_hold_while_waiting() {
    let mut world = World::new();
    world.declare("eta", "Eta", 0);
    world.feature("eta", "a", 0);
    let attempt = world.start("eta", "a", "00000000000000000000000101", 600, 600, 0);
    let state = reduce_at(&world.events, at(20));
    let eta = project(&state, "eta").features[&node("a")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(
        eta.late_by_s,
        10 * 60,
        "ten minutes past the ten-minute bound"
    );
    assert!(eta.likely_finish.is_none());
    assert_eq!(eta.ratio_milli, None);

    // An open ask pauses the attempt: no active lateness while waiting.
    let _ask = world.ask("eta", "a", "00000000000000000000000101", "ask", 15);
    let state = reduce_at(&world.events, at(30));
    let eta = project(&state, "eta").features[&node("a")]
        .eta
        .clone()
        .expect("eta");
    assert!(eta.paused, "the attempt waits on a human");
    assert_eq!(eta.late_by_s, 0);
    assert_eq!(eta.paused_s, 15 * 60, "hold starts at the ask");
    assert_eq!(eta.agent_finish.latest, at(25));
    let _ = attempt;
}

#[test]
fn an_overdue_eta_nudges_only_its_own_session_until_it_re_estimates_or_finishes() {
    use agendar_core::model::SessionId;
    use agendar_core::reduce::nudges;
    let me = "00000000000000000000000101";
    let other = "00000000000000000000000102";
    let mut world = World::new();
    world.declare("eta", "Eta", 0);
    world.feature("eta", "a", 0);
    world.feature("eta", "b", 0);
    let attempt = world.start("eta", "a", me, 300, 600, 0);
    world.start("eta", "b", other, 3600, 7200, 0);
    let mine = SessionId::parse(me).expect("session");
    let theirs = SessionId::parse(other).expect("session");

    let state = reduce_at(&world.events, at(9));
    assert!(
        nudges::for_session(project(&state, "eta"), &mine).is_empty(),
        "inside the window"
    );

    let state = reduce_at(&world.events, at(25));
    let found = nudges::for_session(project(&state, "eta"), &mine);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].node, Some(node("a")));
    assert_eq!(found[0].attempt, Some(id(attempt)));
    assert_eq!(found[0].late_by_s, Some(15 * 60));
    assert_eq!(found[0].due_at, Some(at(10)));
    assert!(
        found[0].text.contains("passed 15m ago"),
        "{}",
        found[0].text
    );
    assert!(
        nudges::for_session(project(&state, "eta"), &theirs).is_empty(),
        "another session's step on time is not mine to hear about"
    );

    // A re-estimate anchors a new window: the nudge is gone.
    world.reestimate("eta", "a", me, attempt, 1800, 25);
    let state = reduce_at(&world.events, at(30));
    assert!(nudges::for_session(project(&state, "eta"), &mine).is_empty());

    // Late again, then on hold for a human: no nudge while waiting.
    let _ask = world.ask("eta", "a", me, "ask", 60);
    let state = reduce_at(&world.events, at(70));
    assert!(nudges::for_session(project(&state, "eta"), &mine).is_empty());

    // Finished work is never overdue.
    world.done("eta", "b", other, attempt + 1, 200);
    let state = reduce_at(&world.events, at(240));
    assert!(nudges::for_session(project(&state, "eta"), &theirs).is_empty());
}

#[test]
fn a_never_started_step_drops_without_an_attempt_and_a_start_brings_it_back() {
    use agendar_core::model::Progress;
    let me = "00000000000000000000000101";
    let mut world = World::new();
    world.declare("plan", "Plan", 0);
    world.feature("plan", "a", 0);
    world.feature("plan", "b", 0);
    let drop = |world: &mut World, name: &str, minute: u64| {
        world.push(
            "plan",
            &format!("h-00000000000000000000000001/s-{me}"),
            minute,
            "work.progress",
            json!({"node": name}),
            json!({
                "agent": {"session": me, "harness": "generic"},
                "status": "dropped",
                "evidence": [],
            }),
        )
    };
    drop(&mut world, "a", 1);
    let state = reduce_at(&world.events, at(2));
    assert_eq!(
        project(&state, "plan").features[&node("a")].progress,
        Progress::Dropped
    );
    assert_eq!(project(&state, "plan").features[&node("a")].attempt, None);

    // A started step cannot leave the plan without naming its attempt.
    world.start("plan", "b", me, 600, 900, 3);
    drop(&mut world, "b", 4);
    let state = reduce_at(&world.events, at(5));
    assert_eq!(
        project(&state, "plan").features[&node("b")].progress,
        Progress::Doing
    );

    // Starting a dropped step brings it back.
    world.start("plan", "a", me, 600, 900, 6);
    let state = reduce_at(&world.events, at(7));
    assert_eq!(
        project(&state, "plan").features[&node("a")].progress,
        Progress::Doing
    );
}

#[test]
fn the_project_forecast_counts_open_steps_at_the_recent_pace() {
    use agendar_core::model::SessionId;
    use agendar_core::model::state::Liveness;
    use agendar_core::reduce::forecast::forecast;
    let me = "00000000000000000000000101";
    let mut world = World::new();
    world.declare("pace", "Pace", 0);
    for name in ["a", "b", "c", "d"] {
        world.feature("pace", name, 0);
    }
    let done = world.start("pace", "d", me, 600, 600, 0);
    world.done("pace", "d", me, done, 10);
    world.start("pace", "a", me, 600, 1200, 20);
    let minutes = |m: u64| m * 60_000;
    let durations = [minutes(10), minutes(20), minutes(30)];

    // Nobody working (the session went quiet): work left, no clock time.
    let mut state = reduce_at(&world.events, at(25));
    state
        .projects
        .get_mut(&slug("pace"))
        .expect("project")
        .features
        .get_mut(&node("a"))
        .expect("a")
        .liveness = Liveness::Quiet;
    let idle = forecast(project(&state, "pace"), &durations, at(25)).expect("forecast");
    assert_eq!(idle.open_steps, 3, "a in progress, b and c not started");
    assert_eq!(idle.agents, 0);
    assert_eq!(idle.finish, None);
    assert_eq!(idle.step_ms, [minutes(10), minutes(30)]);
    assert_eq!(idle.work_ms, [3 * minutes(10), 3 * minutes(30)]);

    // A live agent on `a`: its own window counts, then b and c at the pace.
    let state = reduce_at(&world.events, at(25));
    assert_eq!(
        project(&state, "pace").features[&node("a")].session,
        Some(SessionId::parse(me).expect("session"))
    );
    let running = forecast(project(&state, "pace"), &durations, at(25)).expect("forecast");
    assert_eq!(running.agents, 1);
    // a: started at 20 with 10..20 min, now 25 → 5..15 min left.
    assert_eq!(
        running.work_ms,
        [minutes(5) + 2 * minutes(10), minutes(15) + 2 * minutes(30)]
    );
    let finish = running.finish.expect("clock time while working");
    assert_eq!(finish.earliest, at(25 + 5 + 20));
    assert_eq!(finish.latest, at(25 + 15 + 60));

    // Too few finished steps: no forecast at all.
    assert!(forecast(project(&state, "pace"), &durations[..2], at(25)).is_none());
}

#[test]
fn an_agent_may_archive_but_a_human_archived_value_wins() {
    let me = "00000000000000000000000101";
    let mut world = World::new();
    world.declare("arch", "Archive", 0);
    let agent_archive = |world: &mut World, value: bool, minute: u64| {
        world.push(
            "arch",
            &format!("h-00000000000000000000000001/s-{me}"),
            minute,
            "project.declared",
            json!({}),
            json!({"agent": {"session": me, "harness": "generic"}, "archived": value}),
        );
    };
    agent_archive(&mut world, true, 1);
    let state = reduce_at(&world.events, at(2));
    assert!(
        project(&state, "arch").summary.archived,
        "the agent archived it"
    );

    world.push(
        "arch",
        support::OPERATOR,
        3,
        "project.declared",
        json!({}),
        json!({"archived": false}),
    );
    agent_archive(&mut world, true, 4);
    let state = reduce_at(&world.events, at(5));
    assert!(
        !project(&state, "arch").summary.archived,
        "the human's unarchive outranks a later agent archive"
    );
}

#[test]
fn eta_lateness_uses_the_calibrated_bound_once_the_project_is_eligible() {
    let mut world = World::new();
    world.declare("cal", "Cal", 0);
    // Five distinct features, each taking twice its 600 s estimate.
    for index in 0..5 {
        let name = format!("f{index}");
        world.feature("cal", &name, 0);
        let attempt = world.start(
            "cal",
            &name,
            "00000000000000000000000101",
            600,
            600,
            index * 60 + 1,
        );
        world.done(
            "cal",
            &name,
            "00000000000000000000000101",
            attempt,
            index * 60 + 21,
        );
    }
    world.feature("cal", "g", 0);
    world.start("cal", "g", "00000000000000000000000101", 600, 600, 600);

    let state = reduce_at(&world.events, at(615));
    let feature = &project(&state, "cal").features[&node("g")];
    let eta = feature.eta.as_ref().expect("eta");
    assert_eq!(eta.ratio_milli, Some(2000));
    assert_eq!(
        eta.likely_finish.as_ref().expect("likely").latest,
        at(620),
        "the calibrated bound doubles the agent's ten-minute claim"
    );
    assert_eq!(
        eta.late_by_s, 0,
        "past the agent bound, inside the likely bound"
    );

    let state = reduce_at(&world.events, at(630));
    let eta = project(&state, "cal").features[&node("g")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.late_by_s, 10 * 60, "late against the calibrated bound");
    let calibration = &project(&state, "cal").summary.calibration;
    assert_eq!(calibration.len(), 1);
    assert_eq!(json!(calibration[0].harness), json!("generic"));
    assert_eq!(calibration[0].samples, 5);
    assert_eq!(calibration[0].ratio_milli, 2000);
}

#[test]
fn a_finally_completed_upstream_keeps_its_historical_wait() {
    // x starts depending on y; y asks, is answered and finishes.
    // x kept 60 paused seconds even though y is finally done.
    let mut world = World::new();
    world.declare("s8", "S8", 0);
    world.feature("s8", "y", 0);
    world.feature_depending("s8", "x", "y", 0);
    let _y = world.start("s8", "y", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("s8", "y", "00000000000000000000000101", "ask", 2);
    world.answer("s8", "y", ask, "a", 3);
    world.done("s8", "y", "00000000000000000000000101", _y, 4);
    world.start("s8", "x", "00000000000000000000000101", 600, 600, 1);
    let state = reduce_at(&world.events, at(10));
    let eta = project(&state, "s8").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.paused_s, 60);
    assert!(!eta.paused);
}

#[test]
fn a_restarted_upstream_reopens_the_decision_hold() {
    // B1: y completed once and restarted while an ask stayed open. The current
    // attempt has not finished, so x is still waiting on the human; the old
    // completion must not close the hold.
    let mut world = World::new();
    world.declare("b1a", "B1A", 0);
    world.feature("b1a", "y", 0);
    world.feature_depending("b1a", "x", "y", 0);
    let first = world.start("b1a", "y", "00000000000000000000000101", 600, 600, 1);
    world.done("b1a", "y", "00000000000000000000000101", first, 2);
    world.restart("b1a", "y", "00000000000000000000000101", first, 3);
    world.start("b1a", "x", "00000000000000000000000102", 600, 600, 4);
    world.ask("b1a", "y", "00000000000000000000000101", "ask", 5);

    let state = reduce_at(&world.events, at(20));
    assert!(
        state.diagnostics.is_empty(),
        "a cited restart is not a conflict: {:?}",
        state.diagnostics
    );
    let feature = &project(&state, "b1a").features[&node("x")];
    assert_eq!(feature.gate, Gate::AwaitingUpstream);
    let eta = feature.eta.as_ref().expect("eta");
    assert!(
        eta.paused,
        "the open ask on the restarted attempt still holds x"
    );
    assert_eq!(eta.paused_s, 15 * 60, "held from the ask at 5 to as_of 20");
    assert_eq!(eta.late_by_s, 0, "a held attempt is not actively late");
}

#[test]
fn removing_a_dependency_keeps_the_wait_that_was_already_spent() {
    // B1: an operator edit that drops x→y must not erase the human wait x
    // already paid while the edge was accepted.
    let mut world = World::new();
    world.declare("b1b", "B1B", 0);
    world.feature("b1b", "y", 0);
    world.feature_depending("b1b", "x", "y", 0);
    world.start("b1b", "x", "00000000000000000000000102", 600, 600, 1);
    world.start("b1b", "y", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("b1b", "y", "00000000000000000000000101", "ask", 2);
    world.answer("b1b", "y", ask, "a", 8);

    let before = reduce_at(&world.events, at(20));
    let eta = project(&before, "b1b").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.paused_s, 6 * 60, "held from 2 to 8");
    assert_eq!(eta.late_by_s, 3 * 60, "late after the shifted upper bound");

    world.depends("b1b", "x", &[], 10);
    let after = reduce_at(&world.events, at(20));
    let eta = project(&after, "b1b").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(
        eta.paused_s,
        6 * 60,
        "a later agenda edit does not rewrite spent wait time"
    );
    assert_eq!(eta.late_by_s, 3 * 60);
}

#[test]
fn adding_a_dependency_does_not_backdate_a_wait() {
    // B1: x was not waiting on y before the edge existed, so an ask opened
    // earlier only starts holding x once the edge is accepted.
    let mut world = World::new();
    world.declare("b1d", "B1D", 0);
    world.feature("b1d", "x", 0);
    world.feature("b1d", "y", 0);
    world.start("b1d", "x", "00000000000000000000000102", 600, 600, 1);
    world.start("b1d", "y", "00000000000000000000000101", 600, 600, 1);
    world.ask("b1d", "y", "00000000000000000000000101", "ask", 2);

    let before = reduce_at(&world.events, at(10));
    let eta = project(&before, "b1d").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.paused_s, 0, "no accepted edge means no upstream hold");

    world.depends("b1d", "x", &["y"], 12);
    let after = reduce_at(&world.events, at(20));
    let eta = project(&after, "b1d").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert!(eta.paused, "the open ask holds x once the edge is accepted");
    assert_eq!(
        eta.paused_s,
        8 * 60,
        "held from the edge at 12, not the ask at 2"
    );
}

#[test]
fn an_agenda_edit_cannot_rewrite_completed_attempt_calibration() {
    // B1: the replay keeps the wait that belongs to x's finished attempt, so
    // the calibration sample is identical before and after the dependency edit.
    let mut world = World::new();
    world.declare("b1c", "B1C", 0);
    world.feature("b1c", "y", 0);
    world.feature_depending("b1c", "x", "y", 0);
    let x = world.start("b1c", "x", "00000000000000000000000102", 600, 600, 1);
    world.start("b1c", "y", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("b1c", "y", "00000000000000000000000101", "ask", 2);
    world.answer("b1c", "y", ask, "a", 8);
    world.done("b1c", "x", "00000000000000000000000102", x, 20);

    let (before, report) = reduce_with_report(&world.events, &ReduceContext { as_of: at(30) });
    let sample = report.calibration[&slug("b1c")]
        .samples
        .iter()
        .find(|sample| sample.attempt == id(x))
        .expect("x is a calibration sample");
    assert_eq!(
        sample.actual_ms, 780_000,
        "19 active minutes minus 6 waited"
    );
    assert_eq!(sample.ratio_milli, 1300);
    let eta = project(&before, "b1c").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.paused_s, 6 * 60);

    world.depends("b1c", "x", &[], 25);
    let (after, report) = reduce_with_report(&world.events, &ReduceContext { as_of: at(30) });
    let sample = report.calibration[&slug("b1c")]
        .samples
        .iter()
        .find(|sample| sample.attempt == id(x))
        .expect("x is still a calibration sample");
    assert_eq!(sample.actual_ms, 780_000, "completed history is unchanged");
    assert_eq!(sample.ratio_milli, 1300);
    let eta = project(&after, "b1c").features[&node("x")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.paused_s, 6 * 60);
}

// ---------------------------------------------------------------------------
// Speculation budget
// ---------------------------------------------------------------------------

#[test]
fn budget_usage_counts_unconfirmed_assumptions_only() {
    let mut world = World::new();
    world.declare("budget", "Budget", 0);
    world.feature("budget", "a", 0);
    let attempt = world.start("budget", "a", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("budget", "a", "00000000000000000000000101", "assume", 2);
    world.assume("budget", "a", "00000000000000000000000101", ask, attempt, 3);

    let state = reduce_at(&world.events, at(10));
    let project_state = project(&state, "budget");
    assert_eq!(
        project_state.features[&node("a")].assumptions,
        vec![id(ask + 1)]
    );
    assert_eq!(project_state.summary.budget.speculative_features, 1);
    assert!(!project_state.summary.budget.blocked);

    // A human answer matching the recommendation confirms the assumption.
    world.answer("budget", "a", ask, "a", 4);
    let state = reduce_at(&world.events, at(10));
    let project_state = project(&state, "budget");
    assert_eq!(
        project_state.summary.budget.speculative_features, 0,
        "a matching human answer confirms the assumption without changing progress"
    );
    assert_eq!(
        project_state.features[&node("a")].progress.as_str(),
        "doing"
    );
    // A confirmed feature is no longer speculative, so at the cap it is a new
    // admission and must be refused instead of silently reusing an old slot.
    let confirmed = budget::speculative_nodes(
        &project_state.features,
        &project_state.decisions,
        &project_state.receipts,
    );
    assert!(confirmed.is_empty());
    let at_cap = budget::Activity {
        speculative_features: 10,
        pending_slices: 0,
        demo_gap_s: 0,
        demo_step: None,
    };
    assert!(
        budget::admit_speculation(
            &agendar_core::reduce::policy::DEFAULT_BUDGET,
            at_cap,
            &node("a"),
            &confirmed
        )
        .is_err(),
        "a confirmed feature counts as a new speculative admission"
    );

    // A different choice keeps it unconfirmed.
    let mut other = World::new();
    other.declare("other", "Other", 0);
    other.feature("other", "a", 0);
    let attempt = other.start("other", "a", "00000000000000000000000101", 600, 600, 1);
    let ask = other.ask("other", "a", "00000000000000000000000101", "assume", 2);
    other.assume("other", "a", "00000000000000000000000101", ask, attempt, 3);
    other.answer("other", "a", ask, "b", 4);
    let state = reduce_at(&other.events, at(10));
    assert_eq!(
        project(&state, "other").summary.budget.speculative_features,
        1,
        "a disagreeing human answer leaves the assumption unconfirmed"
    );
}

#[test]
fn budget_usage_and_admission_follow_the_defaults() {
    use agendar_core::reduce::policy::DEFAULT_BUDGET;

    let clear = budget::Activity {
        speculative_features: 10,
        pending_slices: 1,
        demo_gap_s: 3_600,
        demo_step: None,
    };
    let usage = budget::usage(&DEFAULT_BUDGET, clear);
    assert!(!usage.blocked, "exactly ten is at the cap, not over it");
    assert!(usage.reasons.is_empty());
    assert!(budget::refuse_new_speculation(&DEFAULT_BUDGET, clear, 10).is_none());
    let refused = budget::refuse_new_speculation(&DEFAULT_BUDGET, clear, 11)
        .expect("the eleventh speculative feature is refused");
    assert_eq!(refused.code(), agendar_core::ErrorCode::Budget);
    assert!(refused.message().contains("11 features"));

    let pending = budget::Activity {
        speculative_features: 0,
        pending_slices: 2,
        demo_gap_s: 0,
        demo_step: None,
    };
    assert!(budget::usage(&DEFAULT_BUDGET, pending).blocked);
    assert!(budget::refuse_new_speculation(&DEFAULT_BUDGET, pending, 1).is_some());
    assert!(
        budget::usage(&DEFAULT_BUDGET, pending)
            .reasons
            .iter()
            .any(|reason| reason.contains("await review"))
    );

    let stale = budget::Activity {
        speculative_features: 11,
        pending_slices: 0,
        demo_gap_s: 14_400,
        demo_step: None,
    };
    let usage = budget::usage(&DEFAULT_BUDGET, stale);
    assert!(usage.blocked);
    assert_eq!(usage.reasons.len(), 2, "both over-cap counts are reported");
    assert!(usage.reasons.iter().any(|reason| reason.contains("4h 0m")));

    // Exactly the cap is admitted; a further unrelated feature is not.
    let mut world = World::new();
    world.declare("cap", "Cap", 0);
    world.feature("cap", "a", 0);
    let attempt = world.start("cap", "a", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("cap", "a", "00000000000000000000000101", "assume", 2);
    world.assume("cap", "a", "00000000000000000000000101", ask, attempt, 3);
    let state = reduce_at(&world.events, at(10));
    let project_state = project(&state, "cap");
    let speculative = budget::speculative_nodes(
        &project_state.features,
        &project_state.decisions,
        &project_state.receipts,
    );
    let full = budget::Activity {
        speculative_features: 9,
        pending_slices: 0,
        demo_gap_s: 0,
        demo_step: None,
    };
    assert!(
        budget::admit_speculation(&DEFAULT_BUDGET, full, &node("b"), &speculative).is_ok(),
        "nine plus a new feature reaches the cap"
    );
    let at_cap = budget::Activity {
        speculative_features: 10,
        pending_slices: 0,
        demo_gap_s: 0,
        demo_step: None,
    };
    assert!(
        budget::admit_speculation(&DEFAULT_BUDGET, at_cap, &node("b"), &speculative).is_err(),
        "the eleventh fails"
    );
    assert!(
        budget::admit_speculation(&DEFAULT_BUDGET, at_cap, &node("a"), &speculative).is_ok(),
        "work on an already speculative feature is not a new admission"
    );
}

#[test]
fn a_dropped_attempt_stops_counting_against_the_speculation_cap() {
    let mut world = World::new();
    world.declare("drop", "Drop", 0);
    world.feature("drop", "a", 0);
    let attempt = world.start("drop", "a", "00000000000000000000000101", 600, 600, 1);
    let ask = world.ask("drop", "a", "00000000000000000000000101", "assume", 2);
    world.assume("drop", "a", "00000000000000000000000101", ask, attempt, 3);
    let state = reduce_at(&world.events, at(4));
    assert_eq!(
        project(&state, "drop").summary.budget.speculative_features,
        1
    );

    world.dropped("drop", "a", "00000000000000000000000101", attempt, 5);
    let state = reduce_at(&world.events, at(6));
    let project_state = project(&state, "drop");
    assert_eq!(
        project_state.features[&node("a")].progress.as_str(),
        "dropped"
    );
    assert_eq!(
        project_state.summary.budget.speculative_features, 0,
        "dropped work stops counting even though the attempt still carries the receipt"
    );
    assert!(!project_state.summary.budget.blocked);
    let speculative = budget::speculative_nodes(
        &project_state.features,
        &project_state.decisions,
        &project_state.receipts,
    );
    assert!(speculative.is_empty());
}

#[test]
fn the_budget_records_the_doing_step_started_first_as_the_demo_clock_step() {
    let mut world = World::new();
    world.declare("gap", "Gap", 0);
    world.feature("gap", "a", 0);
    world.feature("gap", "b", 0);
    world.feature("gap", "c", 0);
    // Nothing started: no step, and the key is left out of the JSON.
    let idle = reduce_at(&world.events, at(300));
    let usage = &project(&idle, "gap").summary.budget;
    assert_eq!(usage.demo_gap_node, None);
    let wire = serde_json::to_value(usage).expect("json");
    assert!(wire.get("demo_gap_node").is_none());

    // `b` starts before `a`: the earliest start wins, not the node order.
    world.start("gap", "b", "00000000000000000000000101", 600, 600, 1);
    let later = world.start("gap", "a", "00000000000000000000000102", 600, 600, 2);
    let state = reduce_at(&world.events, at(300));
    let usage = &project(&state, "gap").summary.budget;
    assert_eq!(usage.demo_gap_node, Some(node("b")));
    assert_eq!(
        serde_json::to_value(usage).expect("json")["demo_gap_node"],
        json!("b")
    );
    assert_eq!(
        usage.reasons,
        vec![
            "No demo for 4h 59m (limit 4h 0m): b (b) is started, not delivered; agendar deliver on it clears the clock."
        ]
    );
    // Named below the limit too.
    let early = reduce_at(&world.events, at(30));
    let usage = &project(&early, "gap").summary.budget;
    assert!(!usage.blocked);
    assert_eq!(usage.demo_gap_node, Some(node("b")));

    // A state file written before the field existed still reads.
    let mut old = serde_json::to_value(usage).expect("json");
    old.as_object_mut().expect("object").remove("demo_gap_node");
    let old: agendar_core::BudgetUsage = serde_json::from_value(old).expect("old usage reads");
    assert_eq!(old.demo_gap_node, None);

    // The brief's catalog message carries the step's title.
    let brief = agendar_core::reduce::brief::build(&state, &world.events, None);
    let row = brief
        .at_risk
        .iter()
        .find(|row| row.id == "at_risk:budget:gap")
        .expect("over budget row");
    let detail = serde_json::to_value(&row.detail_msg).expect("json");
    assert_eq!(
        detail["args"]["reasons"]["v"][0],
        json!({
            "key": "budget.demo_gap",
            "args": {
                "gap": {"t": "minutes", "v": 299},
                "limit": {"t": "minutes", "v": 240},
                "step": {"t": "text", "v": "b"},
            },
        })
    );

    // Finished without a delivery, `a` is no longer in flight; `b` stays.
    world.done("gap", "a", "00000000000000000000000102", later, 3);
    let state = reduce_at(&world.events, at(300));
    assert_eq!(
        project(&state, "gap").summary.budget.demo_gap_node,
        Some(node("b"))
    );
}

#[test]
fn the_nudge_and_the_refusal_name_the_step_and_say_to_deliver() {
    use agendar_core::reduce::policy::DEFAULT_BUDGET;

    let step = node("core/search");
    let attempt = id(77);
    let named = budget::Activity {
        speculative_features: 0,
        pending_slices: 0,
        demo_gap_s: 18_000,
        demo_step: Some(budget::DemoStep {
            node: &step,
            title: "Search index",
            attempt: Some(&attempt),
        }),
    };
    let reason = "No demo for 5h 0m (limit 4h 0m): Search index (core/search) is started, not delivered; agendar deliver on it clears the clock.";
    let usage = budget::usage(&DEFAULT_BUDGET, named);
    assert_eq!(usage.reasons, vec![reason]);
    assert_eq!(usage.demo_gap_node, Some(step.clone()));

    let refused = budget::refuse_new_speculation(&DEFAULT_BUDGET, named, 1).expect("refused");
    assert_eq!(
        refused.message(),
        format!("Speculation budget exceeded: {reason}")
    );
    assert_eq!(
        refused.hint(),
        Some(
            format!(
                "Deliver what works so far: agendar deliver --node core/search --attempt {} --json delivery.json",
                attempt.as_str()
            )
            .as_str()
        )
    );

    // No step in progress: the plain reason and the general hint.
    let unnamed = budget::Activity {
        demo_step: None,
        ..named
    };
    let usage = budget::usage(&DEFAULT_BUDGET, unnamed);
    assert_eq!(usage.reasons, vec!["No demo for 5h 0m (limit 4h 0m)."]);
    assert_eq!(usage.demo_gap_node, None);
    let general = "Ask the human or work on a feature without unconfirmed choices.";
    let refused = budget::refuse_new_speculation(&DEFAULT_BUDGET, unnamed, 1).expect("refused");
    assert_eq!(refused.hint(), Some(general));

    // Refused for another reason while under the demo limit: the deliver hint
    // would not clear it, so the general hint stays.
    let under = budget::Activity {
        demo_gap_s: 60,
        ..named
    };
    let refused = budget::refuse_new_speculation(&DEFAULT_BUDGET, under, 11).expect("refused");
    assert_eq!(refused.hint(), Some(general));
}

#[test]
fn pending_slices_and_demo_gap_project_from_a_delivery_seed() {
    let seed = support::load_seed("delivery-seen-review");
    // Prefix through the second observation: the delivery is still pending.
    let prefix = seed
        .events
        .iter()
        .take_while(|event| event.id.as_str() != seed.alias("observation-1"))
        .cloned()
        .chain(std::iter::once(
            seed.events
                .iter()
                .find(|event| event.id.as_str() == seed.alias("observation-1"))
                .expect("observation")
                .clone(),
        ))
        .collect::<Vec<_>>();
    let state = reduce_at(&prefix, seed.as_of);
    let usage = &project(&state, "example").summary.budget;
    assert_eq!(
        usage.pending_slices, 1,
        "one (slice, receipt), not one per demo"
    );
    assert_eq!(usage.speculative_features, 0);
    // The attempt is doing and the last delivery was at 00:04.
    assert_eq!(usage.demo_gap_s, 11 * 3600 + 56 * 60);
    assert!(
        usage.blocked,
        "eleven hours without a demo exceeds the four-hour budget"
    );
    assert!(
        usage
            .reasons
            .iter()
            .any(|reason| reason.contains("No demo"))
    );
}

// ---------------------------------------------------------------------------
// Agenda precedence
// ---------------------------------------------------------------------------

#[test]
fn an_agent_plan_patch_never_overrides_a_human_agenda_field() {
    let mut world = World::new();
    world.declare("agenda", "Agenda", 0);
    world.feature("agenda", "go", 0);
    // Human edits: title, priority and deadline.
    world.push(
        "agenda",
        support::OPERATOR,
        1,
        "plan.feature",
        json!({"node": "go"}),
        json!({"title": "Human title", "priority": 0, "deadline": {"op": "set", "value": at(600)}}),
    );
    // Agent tries the same fields and adds one no human ever wrote.
    world.push(
        "agenda",
        "h-00000000000000000000000001/s-00000000000000000000000101",
        2,
        "plan.feature",
        json!({"node": "go"}),
        json!({
            "agent": {"session": "00000000000000000000000101", "harness": "generic"},
            "title": "Agent title",
            "priority": 3,
            "milestone": "M1",
        }),
    );
    let state = reduce_at(&world.events, at(10));
    let feature = &project(&state, "agenda").features[&node("go")];
    assert_eq!(feature.title, "Human title");
    assert_eq!(feature.priority, 0);
    assert_eq!(feature.deadline, Some(at(600)));
    assert_eq!(
        feature.milestone.as_deref(),
        Some("M1"),
        "an unowned field still accepts the agent's patch"
    );
    assert_eq!(feature.field_sources["title"].as_str(), uid(1002));
}

#[test]
fn update_feature_guard_tokens_change_with_the_winning_source() {
    use agendar_core::model::{feature_field_revision, feature_field_revisions};

    let mut world = World::new();
    world.declare("tokens", "Tokens", 0);
    let declared = world.feature("tokens", "go", 0);
    let state = reduce_at(&world.events, at(10));
    let feature = &project(&state, "tokens").features[&node("go")];
    let revisions = feature_field_revisions(&slug("tokens"), feature);
    assert_eq!(
        revisions["title"],
        feature_field_revision(&slug("tokens"), &node("go"), "title", Some(&id(declared)))
    );
    assert_eq!(
        revisions.len(),
        agendar_core::model::FEATURE_FIELDS.len(),
        "unset fields still get a defined absent-token"
    );
    assert_ne!(revisions["title"], revisions["deadline"]);
    let absent = feature_field_revision(&slug("tokens"), &node("go"), "deadline", None);
    assert_eq!(revisions["deadline"], absent);
}

// ---------------------------------------------------------------------------
// Project ordering and the Brief
// ---------------------------------------------------------------------------

#[test]
fn projects_order_by_variance_then_earliest_need_then_slug() {
    let mut world = World::new();
    // Quiet project.
    world.declare("quiet", "Quiet", 0);
    world.feature("quiet", "a", 0);
    // Active project: doing work, recent.
    world.declare("active", "Active", 0);
    world.feature("active", "a", 0);
    world.start("active", "a", "00000000000000000000000101", 3600, 3600, 599);
    // No-demo project: doing work started five hours ago.
    world.declare("nada", "Nada", 0);
    world.feature("nada", "a", 0);
    world.start("nada", "a", "00000000000000000000000102", 3600, 3600, 1);
    let state = reduce_at(&world.events, at(600));

    let mut summaries: Vec<_> = state
        .projects
        .values()
        .map(|project| project.summary.clone())
        .collect();
    ordering::sort_projects(&mut summaries, &BTreeMap::new());
    let order: Vec<&str> = summaries
        .iter()
        .map(|summary| summary.slug.as_str())
        .collect();
    assert_eq!(order, vec!["nada", "active", "quiet"]);
    assert_eq!(summaries[0].variance, Variance::NoDemo);
    assert_eq!(summaries[1].variance, Variance::Active);
    assert_eq!(summaries[2].variance, Variance::Quiet);
}

fn merge(worlds: Vec<(State, Vec<Event>)>) -> (State, Vec<Event>) {
    let mut projects = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let mut events = Vec::new();
    let mut evaluated_at = at(0);
    for (state, mut state_events) in worlds {
        evaluated_at = state.evaluated_at;
        diagnostics.extend(state.diagnostics);
        projects.extend(state.projects);
        events.append(&mut state_events);
    }
    (
        State {
            projects,
            diagnostics,
            event_set_digest: Sha256::from_bytes(b"twelve-projects"),
            evaluated_at,
        },
        events,
    )
}

#[test]
fn brief_projects_rows_for_twelve_projects() {
    let mut worlds = vec![
        delivered_project(),
        needs_project(),
        default_project(),
        budget_project(),
        active_project(),
        late_project(),
    ];
    // Six quiet projects fill the board to twelve.
    for index in 6..12 {
        let mut world = World::new();
        let project = format!("q{index}");
        world.declare(&project, "Quiet", 0);
        world.feature(&project, "a", 0);
        worlds.push((reduce_at(&world.events, at(600)), world.events));
    }
    let (state, events) = merge(worlds);
    assert_eq!(state.projects.len(), 12);

    let before = agendar_core::reduce::brief::build(&state, &events, None);
    assert_eq!(before.delivered.len(), 1, "one accepted delivery");
    assert_eq!(before.delivered[0].target.kind, BriefTargetKind::Project);
    assert_eq!(before.delivered[0].target.id, "p01");
    assert!(before.coming_up.iter().any(|row| row.text == "ship-it"));
    assert!(
        before
            .needs_you
            .iter()
            .any(|row| row.text == "How should answers reach idle agents?")
    );

    // If you wait: the top decision and the one step downstream.
    let wait = &before.if_you_wait[0];
    assert_eq!(wait.text, "1 step wait on your answer");
    assert_eq!(wait.target.kind, BriefTargetKind::Need);
    assert!(!wait.source_ids.is_empty());

    // By default: the pending timeout with the recommended option.
    assert_eq!(before.by_default.len(), 1);
    assert_eq!(
        before.by_default[0].detail.as_deref(),
        Some("Then goes with Choose A")
    );
    assert_eq!(before.by_default[0].target.kind, BriefTargetKind::Need);

    // At risk: over budget, near budget and a late feature.
    let ids: Vec<&str> = before.at_risk.iter().map(|row| row.id.as_str()).collect();
    assert!(ids.contains(&"at_risk:budget:p06"));
    assert!(ids.contains(&"at_risk:near_budget:p04"));
    assert!(ids.iter().any(|id| id.starts_with("at_risk:late:p06")));
    // p01's only step was accepted by the owner: settled, so neither late nor
    // over its demo budget.
    assert!(!ids.iter().any(|id| id.starts_with("at_risk:late:p01")));
    assert!(!ids.contains(&"at_risk:budget:p01"));

    // Every row targets a project or an existing need.
    let need_ids: BTreeSet<&str> = before
        .needs_you
        .iter()
        .map(|row| row.target.id.as_str())
        .collect();
    for row in all_rows(&before) {
        match row.target.kind {
            BriefTargetKind::Project => assert!(state.projects.contains_key(&slug(&row.target.id))),
            BriefTargetKind::Need => {
                assert!(need_ids.contains(row.target.id.as_str()), "{}", row.id);
            }
        }
    }

    // "Since you last looked": the delivery is only new past the watermark.
    let seen = agendar_core::reduce::brief::build(&state, &events, Some(at(3)));
    assert_eq!(seen.delivered.len(), 1);
    let stale = agendar_core::reduce::brief::build(&state, &events, Some(at(4)));
    assert!(stale.delivered.is_empty());
    assert!(!stale.is_empty(), "later sections still show current state");
}

fn all_rows(brief: &BriefData) -> Vec<&agendar_core::reduce::brief::BriefRow> {
    brief
        .delivered
        .iter()
        .chain(&brief.coming_up)
        .chain(&brief.needs_you)
        .chain(&brief.if_you_wait)
        .chain(&brief.by_default)
        .chain(&brief.at_risk)
        .collect()
}

/// p01: the approved delivery seed, accepted at 00:11 and still doing.
fn delivered_project() -> (State, Vec<Event>) {
    let seed = support::load_seed("delivery-seen-review");
    let mut state = reduce_at(&seed.events, seed.as_of);
    let mut project_state = state.projects.remove(&slug("example")).expect("project");
    project_state.summary.slug = slug("p01");
    state.projects.insert(slug("p01"), project_state);
    (state, seed.events)
}

/// p02: an open ask on a feature with one dependent step.
fn needs_project() -> (State, Vec<Event>) {
    let mut world = World::new();
    world.declare("p02", "Needs", 0);
    world.feature("p02", "a", 0);
    world.feature_depending("p02", "b", "a", 0);
    world.start("p02", "a", "00000000000000000000000201", 600, 600, 1);
    world.ask("p02", "a", "00000000000000000000000201", "ask", 2);
    (reduce_at(&world.events, at(30)), world.events)
}

/// p03: an open ask with a timeout default and a completing attempt.
fn default_project() -> (State, Vec<Event>) {
    let mut world = World::new();
    world.declare("p03", "Default", 0);
    world.feature("p03", "a", 0);
    let attempt = world.start("p03", "a", "00000000000000000000000301", 600, 600, 1);
    world.push(
        "p03",
        "h-00000000000000000000000001/s-00000000000000000000000301",
        2,
        "decision.ask",
        json!({"node": "a"}),
        json!({
            "agent": {"session": "00000000000000000000000301", "harness": "generic"},
            "card": card_with_timeout(3600),
            "policy": policy_eval("ask"),
        }),
    );
    world.done("p03", "a", "00000000000000000000000301", attempt, 3);
    (reduce_at(&world.events, at(30)), world.events)
}

/// p04: eight features built on unconfirmed choices; no demo at all.
fn budget_project() -> (State, Vec<Event>) {
    let mut world = World::new();
    world.declare("p04", "Budget", 0);
    for index in 0..8 {
        let name = format!("s{index}");
        world.feature("p04", &name, 0);
        let session = format!("000000000000000000000004{index:02}");
        let attempt = world.start("p04", &name, &session, 600, 600, 1);
        let ask = world.ask("p04", &name, &session, "assume", 2);
        world.assume("p04", &name, &session, ask, attempt, 3);
    }
    (reduce_at(&world.events, at(60)), world.events)
}

/// p05: active, fresh work with no demo.
fn active_project() -> (State, Vec<Event>) {
    let mut world = World::new();
    world.declare("p05", "Active", 0);
    world.feature("p05", "ship-it", 0);
    world.start("p05", "ship-it", "00000000000000000000000501", 600, 600, 11);
    (reduce_at(&world.events, at(15)), world.events)
}

/// p06: one step started long ago, far past its ETA, with no demo since.
fn late_project() -> (State, Vec<Event>) {
    let mut world = World::new();
    world.declare("p06", "Late", 0);
    world.feature("p06", "slow", 0);
    world.start("p06", "slow", "00000000000000000000000601", 600, 600, 1);
    (reduce_at(&world.events, at(600)), world.events)
}

fn card_with_timeout(after_s: u64) -> Value {
    let mut card = support::card();
    card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": after_s});
    card
}

#[test]
fn brief_sections_serialize_to_the_wire_shape() {
    let (state, events) = merge(vec![needs_project()]);
    let brief = agendar_core::reduce::brief::build(&state, &events, None);
    let value = serde_json::to_value(&brief).expect("brief serializes");
    for section in [
        "delivered",
        "coming_up",
        "needs_you",
        "if_you_wait",
        "by_default",
        "at_risk",
    ] {
        assert!(value[section].is_array(), "{section}");
    }
    let row = &value["needs_you"][0];
    assert_eq!(row["target"]["kind"], "need");
    assert!(row["source_ids"].is_array());
    assert!(row.get("detail").is_some());
    assert!(row.get("id").is_some());
    assert!(row.get("text").is_some());

    // The deferred-decision rows carry no delivery-only fields.
    let empty = agendar_core::reduce::brief::build(&state, &events, None);
    assert!(empty.delivered.is_empty());
}

#[test]
fn instant_completions_count_as_finished_but_never_as_calibration_samples() {
    // An import (or a report of work done elsewhere) starts and finishes in the
    // same minute: it measures bookkeeping, so it must not drag the ratio.
    let mut world = World::new();
    world.declare("imp", "Imported", 0);
    for index in 0..5 {
        let name = format!("f{index}");
        world.feature("imp", &name, 0);
        let attempt = world.start(
            "imp",
            &name,
            "00000000000000000000000101",
            600,
            1800,
            index + 1,
        );
        world.done(
            "imp",
            &name,
            "00000000000000000000000101",
            attempt,
            index + 1,
        );
    }
    world.feature("imp", "g", 0);
    world.start("imp", "g", "00000000000000000000000101", 600, 600, 30);

    let (state, report) = reduce_with_report(&world.events, &ReduceContext { as_of: at(35) });
    let calibration = &report.calibration[&slug("imp")];
    assert_eq!(
        calibration.finished_features, 5,
        "imports still finish their features"
    );
    assert!(
        calibration.samples.is_empty(),
        "sub-minute completions are not samples"
    );
    assert!(calibration.calibration.is_empty());
    let eta = project(&state, "imp").features[&node("g")]
        .eta
        .clone()
        .expect("eta");
    assert_eq!(eta.ratio_milli, None, "no ratio rests on bookkeeping");
    assert!(eta.likely_finish.is_none());
}

#[test]
fn header_stats_count_timed_steps_and_leave_out_instant_ones() {
    let mut world = World::new();
    world.declare("pace", "Pace", 0);
    // Two real steps of 20 and 40 minutes, one instant import.
    world.feature("pace", "a", 0);
    let a = world.start("pace", "a", "00000000000000000000000101", 600, 1800, 10);
    world.done("pace", "a", "00000000000000000000000101", a, 30);
    world.feature("pace", "b", 0);
    let b = world.start("pace", "b", "00000000000000000000000101", 600, 1800, 30);
    world.done("pace", "b", "00000000000000000000000101", b, 70);
    world.feature("pace", "c", 0);
    let c = world.start("pace", "c", "00000000000000000000000101", 600, 1800, 70);
    world.done("pace", "c", "00000000000000000000000101", c, 70);

    let state = reduce_at(&world.events, at(80));
    let events: Vec<_> = world
        .events
        .iter()
        .filter(|event| event.project.as_str() == "pace")
        .cloned()
        .collect();
    let stats = agendar_core::reduce::stats::build(project(&state, "pace"), &events, at(80));
    assert_eq!(stats.finished_24h, 2);
    assert_eq!(stats.timed_steps, 2);
    assert_eq!(stats.instant_excluded, 1);
    assert_eq!(
        stats.agent_ms_24h,
        60 * 60 * 1000,
        "20 + 40 minutes of wall time"
    );
    assert_eq!(stats.median_step_ms, Some(30 * 60 * 1000));
    assert_eq!(stats.tokens, None, "no harness reports tokens yet");
}

#[test]
fn harnesses_with_five_timed_samples_each_calibrate_in_first_sample_order() {
    let mut world = World::new();
    world.declare("two", "Two", 0);
    // Codex finishes first, five times at twice its estimate; then generic five
    // times on time. Each harness gets its own ratio; codex is listed first.
    for (round, harness) in [(0_u64, "codex"), (1, "generic")] {
        world.harness = harness;
        for index in 0..5 {
            let name = format!("{harness}{index}");
            world.feature("two", &name, 0);
            let start = round * 300 + index * 30 + 1;
            let attempt = world.start("two", &name, "00000000000000000000000101", 600, 600, start);
            let minutes = if harness == "codex" { 20 } else { 10 };
            world.done(
                "two",
                &name,
                "00000000000000000000000101",
                attempt,
                start + minutes,
            );
        }
    }
    let state = reduce_at(&world.events, at(700));
    let calibration = &project(&state, "two").summary.calibration;
    assert_eq!(calibration.len(), 2);
    assert_eq!(
        json!(calibration[0].harness),
        json!("codex"),
        "first sample first"
    );
    assert_eq!(calibration[0].ratio_milli, 2000);
    assert_eq!(json!(calibration[1].harness), json!("generic"));
    assert_eq!(calibration[1].ratio_milli, 1000);
}
