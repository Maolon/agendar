//! Assumptions, taint repair and pivot delivery through the real service.
//!
//! Every test uses a temporary Vault and data root and drives the public
//! service: an `ask` whose policy evaluates to `assume` writes the ask and its
//! assumed choice, a dependent start inherits the assumption through the basis
//! the service fills, and a human override taints both features.

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use agendar_core::model::{
    DecisionCard, EventId, Harness, Mode, NodePath, OptionId, PlanFeature, ProjectDeclared,
    ProjectSlug, SessionId, WorkStatus,
};
use agendar_core::runtime::sessions::process_identity;
#[cfg(unix)]
use agendar_core::runtime::steward::steward_tick;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::{Clock as _, SystemClock};

const PROJECT: &str = "demo";

struct Scratch {
    _root: tempfile::TempDir,
    vault: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let vault = root.path().join(format!("{name}-vault"));
        let data = root.path().join(format!("{name}-data"));
        let repo = root.path().join(format!("{name}-repo"));
        fs::create_dir_all(&repo).expect("repo");
        Self {
            _root: root,
            vault,
            data,
            repo,
        }
    }
}

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("project slug")
}

fn node(name: &str) -> NodePath {
    NodePath::parse(name).expect("node")
}

fn card() -> DecisionCard {
    serde_json::from_value(support::card()).expect("decision card")
}

fn registration(scratch: &Scratch, harness: Harness, id: &str) -> SessionRegistration {
    let pid = std::process::id();
    SessionRegistration {
        project: project(),
        harness,
        harness_session_id: id.to_owned(),
        pid,
        process_start: process_identity(pid).expect("process identity"),
        repo_root: scratch.repo.clone(),
    }
}

fn register(service: &Service, scratch: &Scratch, harness: Harness, id: &str) -> SessionId {
    register_session(service, registration(scratch, harness, id)).expect("register")
}

/// `y` depends on `x`, so a start on `y` must inherit `x`'s assumptions.
fn declare_board(service: &Service, session: &SessionId) {
    let patch: ProjectDeclared = serde_json::from_value(json!({
        "title": "Assumption board",
        "brief": "One assumption, two consumers.",
        "goal": "Prove pivot delivery."
    }))
    .expect("project patch");
    service
        .declare_project(&project(), session, patch, None)
        .expect("declare project");
    let x: PlanFeature = serde_json::from_value(json!({
        "title": "x",
        "slice": "core",
        "demo_kind": "cli_transcript"
    }))
    .expect("feature patch");
    service
        .declare_feature(&project(), session, node("x"), x, None)
        .expect("declare x");
    let y: PlanFeature = serde_json::from_value(json!({
        "title": "y",
        "slice": "core",
        "demo_kind": "cli_transcript",
        "depends_on": ["x"]
    }))
    .expect("feature patch");
    service
        .declare_feature(&project(), session, node("y"), y, None)
        .expect("declare y");
}

fn start(service: &Service, session: &SessionId, name: &str) -> EventId {
    let value = service
        .progress_started(
            &project(),
            session,
            &node(name),
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(600).expect("min"),
                max_s: agendar_core::model::Seconds::new(1_800).expect("max"),
            },
            None,
            None,
        )
        .expect("start work");
    EventId::parse(value["attempt"].as_str().expect("attempt")).expect("attempt id")
}

fn project_state(service: &Service) -> agendar_core::model::ProjectState {
    service
        .export_state(SystemClock.now())
        .expect("state")
        .projects
        .get(&project())
        .cloned()
        .expect("project")
}

