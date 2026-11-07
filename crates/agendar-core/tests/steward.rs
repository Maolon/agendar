//! Session registration identity and the bounded pid steward tick.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use agendar_core::model::{
    Agent, EventPayload, Harness, Liveness, ProjectSlug, Pulse, Refs, SessionId, WriterId,
    WriterRole,
};
use agendar_core::runtime::sessions::{RuntimeState, process_identity};
use agendar_core::runtime::steward::{
    SessionRegistration, register_session, steward_tick, steward_tick_at,
};
use agendar_core::service::Service;
use agendar_core::vault::{
    Clock, EventDraft, SystemClock, UlidIdSource, WriterKey, append_event, append_event_reserved,
};

struct Scratch {
    _root: tempfile::TempDir,
    vault: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

fn scratch() -> Scratch {
    let root = tempfile::tempdir().expect("temp root");
    let vault = root.path().join("vault");
    let data = root.path().join("data");
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    Scratch {
        _root: root,
        vault,
        data,
        repo,
    }
}

fn registration(
    scratch: &Scratch,
    harness_session: &str,
    pid: u32,
    process_start: u64,
) -> SessionRegistration {
    registration_for(
        scratch,
        Harness::Generic,
        harness_session,
        pid,
        process_start,
    )
}

fn registration_for(
    scratch: &Scratch,
    harness: Harness,
    harness_session: &str,
    pid: u32,
    process_start: u64,
) -> SessionRegistration {
    SessionRegistration {
        project: ProjectSlug::parse("demo").expect("project"),
        harness,
        harness_session_id: harness_session.to_owned(),
        pid,
        process_start,
        repo_root: scratch.repo.clone(),
    }
}

/// Kill and reap a child, then wait until the OS stops listing it: Windows can
/// keep a just-terminated process in its process snapshot for a moment.
fn end_child(mut child: std::process::Child, pid: u32) {
    child.kill().expect("kill child");
    child.wait().expect("reap child");
    drop(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_identity(pid).is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn spawn_sleeping_child() -> (u32, u64, std::process::Child) {
    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let start = process_identity(pid).expect("child identity");
    (pid, start, child)
}

fn pulse_draft(session: &SessionId, message: &str) -> EventDraft {
    EventDraft {
        refs: Refs::default(),
        payload: EventPayload::Pulse(Pulse {
            agent: Agent {
                session: session.clone(),
                harness: Harness::Generic,
            },
            message: message.to_owned(),
            boundary: false,
            eta: None,
        }),
    }
}

#[test]
fn a_reserved_event_id_makes_a_retried_append_idempotent() {
    let scratch = scratch();
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let session = SessionId::parse("00000000000000000000000101").expect("session");
    let reserved =
        agendar_core::model::EventId::parse("00000000000000000000000201").expect("reserved");
    let key = WriterKey {
        project: ProjectSlug::parse("demo").unwrap(),
        writer: WriterId::new(service.host().clone(), WriterRole::Agent(session.clone())),
    };
    let clock = SystemClock;
    let mut ids = UlidIdSource;
    let first = append_event_reserved(
        service.vault(),
        service.local(),
        &key,
        pulse_draft(&session, "reserved start"),
        Some(reserved.clone()),
        &clock,
        &mut ids,
    )
    .expect("first reserved append");
    assert_eq!(first.event.id, reserved);
    let first_path = first.relative_path.clone();
    let retry = append_event_reserved(
        service.vault(),
        service.local(),
        &key,
        pulse_draft(&session, "reserved start"),
        Some(reserved.clone()),
        &clock,
        &mut ids,
    )
    .expect("retry returns the original event");
    assert_eq!(retry.event.id, reserved);
    assert_eq!(retry.event.seq, first.event.seq);
    assert_eq!(retry.byte_end, first.byte_end);
    let path = service.vault().root().join(&first_path);
    let text = fs::read_to_string(&path).expect("writer file");
    assert_eq!(text.lines().count(), 1, "the retry appended no duplicate");

    let error = append_event_reserved(
        service.vault(),
        service.local(),
        &key,
        pulse_draft(&session, "different content"),
        Some(reserved),
        &clock,
        &mut ids,
    )
    .expect_err("a reserved ID with different content freezes the writer");
    assert_eq!(error.code(), agendar_core::error::ErrorCode::WriterFork);
}

#[test]
fn a_dead_pid_writes_exactly_one_session_lost_and_a_live_pid_writes_none() {
    let scratch = scratch();
    let (service, created) = Service::init(&scratch.vault, &scratch.data).expect("init");
    assert!(created);

    let live_pid = std::process::id();
    let live_start = process_identity(live_pid).expect("test process identity");
    let live = register_session(
        &service,
        registration(&scratch, "live-session", live_pid, live_start),
    )
    .expect("register live");

    let (dead_pid, dead_start, child) = spawn_sleeping_child();
    let dead = register_session(
        &service,
        registration(&scratch, "dead-session", dead_pid, dead_start),
    )
    .expect("register dead");
    end_child(child, dead_pid);
    assert!(process_identity(dead_pid).is_none(), "child is gone");

    let first = steward_tick(&service).expect("first tick");
    assert!(first.processed >= 2);
    assert_eq!(first.lost, 1);
    let second = steward_tick(&service).expect("second tick");
    assert_eq!(second.lost, 0, "a lost session is never reported twice");

    let now = SystemClock.now();
    let fresh = service.refresh(now, false).expect("refresh");
    let project = fresh
        .state
        .projects
        .get(&ProjectSlug::parse("demo").unwrap())
        .expect("project");
    assert_eq!(
        project.sessions.get(&live).expect("live state").liveness,
        Liveness::Live
    );
    let lost_state = project.sessions.get(&dead).expect("dead state");
    assert_eq!(lost_state.liveness, Liveness::Lost);
    assert!(lost_state.lost_event.is_some());

    let lost_events: Vec<_> = fresh
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayload::SessionLost(body) if body.session == dead
            )
        })
        .collect();
    assert_eq!(lost_events.len(), 1, "exactly one session.lost fact");

