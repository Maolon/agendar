//! `agendar wait`: a real blocked process, bounded wake queries and deadlines.

mod support;

use std::fs;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use agendar_core::error::ErrorCode;
use agendar_core::model::EventId;
use agendar_core::service::{WaitEvent, WaitObserver, WaitOptions};

use support::{CliEnv, expect_error, expect_success};

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("serialize")).expect("write json");
    path.to_string_lossy().into_owned()
}

fn prepare(env: &mut CliEnv) {
    env.init();
    let project = json!({
        "title": "Wait project",
        "brief": "Scratch wait board.",
        "goal": "Wake the blocked child.",
        "glyph": "radar"
    });
    let project = write_json(env, "project.json", &project);
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    let feature = json!({
        "title": "Blocked child",
        "slice": "core/wake",
        "demo_kind": "cli_transcript"
    });
    let feature = write_json(env, "feature.json", &feature);
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            "core/wake",
            "--json",
            &feature,
        ]),
        "feature declare",
    );
}

fn ask(env: &CliEnv) -> String {
    let card: Value =
        serde_json::from_slice(&fs::read(CliEnv::fixture("ask.json")).expect("ask fixture"))
            .expect("ask json");
    let path = write_json(env, "ask.json", &card);
    let value = expect_success(
        &env.run(&[
            "ask",
            "--node",
            "core/wake",
            "--mode",
            "ask",
            "--json",
            &path,
        ]),
        "ask",
    );
    value["data"]["ask_id"].as_str().unwrap().to_owned()
}

fn spawn_wait(env: &CliEnv, ask_id: &str, timeout: &str) -> std::process::Child {
    let mut command: Command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg(&env.project)
        .arg("--session")
        .arg(&env.session)
        .arg("wait")
        .arg(ask_id)
        .arg("--timeout")
        .arg(timeout)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.spawn().expect("spawn agendar wait")
}

fn is_decision_query(event: &WaitEvent) -> bool {
    matches!(
        event,
        WaitEvent::QueryInitial
            | WaitEvent::QueryInvalidation
            | WaitEvent::QueryReconciliation
            | WaitEvent::QueryDeadline
    )
}

fn queries_in(events: &[WaitEvent]) -> usize {
    events
        .iter()
        .filter(|event| is_decision_query(event))
        .count()
}

#[derive(Default)]
struct Recorder {
    events: Vec<WaitEvent>,
}

impl WaitObserver for Recorder {
    fn observed(&mut self, event: WaitEvent) {
        self.events.push(event);
    }
}

impl Recorder {
    fn queries(&self) -> usize {
        queries_in(&self.events)
    }
}

/// A recorder the test thread can wait on: the wait loop publishes every event,
/// so a test can synchronise on an observed decision query instead of sleeping
/// for a guessed duration. Every wait is bounded and fails instead of hanging.
#[derive(Clone, Default)]
struct SharedRecorder {
    recorder: Arc<Mutex<Recorder>>,
    published: Arc<Condvar>,
}

impl WaitObserver for SharedRecorder {
    fn observed(&mut self, event: WaitEvent) {
        let mut recorder = self.recorder.lock().expect("recorder lock");
        recorder.events.push(event);
        drop(recorder);
        self.published.notify_all();
    }
}

impl SharedRecorder {
    /// Block until `count` decision queries were observed, then return the
    /// events seen so far. `within` is a hard bound: a missing fact fails the
    /// test with the recorded events instead of extending the wait.
    fn wait_for_queries(&self, count: usize, within: Duration) -> Vec<WaitEvent> {
        let deadline = Instant::now() + within;
        let mut recorder = self.recorder.lock().expect("recorder lock");
        loop {
            if recorder.queries() >= count {
                return recorder.events.clone();
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "expected {count} decision queries within {within:?}; events={:?}",
                recorder.events
            );
            recorder = self
                .published
                .wait_timeout(recorder, deadline.saturating_duration_since(now))
                .expect("recorder wait")
                .0;
        }
    }

    fn events(&self) -> Vec<WaitEvent> {
        self.recorder.lock().expect("recorder lock").events.clone()
    }

    fn queries(&self) -> usize {
        self.recorder.lock().expect("recorder lock").queries()
    }
}