/// Build the board up to a live assumption: x started, assumed, then finished;
/// y started afterwards so it inherits the assumption.
fn assumed_board(service: &Service, a: &SessionId, b: &SessionId) -> EventId {
    declare_board(service, a);
    let attempt = start(service, a, "x");
    let ask = service
        .ask(&project(), a, &node("x"), card(), Mode::Assume, None)
        .expect("assume-routed ask")
        .ask_id;
    service
        .progress_finished(
            &project(),
            a,
            &node("x"),
            &attempt,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("x is done");
    start(service, b, "y");
    let state = project_state(service);
    assert_eq!(
        state.features[&node("x")].basis.as_str(),
        "assumed",
        "an assumed choice leaves the target assumed"
    );
    assert_eq!(
        state.features[&node("y")].basis.as_str(),
        "assumed",
        "a completed assumed upstream never makes the child firm"
    );
    assert_eq!(
        state.decisions[&ask].built_on,
        vec![node("x"), node("y")],
        "built_on counts the target and the started descendant"
    );
    ask
}

fn override_with(service: &Service, ask: &EventId, option: &str) -> EventId {
    service
        .answer_decision(
            &project(),
            ask,
            &OptionId::parse(option).expect("option"),
            None,
            None,
            None,
        )
        .expect("human answer")
}

#[test]
fn an_override_marks_every_consumer_tainted_and_a_repaired_restart_shrinks_the_pivot() {
    let scratch = Scratch::new("pivot");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let a = register(&service, &scratch, Harness::Generic, "pivot-a");
    let b = register(&service, &scratch, Harness::Generic, "pivot-b");
    let c = register(&service, &scratch, Harness::Generic, "pivot-c");
    let ask = assumed_board(&service, &a, &b);

    // The override: every `assumed(d)` becomes `tainted(d)`; progress is kept.
    override_with(&service, &ask, "b");
    let state = project_state(&service);
    for (name, want_progress) in [("x", "done"), ("y", "doing")] {
        let feature = &state.features[&node(name)];
        assert_eq!(feature.basis.as_str(), "tainted", "{name} is tainted");
        assert_eq!(feature.taints, vec![ask.clone()]);
        assert_eq!(feature.progress.as_str(), want_progress, "progress is kept");
        assert_eq!(feature.assumptions.len(), 1, "the choice stays unconfirmed");
    }
    assert_eq!(state.decisions[&ask].built_on, vec![node("x"), node("y")]);

    // Every affected session gets the pivot with the sorted redo set; an
    // unrelated session gets nothing.
    for session in [&a, &b] {
        let (packets, _, _) = service
            .inbox(&project(), session, None, false)
            .expect("inbox");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].kind, agendar_core::model::PacketKind::Pivot);
        assert_eq!(packets[0].redo, vec![node("x"), node("y")]);
        let text = packets[0].pivot_text().expect("pivot instruction text");
        assert!(text.contains("Redo"), "text={text}");
        assert!(text.contains("x, y"), "text={text}");
    }
    let (unaffected, _, _) = service.inbox(&project(), &c, None, false).expect("inbox");
    assert!(unaffected.is_empty(), "c consumed nothing to redo");

    // Repairing x is an explicit restart of the tainted feature.
    let previous_attempt = state.features[&node("x")]
        .attempt
        .clone()
        .expect("x has an attempt");
    let repaired = start(&service, &a, "x");
    let state = project_state(&service);
    let x = &state.features[&node("x")];
    assert_eq!(x.basis.as_str(), "firm", "the repaired restart is firm");
    assert!(x.taints.is_empty());
    assert_eq!(x.attempt, Some(repaired.clone()));
    assert_eq!(
        state.features[&node("y")].basis.as_str(),
        "tainted",
        "y repairs on its own"
    );

    // The pivot now goes only to the session that still has redo work.
    let (b_packets, _, _) = service.inbox(&project(), &b, None, false).expect("inbox");
    assert_eq!(b_packets.len(), 1);
    assert_eq!(b_packets[0].redo, vec![node("y")]);
    let (a_packets, _, _) = service.inbox(&project(), &a, None, false).expect("inbox");
    assert!(
        a_packets
            .iter()
            .all(|packet| packet.kind != agendar_core::model::PacketKind::Pivot),
        "a repaired its own work"
    );

    // The restart cites the overriding answer in its own basis.
    let fresh = service.refresh(SystemClock.now(), false).expect("refresh");
    let restarted = fresh
        .events
        .iter()
        .find(|event| event.id == repaired)
        .expect("restart event");
    let effective = state.decisions[&ask]
        .effective_receipt
        .clone()
        .expect("override receipt");
    assert!(
        restarted.refs.basis.contains(&effective),
        "the service filled the current overriding answer into the basis"
    );
    assert!(
        restarted.refs.causes.contains(&previous_attempt),
        "a restart cites the attempt it replaces"
    );
}

#[cfg(unix)]
fn fake_codex_echo(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let script = dir.join("codex");
    let record = dir.join("argv.bin");
    let body = format!(
        "#!/bin/sh\n{{ printf '%s\\0' \"$@\"; printf '\\n\\0'; }} >> '{}'\nprintf 'Queued message m-1 for thread %s.\\n' \"$3\"\nexit 0\n",
        record.display()
    );
    fs::write(&script, body).expect("fake codex");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("executable fake");
    (script, record)
}

#[cfg(unix)]
fn recorded_messages(path: &std::path::Path) -> Vec<String> {
    let bytes = fs::read(path).expect("recorded argv");
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .filter(|part| part.starts_with("Agendar packet"))
        .collect()
}

#[cfg(unix)]
#[test]
fn the_steward_queues_one_pivot_to_every_affected_session() {
    let scratch = Scratch::new("pivot-codex");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let (script, record) = fake_codex_echo(&scratch.repo);
    agendar_core::runtime::Config::update(service.local().data_root(), |config| {
        config.codex_bin = Some(script.to_string_lossy().into_owned());
        Ok(())
    })
    .expect("set local codex path");

    let a = register(&service, &scratch, Harness::Codex, "thread-a");
    let b = register(&service, &scratch, Harness::Codex, "thread-b");
    let ask = assumed_board(&service, &a, &b);

    override_with(&service, &ask, "b");
    let report = steward_tick(&service).expect("steward after the override");
    assert_eq!(report.delivered, 2, "both affected sessions were queued");
    let threads: Vec<String> = report
        .queued
        .iter()
        .map(|queued| queued.thread_id.clone())
        .collect();
    assert_eq!(threads, vec!["thread-a".to_owned(), "thread-b".to_owned()]);

    let messages = recorded_messages(&record);
    assert_eq!(messages.len(), 2, "one queue call per affected session");
    for message in &messages {
        assert!(message.contains("\"kind\":\"pivot\""), "message={message}");
        assert!(message.contains("Redo"), "message={message}");
        assert!(message.contains("x, y"), "message={message}");
    }

    // A second tick never re-enqueues the same pivot.
    let again = steward_tick(&service).expect("second steward tick");
    assert_eq!(again.delivered, 0);
    assert_eq!(recorded_messages(&record).len(), 2);
}
