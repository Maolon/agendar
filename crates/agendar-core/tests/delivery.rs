//! Cross-host delivery, the Codex queue contract and handoff on loss.
//!
//! These tests use two local data directories as two host identities sharing one
//! Vault and a fake `codex` executable that records its argv. They never start
//! the Codex daemon, install anything or contact a network service.

mod support;

use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;

use serde_json::json;

#[cfg(unix)]
use agendar_core::adapters::ResumeStatus;
use agendar_core::error::ErrorCode;
use agendar_core::model::{
    DecisionCard, DeliveredOutcome, EventId, Harness, Liveness, Mode, NodePath, OptionId,
    PlanFeature, ProjectDeclared, ProjectSlug, SessionId,
};
#[cfg(unix)]
use agendar_core::runtime::Config;
#[cfg(unix)]
use agendar_core::runtime::sessions::RuntimeState;
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session, steward_tick};
#[cfg(unix)]
use agendar_core::runtime::steward::{CLAUDE_RESUME_GRACE_S, steward_tick_at};
use agendar_core::service::Service;
use agendar_core::vault::Clock;

const PROJECT: &str = "demo";
const NODE: &str = "core/wake";

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

fn node() -> NodePath {
    NodePath::parse(NODE).expect("node")
}

fn registration(scratch: &Scratch, harness: Harness, id: &str) -> SessionRegistration {
    let pid = std::process::id();
    SessionRegistration {
        project: project(),
        harness,
        harness_session_id: id.to_owned(),
        pid,
        process_start: process_identity(pid).expect("test process identity"),
        repo_root: scratch.repo.clone(),
    }
}

fn declare_board(service: &Service, session: &SessionId) {
    let patch: ProjectDeclared = serde_json::from_value(json!({
        "title": "Cross-host delivery",
        "brief": "Two hosts share one Vault.",
        "goal": "Deliver the answer to the addressed session."
    }))
    .expect("project patch");
    service
        .declare_project(&project(), session, patch, None)
        .expect("declare project");
    let patch: PlanFeature = serde_json::from_value(json!({
        "title": "Wake delivery",
        "slice": "core",
        "demo_kind": "cli_transcript"
    }))
    .expect("feature patch");
    service
        .declare_feature(&project(), session, node(), patch, None)
        .expect("declare feature");
}

fn card() -> DecisionCard {
    serde_json::from_value(support::card()).expect("decision card")
}