/// Bounded barrier on the child's own subscription fact: `agendar wait` records
/// a runtime lease before its first query, so a visible lease means the child is
/// listening for wakes. Replaces a fixed sleep that silently raced a cold spawn.
fn wait_until_subscribed(env: &CliEnv, ask_id: &str, within: Duration) {
    let service = env.service();
    let deadline = Instant::now() + within;
    loop {
        if service
            .runtime()
            .is_ok_and(|runtime| runtime.wait_leases.contains_key(ask_id))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the wait child did not subscribe within {within:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_real_wait_child_wakes_on_a_human_answer() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    let child = spawn_wait(&env, &ask_id, "15s");
    wait_until_subscribed(&env, &ask_id, Duration::from_secs(10));
    let started = Instant::now();
    let answer = env.answer(&ask_id, "inbox");
    let output = child.wait_with_output().expect("wait child");
    let value = expect_success(&output, "wait");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the child woke on the answer, not on its 15s timeout"
    );
    assert_eq!(value["data"]["ask_id"], ask_id);
    assert_eq!(value["data"]["packet"]["kind"], "answer");
    assert_eq!(value["data"]["packet"]["answer"], answer);
    assert_eq!(value["data"]["packet"]["receipt"]["chose"]["id"], "inbox");
}

#[test]
fn an_answer_before_the_wait_starts_is_not_missed() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    let answer = env.answer(&ask_id, "adapters");
    let started = Instant::now();
    let output = env.run(&["wait", &ask_id, "--timeout", "5s"]);
    let value = expect_success(&output, "wait");
    assert_eq!(value["data"]["packet"]["answer"], answer);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn wait_times_out_without_writing_stdout() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    let started = Instant::now();
    let output = env.run(&["wait", &ask_id, "--timeout", "1s"]);
    let error = expect_error(&output, "E_TIMEOUT", 7);
    assert_eq!(error["error"]["retryable"], true);
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(900), "elapsed={elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "elapsed={elapsed:?}");
    // The ask remains open.
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(inbox["data"]["packets"], json!([]));
}

#[test]
fn wait_on_a_withdrawn_ask_exits_eight() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    expect_success(
        &env.run(&["withdraw", &ask_id, "--reason", "spike answered it"]),
        "withdraw",
    );
    let output = env.run(&["wait", &ask_id, "--timeout", "2s"]);
    expect_error(&output, "E_WITHDRAWN", 8);
}

#[test]
fn no_invalidation_means_at_most_two_decision_queries() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    let service = env.service();
    let project = agendar_core::model::ProjectSlug::parse(&env.project).expect("project");
    let session = agendar_core::model::SessionId::parse(&env.session).expect("session");
    let ask = EventId::parse(&ask_id).expect("ask");
    let mut observer = Recorder::default();
    let started = Instant::now();
    let result = service.wait_for_answer(
        &project,
        &session,
        &ask,
        &WaitOptions {
            timeout: Some(Duration::from_secs(10)),
            ..WaitOptions::default()
        },
        &mut observer,
    );
    let error = result.expect_err("no answer exists");
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(observer.queries(), 2, "events={:?}", observer.events);
    assert!(observer.events.contains(&WaitEvent::TimedOut));
    assert!(started.elapsed() >= Duration::from_secs(9));
}

#[test]
fn one_irrelevant_invalidation_and_one_answer_stay_bounded() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let ask_id = ask(&env);
    let service = env.service();
    let project = agendar_core::model::ProjectSlug::parse(&env.project).expect("project");
    let session = agendar_core::model::SessionId::parse(&env.session).expect("session");
    let ask = EventId::parse(&ask_id).expect("ask");
    let recorder = SharedRecorder::default();
    let mut observer = recorder.clone();

    let within = Duration::from_secs(10);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            service.wait_for_answer(
                &project,
                &session,
                &ask,
                &WaitOptions {
                    timeout: Some(Duration::from_secs(20)),
                    ..WaitOptions::default()
                },
                &mut observer,
            )
        });
        // Barrier: the waiting process ran its initial query before anything
        // unrelated is appended.
        let initial = recorder.wait_for_queries(1, within);
        assert_eq!(
            queries_in(&initial),
            1,
            "only the initial query so far; events={initial:?}"
        );
        assert_eq!(initial.last(), Some(&WaitEvent::QueryInitial));

        // An unrelated plan patch: a real file change that does not answer.
        let feature = json!({
            "title": "Unrelated",
            "slice": "core/other",
            "demo_kind": "cli_transcript"
        });
        let path = write_json(&env, "other.json", &feature);
        expect_success(
            &env.run(&[
                "feature",
                "declare",
                "--node",
                "core/other",
                "--json",
                &path,
            ]),
            "feature declare",
        );

        // Barrier: the unrelated change reached its own invalidation query
        // before the answer exists, so the two wakes are ordered by facts, not
        // by a guessed sleep. One irrelevant invalidation costs at most one
        // query; the extra unit only tolerates a duplicated platform hint.
        let unrelated = recorder.wait_for_queries(2, within);
        assert_eq!(unrelated.last(), Some(&WaitEvent::QueryInvalidation));
        assert!(
            queries_in(&unrelated) <= 3,
            "the irrelevant change caused at most one wake query more; events={unrelated:?}"
        );

        let answer = env.answer(&ask_id, "adapters");
        let packet = handle.join().expect("wait thread").expect("answer packet");
        assert_eq!(packet.answer.unwrap().as_str(), answer);
    });

    let events = recorder.events();
    assert!(
        events.last() == Some(&WaitEvent::AnswerFound),
        "the answer ends the wait; events={events:?}"
    );
    assert!(
        recorder.queries() >= 2,
        "initial + invalidation were observed; events={events:?}"
    );
    // The answer may share the invalidation query or need one more; the
    // historical bound of six wake queries still tolerates platform hint
    // duplicates and fails on a spin loop.
    assert!(
        recorder.queries() <= 6,
        "wake queries stay bounded; events={events:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the wait stayed bounded; elapsed={:?}",
        started.elapsed()
    );
    assert!(!events.contains(&WaitEvent::LeaseRenewed));
}