    // The lost session writer is sealed: no later append may reopen it.
    let key = WriterKey {
        project: ProjectSlug::parse("demo").unwrap(),
        writer: WriterId::new(service.host().clone(), WriterRole::Agent(dead.clone())),
    };
    let draft = EventDraft {
        refs: Refs::default(),
        payload: EventPayload::Pulse(Pulse {
            agent: Agent {
                session: dead.clone(),
                harness: Harness::Generic,
            },
            message: "late pulse".to_owned(),
            boundary: true,
            eta: None,
        }),
    };
    let clock = SystemClock;
    let mut ids = UlidIdSource;
    let error = append_event(
        service.vault(),
        service.local(),
        &key,
        draft,
        &clock,
        &mut ids,
    )
    .expect_err("sealed writer refuses appends");
    assert_eq!(error.code(), agendar_core::error::ErrorCode::WriterFork);
    assert_eq!(error.detail(), Some("writer_sealed"));
}

#[test]
fn registration_reuses_a_live_identity_and_allocates_after_a_process_change() {
    let scratch = scratch();
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let pid = std::process::id();
    let start = process_identity(pid).expect("identity");

    let first = register_session(&service, registration(&scratch, "same-session", pid, start))
        .expect("first registration");
    let again = register_session(&service, registration(&scratch, "same-session", pid, start))
        .expect("resumed registration");
    assert_eq!(first, again, "same process identity reuses the session");

    let (other_pid, other_start, mut child) = spawn_sleeping_child();
    let other = register_session(
        &service,
        registration(&scratch, "same-session", other_pid, other_start),
    )
    .expect("new process identity");
    child.kill().expect("kill");
    child.wait().expect("reap");
    assert_ne!(
        first, other,
        "changed process identity allocates a new session"
    );

    let now = SystemClock.now();
    let fresh = service.refresh(now, false).expect("refresh");
    let project = fresh
        .state
        .projects
        .get(&ProjectSlug::parse("demo").unwrap())
        .expect("project");
    assert!(project.sessions.contains_key(&first));
    assert!(project.sessions.contains_key(&other));
    // One start pulse per session; the resumed registration wrote no duplicate.
    let pulses = fresh
        .events
        .iter()
        .filter(|event| matches!(event.payload, EventPayload::Pulse(_)))
        .count();
    assert_eq!(pulses, 2);
}