fn start(service: &Service, session: &SessionId) -> EventId {
    let value = service
        .progress_started(
            &project(),
            session,
            &node(),
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

fn ask(service: &Service, session: &SessionId) -> EventId {
    let result = service
        .ask(&project(), session, &node(), card(), Mode::Ask, None)
        .expect("ask");
    result.ask_id
}

fn option() -> OptionId {
    OptionId::parse("a").expect("option")
}

#[test]
fn a_cross_host_answer_reaches_the_generic_session_as_an_inbox_packet() {
    let host_a = Scratch::new("a");
    let host_b = Scratch::new("b");
    let (service_a, _) = Service::init(&host_a.vault, &host_a.data).expect("init host A");
    let (service_b, created) = Service::init(&host_a.vault, &host_b.data).expect("init host B");
    assert!(!created, "host B opens the same Vault manifest");
    let session = register_session(
        &service_a,
        registration(&host_a, Harness::Generic, "generic-a"),
    )
    .expect("register host A session");
    declare_board(&service_a, &session);
    let _attempt = start(&service_a, &session);
    let ask_id = ask(&service_a, &session);

    // Host B's human writes the answer. It is a different host writer path.
    let answer = service_b
        .answer_decision(&project(), &ask_id, &option(), None, None, None)
        .expect("answer on host B");

    // Host B's steward only knows host B's registrations; it must not invent a
    // loss for host A's session.
    let b_report = steward_tick(&service_b).expect("host B steward tick");
    assert_eq!(b_report.lost, 0);
    assert_eq!(b_report.processed, 0);

    // Host A's steward sees the synced answer; the generic adapter is pull-only,
    // so readiness is an inbox packet, not a fabricated push.
    let a_report = steward_tick(&service_a).expect("host A steward tick");
    assert_eq!(a_report.delivered, 0);
    let (packets, _, _) = service_a
        .inbox(&project(), &session, None, false)
        .expect("inbox");
    let packet = packets
        .iter()
        .find(|packet| packet.ask.as_ref() == Some(&ask_id))
        .expect("the synced answer is an inbox packet");
    assert_eq!(packet.answer.as_ref(), Some(&answer));
    assert_eq!(
        packet.receipt.as_ref().expect("receipt").chose.label,
        "Choose A"
    );

    // The CLI's successful inbox output records the checkpoint; the event is a
    // steward-owned delivery fact on host A.
    service_a
        .mark_delivered(
            &project(),
            &session,
            std::slice::from_ref(packet),
            DeliveredOutcome::InboxRead,
        )
        .expect("record the inbox read");

    let now = agendar_core::vault::SystemClock.now();
    let state = service_a.export_state(now).expect("state");
    let project_state = state.projects.get(&project()).expect("project");
    assert_eq!(
        project_state
            .sessions
            .get(&session)
            .expect("session")
            .liveness,
        Liveness::Live
    );
    assert!(
        project_state
            .sessions
            .get(&session)
            .expect("session")
            .lost_event
            .is_none(),
        "host B never marks host A's session lost"
    );
    let receipt = project_state
        .receipts
        .get(&answer)
        .expect("receipt from the remote answer");
    assert_eq!(receipt.source, agendar_core::model::ReceiptSource::Human);

    // The answer file is under host B and the delivery event under host A.
    let host_a_dir = host_a
        .vault
        .join("projects")
        .join(PROJECT)
        .join(service_a.host().as_str());
    let host_b_dir = host_a
        .vault
        .join("projects")
        .join(PROJECT)
        .join(service_b.host().as_str());
    assert!(
        host_a_dir.join("steward.jsonl").is_file(),
        "host A writes its own steward stream"
    );
    assert!(
        host_b_dir.join("operator.jsonl").is_file(),
        "host B writes its own operator stream"
    );

    // Host B cannot write a delivery event for host A's session.
    let error = service_b
        .mark_delivered(
            &project(),
            &session,
            std::slice::from_ref(packet),
            DeliveredOutcome::InboxRead,
        )
        .expect_err("a foreign session is never writable");
    assert_eq!(error.code(), ErrorCode::Context);

    // Silence from host A is staleness after the freshness window, never lost.
    let later = now.add_seconds(20 * 60);
    let stale = service_b.export_state(later).expect("state later");
    let session_state = stale
        .projects
        .get(&project())
        .expect("project")
        .sessions
        .get(&session)
        .expect("session");
    assert_eq!(session_state.liveness, Liveness::Quiet);
    assert!(session_state.lost_event.is_none());
}

#[cfg(unix)]
fn fake_codex(dir: &Path, thread: &str, warning: bool) -> (PathBuf, PathBuf, PathBuf) {
    let script = dir.join("codex");
    let record = dir.join("argv.bin");
    let runs = dir.join("runs.txt");
    let mut body = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\necho run >> '{}'\nprintf 'Queued message m-1 for thread {}.\\n'\n",
        record.display(),
        runs.display(),
        thread
    );
    if warning {
        body.push_str(&format!(
            "printf 'warning: could not resume thread {}; start a local daemon\\n' >&2\n",
            thread
        ));
    }
    body.push_str("exit 0\n");
    fs::write(&script, body).expect("fake codex");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable fake");
    }
    (script, record, runs)
}

