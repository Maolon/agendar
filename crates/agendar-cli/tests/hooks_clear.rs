//! Claude Code hooks across `/clear`: one Agendar session per conversation,
//! `init --session` keeps a session, and Stop holds only for what the
//! session owes, once per state.

mod support;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use serde_json::{Value, json};

use support::{CliEnv, expect_success};

/// Run one hook entry point with this stdin, as harness process `pid`.
fn hook(env: &CliEnv, action: &str, input: &Value, pid: u32) -> Output {
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["hook", action])
        .env("AGENDAR_HOOK_PID", pid.to_string())
        .env("AGENDAR_HOOK_BUDGET_MS", "2000")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(&serde_json::to_vec(input).expect("hook input"))
        .expect("write hook stdin");
    child.wait_with_output().expect("wait hook")
}

fn me() -> u32 {
    std::process::id()
}

fn start_input(id: &str, cwd: &Path, source: &str) -> Value {
    json!({
        "session_id": id,
        "cwd": cwd,
        "hook_event_name": "SessionStart",
        "source": source,
    })
}

fn stop_input(id: &str, cwd: &Path) -> Value {
    json!({
        "session_id": id,
        "cwd": cwd,
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Waiting for the workers.",
    })
}

fn ok(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The Agendar session a SessionStart output names.
fn started(output: &Output) -> String {
    let value: Value = serde_json::from_str(&ok(output)).expect("SessionStart JSON");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext");
    let marker = "Agendar session ";
    let from = context.find(marker).expect("session marker") + marker.len();
    context[from..].split_once('.').expect("end").0.to_owned()
}

fn silent(output: &Output) {
    assert_eq!(ok(output), "", "expected no hook output");
}

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("json")).expect("write");
    path.to_string_lossy().into_owned()
}

/// A project with these planned steps, declared by `session`.
fn declare(env: &CliEnv, session: &str, nodes: &[&str]) {
    let project = write_json(
        env,
        "project.json",
        &json!({"title": "Clear", "brief": "Scratch.", "goal": "Hooks.", "glyph": "radar"}),
    );
    expect_success(
        &env.run_as(session, &["project", "declare", "--json", &project]),
        "project declare",
    );
    for node in nodes {
        let feature = write_json(
            env,
            "feature.json",
            &json!({"title": node, "slice": node, "demo_kind": "cli_transcript"}),
        );
        expect_success(
            &env.run_as(
                session,
                &["feature", "declare", "--node", node, "--json", &feature],
            ),
            "feature declare",
        );
    }
}