#[test]
fn a_codex_turn_process_exit_is_resumable_and_keeps_the_writer_open() {
    let scratch = scratch();
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let (pid, start, child) = spawn_sleeping_child();
    let session = register_session(
        &service,
        registration_for(&scratch, Harness::Codex, "thread-resume", pid, start),
    )
    .expect("register codex session");
    end_child(child, pid);
    assert!(process_identity(pid).is_none(), "the turn process is gone");

    let first = steward_tick(&service).expect("first tick");
    assert_eq!(first.lost, 0, "a Codex turn exit is never a loss");
    assert_eq!(first.resumable, 1);
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

    let fresh = service.refresh(SystemClock.now(), false).expect("refresh");
    assert!(
        !fresh
            .events
            .iter()
            .any(|event| matches!(&event.payload, EventPayload::SessionLost(_))),
        "no session.lost fact is written for a Codex turn exit"
    );
    let state = fresh
        .state
        .projects
        .get(&ProjectSlug::parse("demo").unwrap())
        .expect("project");
    let session_state = state.sessions.get(&session).expect("session state");
    assert!(session_state.lost_event.is_none());
    assert_ne!(session_state.liveness, Liveness::Lost);

    // The writer stays open: the next turn of the same thread may append.
    let key = WriterKey {
        project: ProjectSlug::parse("demo").unwrap(),
        writer: WriterId::new(service.host().clone(), WriterRole::Agent(session.clone())),
    };
    let clock = SystemClock;
    let mut ids = UlidIdSource;
    append_event(
        service.vault(),
        service.local(),
        &key,
        pulse_draft(&session, "next turn"),
        &clock,
        &mut ids,
    )
    .expect("a resumable session writer is not sealed");
}

#[test]
fn a_claude_session_keeps_the_pid_loss_rule_after_the_resume_window() {
    let scratch = scratch();
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let (pid, start, mut child) = spawn_sleeping_child();
    let session = register_session(
        &service,
        registration_for(&scratch, Harness::ClaudeCode, "claude-1", pid, start),
    )
    .expect("register claude session");
    child.kill().expect("kill child");
    child.wait().expect("reap child");

    // `claude --resume` may bring it back within the window: quiet, not lost.
    let early = steward_tick(&service).expect("tick");
    assert_eq!(early.lost, 0);
    assert_eq!(early.resumable, 1);

    let later = SystemClock
        .now()
        .add_seconds(agendar_core::runtime::CLAUDE_RESUME_GRACE_S + 60);
    let report = steward_tick_at(&service, later).expect("tick after the window");
    assert_eq!(
        report.lost, 1,
        "past the window a Claude process exit is a loss"
    );
    assert_eq!(report.resumable, 0);

    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let record = runtime.sessions.get(&session).expect("session");
    assert!(record.lost && record.sealed);
    assert!(!record.resumable);

    let fresh = service.refresh(SystemClock.now(), false).expect("refresh");
    let state = fresh
        .state
        .projects
        .get(&ProjectSlug::parse("demo").unwrap())
        .expect("project");
    assert_eq!(
        state
            .sessions
            .get(&session)
            .expect("session state")
            .liveness,
        Liveness::Lost
    );
}

#[test]
fn runtime_registrations_survive_index_removal_and_never_use_vault_state() {
    let scratch = scratch();
    let (service, _) = Service::init(&scratch.vault, &scratch.data).expect("init");
    let pid = std::process::id();
    let start = process_identity(pid).expect("identity");
    let session = register_session(&service, registration(&scratch, "durable", pid, start))
        .expect("register");

    let vault_id = service.vault().id().clone();
    let runtime_path = service.local().runtime_path(&vault_id);
    assert!(runtime_path.is_file());
    assert!(!runtime_path.starts_with(service.vault().root()));

    // Delete only the disposable index; the authoritative runtime journal stays.
    for suffix in ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
        let path = service.local().vault_local_dir(&vault_id).join(suffix);
        let _ = fs::remove_file(path);
    }
    let fresh = service.refresh(SystemClock.now(), false).expect("reopen");
    assert!(
        fresh
            .state
            .projects
            .contains_key(&ProjectSlug::parse("demo").unwrap())
    );

    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    assert!(runtime.sessions.contains_key(&session));
    assert!(runtime.revision >= 3);
    let registered: &SessionId = &session;
    assert_eq!(
        runtime
            .sessions
            .get(registered)
            .expect("session")
            .harness_session_id,
        "durable"
    );
}