#[cfg(unix)]
fn recorded_argv(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).expect("recorded argv");
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[cfg(unix)]
fn codex_scratch(
    warning: bool,
    answer_now: bool,
) -> (Scratch, Service, SessionId, PathBuf, PathBuf) {
    let scratch = Scratch::new("codex");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let thread = "thread-abc";
    let session = register_session(&service, registration(&scratch, Harness::Codex, thread))
        .expect("register codex session");
    declare_board(&service, &session);
    let _attempt = start(&service, &session);
    let ask_id = ask(&service, &session);
    if answer_now {
        service
            .answer_decision(&project(), &ask_id, &option(), None, None, None)
            .expect("human answer");
    }
    let (script, record, runs) =
        fake_codex(scratch.repo.parent().expect("parent"), thread, warning);
    Config::update(service.local().data_root(), |config| {
        config.codex_bin = Some(script.to_string_lossy().into_owned());
        Ok(())
    })
    .expect("set local codex path");
    (scratch, service, session, record, runs)
}

#[cfg(unix)]
#[test]
fn the_codex_steward_queues_to_the_original_thread_once_with_exact_argv() {
    let (_scratch, service, session, record, runs) = codex_scratch(false, true);
    let report = steward_tick(&service).expect("steward tick");
    assert_eq!(report.delivered, 1);
    assert_eq!(report.queued.len(), 1);
    assert_eq!(report.queued[0].status, "queued");
    assert_eq!(report.queued[0].message_id, "m-1");
    assert!(
        report
            .capabilities
            .iter()
            .any(|capability| capability.harness == Harness::Codex && capability.queue_supported)
    );

    let argv = recorded_argv(&record);
    assert_eq!(argv.len(), 6, "argv={argv:?}");
    assert_eq!(argv[0], "queue");
    assert_eq!(argv[1], "--thread");
    assert_eq!(argv[2], "thread-abc");
    assert_eq!(argv[3], "--message");
    let message = &argv[4];
    assert_eq!(argv[5], "--resume");
    assert!(message.starts_with("Agendar packet\n"));
    assert!(message.contains("\"kind\":\"answer\""));
    assert!(message.contains("Deduplicate by packet.id."));
    assert!(message.contains("--session"));

    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let attempt = runtime
        .delivery_attempts
        .iter()
        .find(|(_, attempt)| attempt.session == session)
        .map(|(_, attempt)| attempt)
        .expect("persisted queue attempt");
    let queued = attempt.queued.as_ref().expect("durable queue outcome");
    assert_eq!(queued.thread_id, "thread-abc");
    assert_eq!(queued.message_id, "m-1");
    assert_eq!(
        queued.resume_status,
        ResumeStatus::RequestedUnconfirmed,
        "a queue without a resume warning is queued, not continued"
    );

    let deliveries = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state")
        .projects
        .get(&project())
        .expect("project")
        .deliveries
        .len();
    assert_eq!(deliveries, 0, "this board has no evidence deliveries");
    let delivered = std::fs::read_dir(
        service
            .vault()
            .root()
            .join("projects")
            .join(PROJECT)
            .join(service.host().as_str()),
    )
    .expect("host dir")
    .filter_map(Result::ok)
    .filter(|entry| entry.file_name().to_string_lossy().starts_with("steward"))
    .filter_map(|entry| fs::read_to_string(entry.path()).ok())
    .any(|text| text.contains("\"kind\":\"delivered\""));
    assert!(delivered, "queue acceptance appends one delivered event");
    assert_eq!(
        fs::read_to_string(&runs)
            .expect("run counter")
            .lines()
            .count(),
        1,
        "the fake transport ran exactly once"
    );

    // No timer re-enqueue and no duplicate delivered event.
    let second = steward_tick(&service).expect("second tick");
    assert_eq!(second.delivered, 0);
    assert_eq!(
        fs::read_to_string(&runs)
            .expect("run counter")
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn a_resume_warning_is_queued_for_the_next_resume_never_reported_as_continued() {
    let (_scratch, service, session, _record, _runs) = codex_scratch(true, true);
    let report = steward_tick(&service).expect("steward tick");
    assert_eq!(report.delivered, 1);
    assert_eq!(report.queued.len(), 1);
    assert_eq!(
        report.queued[0].status, "queued_waiting_for_resume",
        "a resume warning is reported as waiting, never as continued"
    );
    assert_eq!(
        report.queued[0].detail,
        "queued, applies when the session resumes"
    );
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let attempt = runtime
        .delivery_attempts
        .iter()
        .find(|(_, attempt)| attempt.session == session)
        .map(|(_, attempt)| attempt)
        .expect("attempt");
    assert_eq!(
        attempt
            .queued
            .as_ref()
            .expect("queued outcome")
            .resume_status,
        ResumeStatus::WarningWaitingForResume
    );
    assert!(
        attempt.outcome == "accepted",
        "the transport accepted the enqueue even when the daemon is absent"
    );
}

#[cfg(unix)]
#[test]
fn a_lost_feature_needs_an_explicit_handoff_before_a_new_session_may_start_it() {
    let scratch = Scratch::new("handoff");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleeping harness");
    let dead_pid = child.id();
    let dead_start = process_identity(dead_pid).expect("child identity");
    let first = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Generic,
            harness_session_id: "first".to_owned(),
            pid: dead_pid,
            process_start: dead_start,
            repo_root: scratch.repo.clone(),
        },
    )
    .expect("register first session");
    declare_board(&service, &first);
    let _attempt = start(&service, &first);
    child.kill().expect("kill child");
    child.wait().expect("reap child");
    let report = steward_tick(&service).expect("steward tick");
    assert_eq!(report.lost, 1);

    let second = register_session(&service, registration(&scratch, Harness::Generic, "second"))
        .expect("register replacement session");

    // Without a handoff the lost feature is not runnable for the new session.
    let error = service
        .progress_started(
            &project(),
            &second,
            &node(),
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(60).expect("min"),
                max_s: agendar_core::model::Seconds::new(120).expect("max"),
            },
            None,
            None,
        )
        .expect_err("a lost feature is not silently reassigned");
    assert_eq!(error.code(), ErrorCode::Policy);

    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let need = state
        .projects
        .get(&project())
        .expect("project")
        .needs_you
        .iter()
        .find(|need| need.id.starts_with("rescue:lost:"))
        .expect("lost rescue row")
        .clone();
    assert_eq!(need.node.as_ref(), Some(&node()));
    let resolved = service
        .resolve_rescue(
            &project(),
            &need.id,
            agendar_core::model::RescueAction::Handoff,
        )
        .expect("handoff");
    assert_eq!(resolved["action"], "handoff");

    // A handoff does not answer an independent open human question.
    let blocked_ask = service
        .ask(&project(), &second, &node(), card(), Mode::Ask, None)
        .expect("open ask after the loss");
    let blocked = service
        .progress_started(
            &project(),
            &second,
            &node(),
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(60).expect("min"),
                max_s: agendar_core::model::Seconds::new(120).expect("max"),
            },
            None,
            None,
        )
        .expect_err("an open ask still blocks the replacement");
    assert_eq!(blocked.code(), ErrorCode::Policy);
    service
        .withdraw(
            &project(),
            &second,
            &blocked_ask.ask_id,
            "test cleanup".to_owned(),
            None,
        )
        .expect("withdraw the blocking ask");

    let value = service
        .progress_started(
            &project(),
            &second,
            &node(),
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(60).expect("min"),
                max_s: agendar_core::model::Seconds::new(120).expect("max"),
            },
            None,
            None,
        )
        .expect("the replacement start consumes the handoff");
    let attempt = EventId::parse(value["attempt"].as_str().expect("attempt")).expect("attempt");

    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let feature = state
        .projects
        .get(&project())
        .expect("project")
        .features
        .get(&node())
        .expect("feature");
    assert_eq!(feature.session.as_ref(), Some(&second));
    assert_eq!(feature.handoff_consumed_by.as_ref(), Some(&attempt));
    assert!(!feature.handoff_pending);
}