fn start_step(env: &CliEnv, session: &str, node: &str, eta: &str) -> String {
    let value = expect_success(
        &env.run_as(
            session,
            &["progress", "started", "--node", node, "--eta", eta],
        ),
        "progress",
    );
    value["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned()
}

fn init_claude(env: &CliEnv, session: Option<&str>, harness_session: &str, pid: u32) -> Value {
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["--project", &env.project]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command
        .args(["init", "--harness", "claude_code", "--harness-session"])
        .arg(harness_session)
        .args(["--pid", &pid.to_string(), "--repo"])
        .arg(&env.repo);
    expect_success(&command.output().expect("init"), "init")
}

// Observation 1: after `/clear` the conversation has a new harness session id.

#[test]
fn clear_keeps_the_agendar_session_and_its_steps() {
    let mut env = CliEnv::new("demo");
    env.init();
    let first = started(&hook(
        &env,
        "session-start",
        &start_input("before-clear", &env.repo, "startup"),
        me(),
    ));
    declare(&env, &first, &["core/wake"]);
    start_step(&env, &first, "core/wake", "10m..20m");

    for (id, source) in [("after-clear", "clear"), ("after-compact", "compact")] {
        let next = started(&hook(
            &env,
            "session-start",
            &start_input(id, &env.repo, source),
            me(),
        ));
        assert_eq!(next, first, "{source} continues the Agendar session");
        silent(&hook(&env, "stop", &stop_input(id, &env.repo), me()));
    }
    // The step is still this session's.
    let settle = expect_success(&env.run_as(&first, &["settle"]), "settle");
    assert_eq!(settle["data"]["yours"][0]["node"], "core/wake");
}

#[test]
fn a_stop_under_a_new_id_with_no_session_start_continues_the_session() {
    // The observed case: SessionStart after `/clear` never reached this
    // project, and the first hook to see the new id was Stop.
    let mut env = CliEnv::new("demo");
    env.init();
    let first = started(&hook(
        &env,
        "session-start",
        &start_input("before-clear", &env.repo, "startup"),
        me(),
    ));
    declare(&env, &first, &[]);
    let stop = hook(&env, "stop", &stop_input("after-clear", &env.repo), me());
    assert!(
        !String::from_utf8_lossy(&stop.stderr).contains("Missing harness session registration"),
        "the new id is this process's conversation"
    );
    silent(&stop);
    let again = started(&hook(
        &env,
        "session-start",
        &start_input("after-clear", &env.repo, "resume"),
        me(),
    ));
    assert_eq!(again, first);
}

#[test]
fn hooks_from_an_unlinked_umbrella_folder_speak_for_the_process_session() {
    let mut env = CliEnv::new("demo");
    env.init();
    let umbrella = env.root.path().to_path_buf();
    // Not registered yet: an unlinked folder is not Agendar's, silent.
    silent(&hook(
        &env,
        "session-start",
        &start_input("before-clear", &umbrella, "startup"),
        me(),
    ));
    let first = init_claude(&env, None, "before-clear", me())["data"]["session"]
        .as_str()
        .expect("session")
        .to_owned();
    let next = started(&hook(
        &env,
        "session-start",
        &start_input("after-clear", &umbrella, "clear"),
        me(),
    ));
    assert_eq!(next, first, "the umbrella folder resolves by the process");
    silent(&hook(
        &env,
        "stop",
        &stop_input("after-clear", &umbrella),
        me(),
    ));
}

#[test]
fn another_process_never_continues_a_session_by_a_new_id() {
    let mut env = CliEnv::new("demo");
    env.init();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("other harness process");
    let theirs = started(&hook(
        &env,
        "session-start",
        &start_input("their-conversation", &env.repo, "startup"),
        other.id(),
    ));
    // This process clears: it has no session of its own, so it gets a new one.
    let mine = started(&hook(
        &env,
        "session-start",
        &start_input("my-conversation", &env.repo, "clear"),
        me(),
    ));
    other.kill().expect("kill");
    other.wait().expect("reap");
    assert_ne!(mine, theirs);
}

// Observation 2: `init --session X --harness-session NEW`.

#[test]
fn init_with_a_session_binds_the_new_harness_id_to_it() {
    let mut env = CliEnv::new("demo");
    env.init();
    let first = init_claude(&env, None, "before-clear", me())["data"]["session"]
        .as_str()
        .expect("session")
        .to_owned();
    declare(&env, &first, &["core/wake"]);
    start_step(&env, &first, "core/wake", "10m..20m");

    let bound = init_claude(&env, Some(&first), "after-clear", me());
    assert_eq!(bound["data"]["session"], first.as_str(), "no new session");
    assert!(bound["data"].get("session_note").is_none());
    // The hooks now speak for the same session.
    silent(&hook(
        &env,
        "stop",
        &stop_input("after-clear", &env.repo),
        me(),
    ));
    let again = started(&hook(
        &env,
        "session-start",
        &start_input("after-clear", &env.repo, "resume"),
        me(),
    ));
    assert_eq!(again, first);
}

#[test]
fn init_never_binds_a_session_another_running_process_holds() {
    let mut env = CliEnv::new("demo");
    env.init();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("other harness process");
    let theirs = init_claude(&env, None, "their-conversation", other.id())["data"]["session"]
        .as_str()
        .expect("session")
        .to_owned();

    let refused = init_claude(&env, Some(&theirs), "my-conversation", me());
    assert_ne!(refused["data"]["session"], theirs.as_str());
    let note = refused["data"]["session_note"].as_str().expect("note");
    assert!(note.contains("another running process holds it"), "{note}");

    // Its process is gone: the session may be taken back.
    other.kill().expect("kill");
    other.wait().expect("reap");
    let taken = init_claude(&env, Some(&theirs), "resumed-conversation", me());
    assert_eq!(taken["data"]["session"], theirs.as_str());
}

// Observation 3: Stop holds for what the session owes, once per state.

#[test]
fn stop_never_holds_for_planned_steps_nobody_holds() {
    let mut env = CliEnv::new("demo");
    env.init();
    let session = started(&hook(
        &env,
        "session-start",
        &start_input("parent-session", &env.repo, "startup"),
        me(),
    ));
    declare(
        &env,
        &session,
        &["backlog/tauri3", "next/hooks", "review/board"],
    );
    for _ in 0..3 {
        let stop = hook(&env, "stop", &stop_input("parent-session", &env.repo), me());
        assert!(
            !String::from_utf8_lossy(&stop.stdout).contains("Work remains"),
            "planned steps are not this session's debt"
        );
        silent(&stop);
    }
    // `settle` still counts them.
    let settle = expect_success(&env.run_as(&session, &["settle"]), "settle");
    assert_eq!(settle["data"]["planned"], 3);
}

#[test]
fn stop_holds_once_per_state_of_a_step_past_its_eta() {
    let mut env = CliEnv::new("demo");
    env.init();
    let session = started(&hook(
        &env,
        "session-start",
        &start_input("parent-session", &env.repo, "startup"),
        me(),
    ));
    declare(&env, &session, &["core/wake"]);
    let attempt = start_step(&env, &session, "core/wake", "1s..2s");
    std::thread::sleep(Duration::from_millis(3600));

    let stop = || hook(&env, "stop", &stop_input("parent-session", &env.repo), me());
    let held: Value = serde_json::from_str(&ok(&stop())).expect("block");
    assert_eq!(held["decision"], "block");
    assert!(
        held["reason"]
            .as_str()
            .expect("reason")
            .contains("past its estimate")
    );
    // Later turns of the same conversation, same state: not held again.
    silent(&stop());
    silent(&stop());

    // A new ETA that passes is a new state: held once more.
    expect_success(
        &env.run_as(
            &session,
            &[
                "pulse",
                "--node",
                "core/wake",
                "--attempt",
                &attempt,
                "--eta",
                "1s..2s",
                "--message",
                "still on it",
            ],
        ),
        "pulse",
    );
    silent(&stop());
    std::thread::sleep(Duration::from_millis(3600));
    let held: Value = serde_json::from_str(&ok(&stop())).expect("block");
    assert_eq!(held["decision"], "block");
    silent(&stop());
}