#[cfg(unix)]
fn claude_session(service: &Service, scratch: &Scratch, conversation: &str, pid: u32) -> SessionId {
    register_session(
        service,
        SessionRegistration {
            project: project(),
            harness: Harness::ClaudeCode,
            harness_session_id: conversation.to_owned(),
            pid,
            process_start: process_identity(pid).expect("identity"),
            repo_root: scratch.repo.clone(),
        },
    )
    .expect("register claude code session")
}

#[cfg(unix)]
#[test]
fn a_resumed_claude_code_conversation_keeps_its_session_and_steps() {
    let scratch = Scratch::new("claude-resume");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
    let session = claude_session(&service, &scratch, "conversation-1", child.id());
    declare_board(&service, &session);
    let _attempt = start(&service, &session);
    child.kill().expect("kill");
    child.wait().expect("reap");

    // Within the window an exited Claude Code process is not a loss.
    let report = steward_tick(&service).expect("tick");
    assert_eq!(report.lost, 0);
    assert_eq!(report.resumable, 1);

    // `claude --resume` registers the same conversation from a new process.
    let resumed = claude_session(&service, &scratch, "conversation-1", std::process::id());
    assert_eq!(resumed, session, "the same Agendar session comes back");
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let record = runtime.sessions.get(&session).expect("session");
    assert!(!record.resumable && !record.lost && !record.sealed);

    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let project_state = state.projects.get(&project()).expect("project");
    let feature = project_state.features.get(&node()).expect("feature");
    assert_eq!(
        feature.session.as_ref(),
        Some(&session),
        "the step stays its own"
    );
    assert!(
        project_state
            .needs_you
            .iter()
            .all(|need| !need.id.starts_with("rescue:lost:")),
        "no hand-off item"
    );
    // A later tick leaves the live, resumed session alone.
    assert_eq!(steward_tick(&service).expect("tick").lost, 0);
}

#[cfg(unix)]
#[test]
fn an_exited_claude_code_session_is_lost_once_the_window_passes() {
    let scratch = Scratch::new("claude-lost");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
    let session = claude_session(&service, &scratch, "conversation-2", child.id());
    declare_board(&service, &session);
    let attempt = start(&service, &session);
    child.kill().expect("kill");
    child.wait().expect("reap");

    let later = agendar_core::vault::SystemClock
        .now()
        .add_seconds(CLAUDE_RESUME_GRACE_S + 60);
    let report = steward_tick_at(&service, later).expect("tick after the window");
    assert_eq!(
        report.lost, 1,
        "no report for 15 minutes: the session is lost"
    );
    let state = service.export_state(later).expect("state");
    assert!(
        state
            .projects
            .get(&project())
            .expect("project")
            .needs_you
            .iter()
            .any(|need| need.id.starts_with("rescue:lost:")),
        "the hand-off item reaches the human"
    );
    // A resume after the loss starts a new session that continues the lost
    // one: the same conversation takes its step back, no hand-off involved.
    let resumed = claude_session(&service, &scratch, "conversation-2", std::process::id());
    assert_ne!(resumed, session, "a sealed session never comes back");
    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let project_state = state.projects.get(&project()).expect("project");
    let feature = project_state.features.get(&node()).expect("feature");
    assert_eq!(
        feature.session.as_ref(),
        Some(&resumed),
        "the running step moved to the continuation"
    );
    assert_ne!(feature.liveness, Liveness::Lost);
    assert!(
        project_state
            .needs_you
            .iter()
            .all(|need| !need.id.starts_with("rescue:lost:")),
        "no hand-off item once the conversation is back"
    );
    // The continuation pulses and finishes the attempt it took over: its
    // events cite the predecessor's attempt and are not quarantined.
    service
        .pulse(
            &project(),
            &resumed,
            &node(),
            &attempt,
            "Back after a restart".to_owned(),
            true,
            None,
            None,
        )
        .expect("the continuation pulses its attempt");
    service
        .progress_finished(
            &project(),
            &resumed,
            &node(),
            &attempt,
            agendar_core::model::WorkStatus::Done,
            Vec::new(),
            Some("finished after the restart".to_owned()),
            None,
        )
        .expect("the continuation finishes its attempt");
    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let feature = state
        .projects
        .get(&project())
        .expect("project")
        .features
        .get(&node())
        .expect("feature");
    assert!(
        matches!(
            feature.progress,
            agendar_core::model::Progress::Done | agendar_core::model::Progress::Verified
        ),
        "the done counts: {:?}",
        feature.progress
    );
    assert!(
        state
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code.as_deref() != Some("reference_mismatch")),
        "nothing was quarantined: {:?}",
        state.diagnostics
    );

    // Another conversation never continues it.
    let stranger = claude_session(&service, &scratch, "conversation-3", std::process::id());
    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let feature = state
        .projects
        .get(&project())
        .expect("project")
        .features
        .get(&node())
        .expect("feature");
    assert_eq!(feature.session.as_ref(), Some(&resumed));
    assert_ne!(feature.session.as_ref(), Some(&stranger));
}

#[cfg(unix)]
#[test]
fn a_codex_turn_exit_is_resumable_and_raises_no_rescue() {
    let scratch = Scratch::new("resumable");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let thread = "thread-resume";
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn turn process");
    let turn_pid = child.id();
    let turn_start = process_identity(turn_pid).expect("turn identity");
    let session = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Codex,
            harness_session_id: thread.to_owned(),
            pid: turn_pid,
            process_start: turn_start,
            repo_root: scratch.repo.clone(),
        },
    )
    .expect("register codex session");
    declare_board(&service, &session);
    let _attempt = start(&service, &session);
    child.kill().expect("kill the turn process");
    child.wait().expect("reap the turn process");
    assert!(process_identity(turn_pid).is_none(), "the turn ended");

    // A finished Codex turn is a resumable session, never an agent stopped.
    let report = steward_tick(&service).expect("steward tick");
    assert_eq!(report.lost, 0, "a Codex turn exit is not a loss");
    assert_eq!(report.resumable, 1);
    let second = steward_tick(&service).expect("second tick");
    assert_eq!(second.lost, 0);
    assert_eq!(second.resumable, 0, "resumable is reported once");

    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let record = runtime.sessions.get(&session).expect("session");
    assert!(record.resumable, "the session is resumable");
    assert!(
        !record.lost && !record.sealed,
        "no loss and no sealed writer"
    );

    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    let project_state = state.projects.get(&project()).expect("project");
    let session_state = project_state.sessions.get(&session).expect("session state");
    assert!(session_state.lost_event.is_none());
    assert_ne!(session_state.liveness, Liveness::Lost);
    assert!(
        project_state
            .needs_you
            .iter()
            .all(|need| !need.id.starts_with("rescue:lost:")),
        "no agent-stopped rescue item"
    );
    assert_ne!(
        project_state
            .features
            .get(&node())
            .expect("feature")
            .liveness,
        Liveness::Lost
    );

    // An answer that arrives after the turn ended is still queued to the same
    // durable thread.
    let (script, _record, runs) = fake_codex(scratch.repo.parent().expect("parent"), thread, false);
    Config::update(service.local().data_root(), |config| {
        config.codex_bin = Some(script.to_string_lossy().into_owned());
        Ok(())
    })
    .expect("set local codex path");
    let ask_id = ask(&service, &session);
    service
        .answer_decision(&project(), &ask_id, &option(), None, None, None)
        .expect("human answer after the turn ended");
    let delivery = steward_tick(&service).expect("delivery tick");
    assert_eq!(delivery.delivered, 1);
    assert_eq!(delivery.queued.len(), 1);
    assert_eq!(delivery.queued[0].thread_id, thread);
    assert_eq!(
        fs::read_to_string(&runs)
            .expect("run counter")
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn the_next_codex_turn_reuses_the_session_and_can_ack_a_delivered_packet() {
    let scratch = Scratch::new("codex-resume");
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let thread = "thread-next";
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn turn process");
    let turn_pid = child.id();
    let turn_start = process_identity(turn_pid).expect("turn identity");
    let session = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Codex,
            harness_session_id: thread.to_owned(),
            pid: turn_pid,
            process_start: turn_start,
            repo_root: scratch.repo.clone(),
        },
    )
    .expect("register codex session");
    declare_board(&service, &session);
    let _attempt = start(&service, &session);
    let ask_id = ask(&service, &session);
    let (script, record, runs) = fake_codex(scratch.repo.parent().expect("parent"), thread, true);
    Config::update(service.local().data_root(), |config| {
        config.codex_bin = Some(script.to_string_lossy().into_owned());
        Ok(())
    })
    .expect("set local codex path");

    // The turn process ends; the session is resumable, not lost.
    child.kill().expect("kill the turn process");
    child.wait().expect("reap the turn process");
    let ended = steward_tick(&service).expect("tick");
    assert_eq!(ended.lost, 0);
    assert_eq!(ended.resumable, 1);

    // The human answers after the turn ended; the steward queues to the thread.
    let answer = service
        .answer_decision(&project(), &ask_id, &option(), None, None, None)
        .expect("human answer after the turn ended");
    let delivery = steward_tick(&service).expect("delivery tick");
    assert_eq!(delivery.delivered, 1);
    assert_eq!(delivery.queued.len(), 1);
    assert_eq!(delivery.queued[0].status, "queued_waiting_for_resume");
    assert_eq!(recorded_argv(&record)[2], thread);

    // The next `codex exec` turn is a new process with the same thread id.
    let live_pid = std::process::id();
    let resumed = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Codex,
            harness_session_id: thread.to_owned(),
            pid: live_pid,
            process_start: process_identity(live_pid).expect("test process identity"),
            repo_root: scratch.repo.clone(),
        },
    )
    .expect("re-register the same thread");
    assert_eq!(
        resumed, session,
        "the same Codex thread reuses the same Agendar session"
    );

    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let record = runtime.sessions.get(&session).expect("session");
    assert_eq!(record.pid, live_pid, "the recorded pid is updated");
    assert!(!record.resumable && !record.lost && !record.sealed);
    assert!(
        record.started,
        "the reused session writes no second start pulse"
    );

    let state = service
        .export_state(agendar_core::vault::SystemClock.now())
        .expect("state");
    assert!(
        state
            .projects
            .get(&project())
            .expect("project")
            .needs_you
            .iter()
            .all(|need| !need.id.starts_with("rescue:lost:")),
        "re-registration raises no rescue item either"
    );

    // Pending packets and acks keep working for the reused session.
    let after_resume = steward_tick(&service).expect("tick after re-registration");
    assert_eq!(after_resume.lost, 0);
    assert_eq!(
        after_resume.delivered, 0,
        "the already queued packet is not re-enqueued after same-thread re-registration"
    );
    let (packets, _, _) = service
        .inbox(&project(), &session, None, false)
        .expect("inbox");
    assert!(
        packets
            .iter()
            .any(|packet| packet.answer.as_ref() == Some(&answer)),
        "the queued answer is still readable by the same session"
    );
    let ack = service
        .ack(&project(), &session, &answer, Vec::new(), None)
        .expect("ack after re-registration");
    assert_eq!(ack["lifecycle"], "acked");
    assert_eq!(
        fs::read_to_string(&runs)
            .expect("run counter")
            .lines()
            .count(),
        1,
        "the same packet is never re-enqueued to the same thread"
    );
}
