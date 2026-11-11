//! Claude Code hook protocol outputs, both Stop flag values and the shim budget.

mod support;

use std::fs;
use std::io::Write;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use support::{CliEnv, expect_success};

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("serialize")).expect("write json");
    path.to_string_lossy().into_owned()
}

fn run_hook(
    env: &CliEnv,
    action: &str,
    harness_session: &str,
    stop_hook_active: Option<bool>,
    budget_ms: Option<u64>,
) -> Output {
    run_hook_with_pid(
        env,
        action,
        harness_session,
        stop_hook_active,
        budget_ms,
        std::process::id(),
    )
}

fn run_hook_with_pid(
    env: &CliEnv,
    action: &str,
    harness_session: &str,
    stop_hook_active: Option<bool>,
    budget_ms: Option<u64>,
    pid: u32,
) -> Output {
    let input = json!({
        "session_id": harness_session,
        "cwd": env.repo,
        "hook_event_name": match action {
            "session-start" => "SessionStart",
            "post-tool-use" => "PostToolUse",
            "user-prompt-submit" => "UserPromptSubmit",
            _ => "Stop",
        },
        "stop_hook_active": stop_hook_active,
        "transcript_path": "/tmp/not-ingested",
    });
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("hook")
        .arg(action)
        .env("AGENDAR_HOOK_PID", pid.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(budget) = budget_ms {
        command.env("AGENDAR_HOOK_BUDGET_MS", budget.to_string());
    }
    let mut child = command.spawn().expect("spawn hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(serde_json::to_vec(&input).expect("hook input").as_slice())
        .expect("write hook stdin");
    child.wait_with_output().expect("wait hook")
}

fn single_json(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let first = lines.next().unwrap_or_default();
    assert!(lines.next().is_none(), "hook wrote extra lines: {stdout}");
    serde_json::from_str(first).expect("hook JSON")
}

/// SessionStart for an explicit hook cwd and an optional explicit project.
fn hook_start_in(
    env: &CliEnv,
    cwd: &std::path::Path,
    project: Option<&str>,
    project_env: Option<&str>,
    harness_session: &str,
) -> Output {
    let input = json!({
        "session_id": harness_session,
        "cwd": cwd,
        "hook_event_name": "SessionStart",
        "stop_hook_active": null,
        "transcript_path": "/tmp/not-ingested",
    });
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data);
    if let Some(project) = project {
        command.arg("--project").arg(project);
    }
    if let Some(project) = project_env {
        command.env("AGENDAR_PROJECT", project);
    }
    command
        .arg("hook")
        .arg("session-start")
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(serde_json::to_vec(&input).expect("hook input").as_slice())
        .expect("write hook stdin");
    child.wait_with_output().expect("wait hook")
}
fn assert_no_stdout(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "expected no stdout, got {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn session_from_context(context: &str) -> String {
    let marker = "Agendar session ";
    let start = context.find(marker).expect("context session marker") + marker.len();
    context[start..]
        .split_once('.')
        .expect("session terminator")
        .0
        .to_owned()
}

fn start(env: &CliEnv, harness_session: &str) -> (String, String) {
    let output = run_hook(env, "session-start", harness_session, None, None);
    let value = single_json(&output);
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned();
    let session = session_from_context(&context);
    assert!(
        context.contains("--vault"),
        "context has an explicit command prefix: {context}"
    );
    let cli = agendar_core::runtime::paths::display_path(
        &std::path::PathBuf::from(env!("CARGO_BIN_EXE_agendar"))
            .canonicalize()
            .expect("canonical CLI path"),
    );
    assert!(
        context.contains(&cli),
        "the prefix starts with the absolute agendar path {cli:?}: {context}"
    );
    assert!(
        !context.contains("agendar --vault"),
        "a bare agendar token is never printed as an instruction: {context}"
    );
    assert!(context.contains(&session), "context names its session");
    (session, context)
}

fn declare_board(env: &CliEnv, session: &str) {
    let project = json!({
        "title": "Hook project",
        "brief": "Scratch hook board.",
        "goal": "Prove the hook loop.",
        "glyph": "radar"
    });
    let path = write_json(env, "project.json", &project);
    expect_success(
        &env.run_as(session, &["project", "declare", "--json", &path]),
        "project declare",
    );
    let feature = json!({
        "title": "Hook feature",
        "slice": "core/wake",
        "demo_kind": "cli_transcript"
    });
    let path = write_json(env, "feature.json", &feature);
    expect_success(
        &env.run_as(
            session,
            &["feature", "declare", "--node", "core/wake", "--json", &path],
        ),
        "feature declare",
    );
}

fn ask_as(env: &CliEnv, session: &str) -> String {
    let path = CliEnv::fixture("ask.json");
    let value = expect_success(
        &env.run_as(
            session,
            &[
                "ask",
                "--node",
                "core/wake",
                "--mode",
                "ask",
                "--json",
                path.to_str().unwrap(),
            ],
        ),
        "ask",
    );
    value["data"]["ask_id"].as_str().unwrap().to_owned()
}

#[test]
fn session_start_reuses_identity_and_uses_distinct_prefixes() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (first, first_context) = start(&env, "harness-a");
    let (again, _) = start(&env, "harness-a");
    assert_eq!(first, again, "same harness identity reuses its session");
    let (second, second_context) = start(&env, "harness-b");
    assert_ne!(first, second, "two harness sessions get distinct IDs");
    assert!(first_context.contains("--session"));
    assert!(second_context.contains("--session"));
    assert_ne!(first_context, second_context);

    declare_board(&env, &first);
    let first_ask = ask_as(&env, &first);
    let second_ask = ask_as(&env, &second);
    assert!(first_ask != second_ask);

    let writers: Vec<String> = env
        .event_lines()
        .into_iter()
        .filter(|line| line.contains(&format!("s-{first}")))
        .collect();
    assert!(!writers.is_empty());
    let second_writers: Vec<String> = env
        .event_lines()
        .into_iter()
        .filter(|line| line.contains(&format!("s-{second}")))
        .collect();
    assert!(!second_writers.is_empty());
}

#[test]
fn hook_project_override_wins_over_the_repository_link() {
    let mut env = CliEnv::new("override");
    env.init();
    let project = json!({
        "title": "Override board",
        "brief": "Scratch override board.",
        "goal": "Prove the explicit project wins.",
        "glyph": "radar"
    });
    let path = write_json(&env, "override-project.json", &project);
    expect_success(
        &env.run(&["project", "declare", "--json", &path]),
        "project declare",
    );
    let linked = agendar_core::service::shell_quote("linked");
    let explicit = agendar_core::service::shell_quote(&env.project);

    // 1. No repository link at all: hooks run in every repository on the
    // machine, so in one that is not linked the shim says nothing and succeeds.
    let unlinked = env.root.path().join("unlinked");
    fs::create_dir_all(&unlinked).expect("unlinked dir");
    let output = hook_start_in(&env, &unlinked, None, None, "harness-unlinked");
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unlinked cwd is not an error"
    );
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "the shim is silent in an unlinked repository: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // 2. The explicit project resolves the same unlinked cwd.
    let output = hook_start_in(&env, &unlinked, Some(&env.project), None, "harness-flag");
    let value = single_json(&output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned();
    assert!(
        context.contains(&format!("--project {explicit}")),
        "the explicit project is in the prefix: {context}"
    );
    let flagged = session_from_context(&context);
    let (again, again_context) = start(&env, "harness-flag");
    assert_eq!(flagged, again, "the flagged session is reusable");
    assert!(again_context.contains(&format!("--project {explicit}")));

    // 3. A conflicting link: the cwd link names another project, the flag wins.
    let linked_repo = env.root.path().join("linked-repo");
    fs::create_dir_all(&linked_repo).expect("linked repo");
    let mut link = env.command();
    let output = link
        .current_dir(&linked_repo)
        .args(["project", "link", "--repo"])
        .arg(&linked_repo)
        .args(["--project", "linked"])
        .output()
        .expect("run project link");
    expect_success(&output, "project link");
    let output = hook_start_in(
        &env,
        &linked_repo,
        Some(&env.project),
        None,
        "harness-conflict",
    );
    let value = single_json(&output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned();
    assert!(
        context.contains(&format!("--project {explicit}")),
        "the explicit project beats the cwd link: {context}"
    );
    assert!(
        !context.contains(&format!("--project {linked}")),
        "the cwd link is not used when --project is explicit: {context}"
    );

    // 4. `AGENDAR_PROJECT` is an explicit override too.
    let output = hook_start_in(&env, &linked_repo, None, Some(&env.project), "harness-env");
    let value = single_json(&output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned();
    assert!(
        context.contains(&format!("--project {explicit}")),
        "AGENDAR_PROJECT beats the cwd link: {context}"
    );
}

#[test]
fn a_new_process_identity_for_the_same_harness_session_allocates_a_new_session() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (first, _) = start(&env, "harness-a");

    let mut child = std::process::Command::new("sleep")
        .arg("10")
        .spawn()
        .expect("spawn child");
    let output = run_hook_with_pid(&env, "session-start", "harness-a", None, None, child.id());
    child.kill().expect("kill child");
    child.wait().expect("reap child");
    let value = single_json(&output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context")
        .to_owned();
    let second = session_from_context(&context);
    assert_ne!(
        first, second,
        "a changed process identity allocates a new Agendar session"
    );
}

#[test]
fn post_tool_use_is_silent_until_an_unseen_packet_exists() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    assert_no_stdout(&run_hook(&env, "post-tool-use", "harness-a", None, None));

    declare_board(&env, &session);
    let ask = ask_as(&env, &session);
    let answer = env.answer(&ask, "adapters");
    let output = run_hook(&env, "post-tool-use", "harness-a", None, None);
    let value = single_json(&output);
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(context.contains(&answer), "context names the answer id");

    // The first injection recorded the delivery; the next call has nothing new.
    assert_no_stdout(&run_hook(&env, "post-tool-use", "harness-a", None, None));
}

#[test]
fn stop_passes_planned_work_nobody_holds() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);

    // A planned step nobody holds is not this session's debt: `settle` lists
    // it, the Stop does not hold for it.
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(false), None));
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), None));

    // Started and finished: still nothing held, no stdout is written.
    let started = expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "1m..2m",
            ],
        ),
        "progress",
    );
    let attempt = started["data"]["attempt"].as_str().unwrap().to_owned();
    expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "done",
                "--node",
                "core/wake",
                "--attempt",
                &attempt,
            ],
        ),
        "progress",
    );
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(false), None));
}

#[test]
fn stop_waits_within_its_budget_then_prints_the_wait_fallback() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let ask = ask_as(&env, &session);

    let started = Instant::now();
    let output = run_hook(&env, "stop", "harness-a", Some(false), Some(3000));
    let elapsed = started.elapsed();
    let value = single_json(&output);
    assert_eq!(value["decision"], "block");
    let reason = value["reason"].as_str().expect("reason");
    assert!(
        reason.contains(&format!("wait {ask}")),
        "fallback names the wait command: {reason}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "the shim must finish inside its budget, took {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "the shim actually waited, took {elapsed:?}"
    );

    // A repeated Stop with nothing new exits; it does not promise an endless hold.
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), Some(500)));
}

#[test]
fn stop_wakes_on_an_answer_and_never_acks_for_the_agent() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let ask = ask_as(&env, &session);

    let mut command: Command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("hook")
        .arg("stop")
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .env("AGENDAR_HOOK_BUDGET_MS", "6000")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn stop");
    let input = json!({
        "session_id": "harness-a",
        "cwd": env.repo,
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(serde_json::to_vec(&input).expect("input").as_slice())
        .expect("write stdin");
    std::thread::sleep(Duration::from_millis(600));
    let answer = env.answer(&ask, "adapters");
    let output = child.wait_with_output().expect("wait stop");
    let value = single_json(&output);
    assert_eq!(value["decision"], "block");
    let reason = value["reason"].as_str().expect("reason");
    assert!(reason.contains("New answer available"), "reason={reason}");
    assert!(reason.contains(&format!("ack {answer}")), "reason={reason}");
    assert_eq!(env.count_kind("decision.ack"), 0, "the hook never acks");

    // Hold once: the Stop that follows a hold ends the turn even though the
    // answer is still unseen; PostToolUse still injects it on the next tool.
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), Some(500)));
    let delivered = single_json(&run_hook(&env, "post-tool-use", "harness-a", None, None));
    assert_eq!(
        delivered["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), Some(500)));
}

// ---------------------------------------------------------------------------
// install-hooks: dry run, --yes, idempotence, backup, unrelated keys, symlink
// ---------------------------------------------------------------------------

fn run_install(env: &CliEnv, args: &[&str]) -> Output {
    let mut command = env.command();
    command.args(["install-hooks", "--harness", "claude_code"]);
    command.args(args);
    command.output().expect("run install-hooks")
}

fn expect_install(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "install-hooks failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("install-hooks envelope")
}

fn backup_count(env: &CliEnv) -> usize {
    let directory = env.home.join(".claude");
    fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .contains("agendar-backup")
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn install_hooks_is_a_dry_run_by_default() {
    let env = CliEnv::new("demo");
    let output = run_install(&env, &[]);
    let envelope = expect_install(&output);
    assert_eq!(envelope["data"]["installed"], false);
    let plan = &envelope["data"]["plan"];
    assert_eq!(plan["harness"], "claude_code");
    assert_eq!(plan["changes"].as_array().expect("changes").len(), 5);
    assert_eq!(plan["requires_consent"], true);
    assert!(
        plan["settings_path"]
            .as_str()
            .expect("settings path")
            .replace('\\', "/")
            .ends_with(".claude/settings.json")
    );
    assert!(
        !env.home.join(".claude/settings.json").exists(),
        "a dry run creates nothing"
    );
}

#[test]
fn install_hooks_writes_once_preserves_unrelated_keys_and_is_idempotent() {
    let env = CliEnv::new("demo");
    let settings = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().expect("settings parent")).expect("settings dir");
    let original = json!({
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {
            "PreToolUse": [{"matcher": "Write", "hooks": [
                {"type": "command", "command": "someone-elses-hook", "timeout": 5}
            ]}]
        }
    });
    fs::write(
        &settings,
        serde_json::to_vec_pretty(&original).expect("json"),
    )
    .expect("settings");

    let first = expect_install(&run_install(&env, &["--yes"]));
    assert_eq!(first["data"]["installed"], true);
    let written: Value =
        serde_json::from_slice(&fs::read(&settings).expect("read settings")).expect("json");
    assert_eq!(written["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(
        written["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "someone-elses-hook",
        "unrelated hooks are preserved"
    );
    for event in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
        let commands: Vec<&str> = written["hooks"][event]
            .as_array()
            .expect("event hooks")
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().expect("hook list"))
            .map(|hook| hook["command"].as_str().expect("command"))
            .collect();
        assert!(
            commands.iter().any(|command| command.contains(" hook ")),
            "{event} has the agendar hook: {commands:?}"
        );
    }
    assert_eq!(backup_count(&env), 1, "one backup per change");

    let before = fs::read(&settings).expect("bytes");
    let second = expect_install(&run_install(&env, &["--yes"]));
    assert_eq!(second["data"]["plan"]["already_installed"], true);
    assert_eq!(
        second["data"]["plan"]["changes"].as_array().unwrap().len(),
        0
    );
    assert_eq!(fs::read(&settings).expect("bytes"), before, "idempotent");
    assert_eq!(backup_count(&env), 1, "no backup without a change");
}

#[test]
fn install_hooks_refuses_malformed_settings_without_writing() {
    let env = CliEnv::new("demo");
    let settings = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().expect("settings parent")).expect("settings dir");
    fs::write(&settings, b"{not json").expect("malformed settings");
    let output = run_install(&env, &["--yes"]);
    assert_ne!(output.status.code(), Some(0));
    assert_eq!(fs::read(&settings).expect("bytes"), b"{not json");
    assert_eq!(backup_count(&env), 0);
}

#[test]
fn install_hooks_accepts_an_explicit_settings_path() {
    let env = CliEnv::new("demo");
    let settings = env.root.path().join("custom/claude-settings.json");
    let output = run_install(
        &env,
        &["--yes", "--settings", settings.to_str().expect("path")],
    );
    assert_eq!(expect_install(&output)["data"]["installed"], true);
    assert!(settings.is_file());
    assert!(!env.home.join(".claude/settings.json").exists());
}

#[cfg(unix)]
#[test]
fn install_hooks_refuses_a_symlinked_settings_target() {
    use std::os::unix::fs::symlink;

    let env = CliEnv::new("demo");
    let target = env.root.path().join("real-settings.json");
    fs::write(&target, br#"{"permissions":{"allow":["Bash(ls)"]}}"#).expect("target");
    let settings = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().expect("settings parent")).expect("settings dir");
    symlink(&target, &settings).expect("symlink settings");

    let output = run_install(&env, &["--yes"]);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E_UNSUPPORTED"), "stderr={stderr}");
    assert_eq!(
        fs::read(&target).expect("target"),
        br#"{"permissions":{"allow":["Bash(ls)"]}}"#
    );
    assert_eq!(backup_count(&env), 0);
}

#[test]
fn install_hooks_rejects_other_harnesses() {
    let env = CliEnv::new("demo");
    let mut command = env.command();
    // Claude Code and Codex have installers; a generic harness has no hooks.
    command.args(["install-hooks", "--harness", "generic"]);
    let output = command.output().expect("run install-hooks");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E_UNSUPPORTED"), "stderr={stderr}");
}

// ---------------------------------------------------------------------------
// Stop budget includes the runtime lock wait and cleanup (1de N1)
// ---------------------------------------------------------------------------

#[test]
fn stop_budget_includes_a_held_runtime_lock() {
    use fs2::FileExt as _;
    use std::fs::OpenOptions;

    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let _ask = ask_as(&env, &session);

    let vaults = env.data.join("vaults");
    let vault_dir = fs::read_dir(&vaults)
        .expect("vaults")
        .flatten()
        .next()
        .expect("one vault")
        .path();
    let lock_path = vault_dir.join("locks").join("runtime.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("runtime lock file");
    lock.lock_exclusive().expect("hold runtime lock");

    let started = Instant::now();
    let output = run_hook(&env, "stop", "harness-a", Some(false), Some(3_000));
    let elapsed = started.elapsed();
    fs2::FileExt::unlock(&lock).expect("unlock");
    drop(lock);

    let value = single_json(&output);
    assert_eq!(value["decision"], "block");
    let reason = value["reason"].as_str().expect("reason");
    assert!(
        reason.contains("Answer still pending"),
        "the held lock must not turn into an error: {reason}"
    );
    assert!(
        elapsed < Duration::from_secs(3) + Duration::from_millis(500),
        "Stop exceeded its 3 s budget: {elapsed:?}"
    );
}

fn vault_lock_path(env: &CliEnv, name: &str) -> std::path::PathBuf {
    let vaults = env.data.join("vaults");
    let vault_dir = fs::read_dir(&vaults)
        .expect("vaults")
        .flatten()
        .next()
        .expect("one vault")
        .path();
    vault_dir.join("locks").join(name)
}

#[test]
fn stop_with_the_default_budget_and_a_held_runtime_lock_blocks_pending() {
    use fs2::FileExt as _;
    use std::fs::OpenOptions;

    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let _ask = ask_as(&env, &session);

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(vault_lock_path(&env, "runtime.lock"))
        .expect("runtime lock file");
    lock.lock_exclusive().expect("hold runtime lock");

    // No injected budget: the real default is 45 s. A held runtime lock must
    // not turn into an empty-stdout E_BUSY after the 5 s lock wait.
    let started = Instant::now();
    let output = run_hook(&env, "stop", "harness-a", Some(false), None);
    let elapsed = started.elapsed();
    fs2::FileExt::unlock(&lock).expect("unlock");
    drop(lock);

    let value = single_json(&output);
    assert_eq!(value["decision"], "block");
    let reason = value["reason"].as_str().expect("reason");
    assert!(
        reason.contains("Answer still pending"),
        "the held lock must not turn into an error: {reason}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "a held runtime lock must not consume the whole default budget: {elapsed:?}"
    );
}

#[test]
fn stop_near_the_deadline_with_ingest_contention_blocks_pending() {
    use fs2::FileExt as _;
    use std::fs::OpenOptions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let _ask = ask_as(&env, &session);

    // The refresh at the deadline must respect the caller's budget instead of
    // starting an unbounded ingest-lock wait after expiry. Hold the ingest lock
    // just before the (8 s - 2 s reserve) deadline of this injected budget.
    let release = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&release);
    let lock_path = vault_lock_path(&env, "ingest.lock");
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(4_000));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .expect("ingest lock file");
        file.lock_exclusive().expect("hold ingest lock");
        while !flag.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(25));
        }
        fs2::FileExt::unlock(&file).expect("unlock ingest lock");
    });

    let started = Instant::now();
    let output = run_hook(&env, "stop", "harness-a", Some(false), Some(8_000));
    let elapsed = started.elapsed();
    release.store(true, Ordering::SeqCst);
    holder.join().expect("ingest lock holder");

    let value = single_json(&output);
    assert_eq!(value["decision"], "block");
    let reason = value["reason"].as_str().expect("reason");
    assert!(
        reason.contains("Answer still pending"),
        "deadline contention must stay a pending answer: {reason}"
    );
    assert!(
        elapsed < Duration::from_millis(8_500),
        "Stop exceeded its 8 s budget: {elapsed:?}"
    );
}

fn observe(env: &CliEnv, input: &[u8]) -> Output {
    let mut child = env
        .command()
        .arg("--data-dir")
        .arg(&env.data)
        .arg("hook")
        .arg("observe")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn observe");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write stdin");
    child.wait_with_output().expect("wait observe")
}

#[test]
fn the_observe_hook_records_silently_and_never_fails_its_harness() {
    let env = CliEnv::new("demo");
    let event = |name: &str, extra: Value| {
        let mut input = json!({"hook_event_name": name, "session_id": "s-1", "cwd": "/work/app"});
        if let (Value::Object(base), Value::Object(more)) = (&mut input, extra) {
            base.extend(more);
        }
        serde_json::to_vec(&input).expect("input")
    };
    let read = || -> Value {
        serde_json::from_slice(&fs::read(env.data.join("presence.json")).expect("presence"))
            .expect("presence json")
    };

    let output = observe(&env, &event("UserPromptSubmit", json!({})));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "nothing the model could read");
    assert_eq!(read()["agents"]["claude_code:s-1"]["state"], "working");

    observe(
        &env,
        &event(
            "Notification",
            json!({"notification_type": "permission_prompt"}),
        ),
    );
    assert_eq!(read()["agents"]["claude_code:s-1"]["state"], "permission");

    observe(&env, &event("SessionEnd", json!({})));
    assert_eq!(read()["agents"], json!({}));

    let garbage = observe(&env, b"not json at all");
    assert_eq!(garbage.status.code(), Some(0), "never breaks the harness");
    assert!(garbage.stdout.is_empty());
}

#[test]
fn an_overdue_step_is_reminded_once_in_the_agents_context() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "1s..2s",
            ],
        ),
        "progress",
    );
    // Nothing new yet: the prompt hook stays silent.
    let quiet = run_hook(&env, "user-prompt-submit", "harness-a", None, None);
    assert!(
        String::from_utf8_lossy(&quiet.stdout).trim().is_empty(),
        "no reminder before the ETA passes"
    );
    // Lateness counts whole seconds: wait until it is past the 2 s bound by 1 s+.
    std::thread::sleep(std::time::Duration::from_millis(3600));
    let reminded = single_json(&run_hook(
        &env,
        "user-prompt-submit",
        "harness-a",
        None,
        None,
    ));
    assert_eq!(
        reminded["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    let context = reminded["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(context.contains("Agendar reminders"), "{context}");
    assert!(context.contains("core/wake"), "{context}");
    // Shown once: the next prompt and tool call stay silent.
    for action in ["user-prompt-submit", "post-tool-use"] {
        let again = run_hook(&env, action, "harness-a", None, None);
        assert!(
            String::from_utf8_lossy(&again.stdout).trim().is_empty(),
            "{action} repeated the reminder"
        );
    }
}

#[test]
fn stop_holds_once_for_a_step_past_its_estimate() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "1s..2s",
            ],
        ),
        "progress",
    );
    std::thread::sleep(Duration::from_millis(3600));
    let held = single_json(&run_hook(
        &env,
        "stop",
        "harness-a",
        Some(false),
        Some(2000),
    ));
    assert_eq!(held["decision"], "block");
    let reason = held["reason"].as_str().expect("reason");
    assert!(reason.contains("core/wake is"), "reason={reason}");
    assert!(reason.contains("past its estimate"), "reason={reason}");
    // The second Stop ends the turn.
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), Some(2000)));
}

fn spawn_rewake(env: &CliEnv, wait_ms: u64) -> std::process::Child {
    let mut command: Command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("hook")
        .arg("rewake")
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .env("AGENDAR_REWAKE_MS", wait_ms.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn rewake");
    let input = json!({
        "session_id": "harness-a",
        "cwd": env.repo,
        "hook_event_name": "Stop",
        "stop_hook_active": true,
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(serde_json::to_vec(&input).expect("input").as_slice())
        .expect("write stdin");
    child
}

#[test]
fn rewake_wakes_an_idle_session_with_a_new_answer() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let ask = ask_as(&env, &session);
    let child = spawn_rewake(&env, 15_000);
    std::thread::sleep(Duration::from_millis(1500));
    let answer = env.answer(&ask, "adapters");
    let output = child.wait_with_output().expect("rewake");
    assert_eq!(output.status.code(), Some(2), "exit 2 wakes Claude Code");
    assert!(output.stdout.is_empty(), "nothing on stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("answered your question"), "{stderr}");
    assert!(stderr.contains(&format!("ack {answer}")), "{stderr}");
    assert_eq!(env.count_kind("decision.ack"), 0, "the hook never acks");
}

#[test]
fn rewake_never_wakes_for_an_answer_the_turn_already_had() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    // Nothing open: it ends at once, silently.
    let idle = spawn_rewake(&env, 5_000)
        .wait_with_output()
        .expect("rewake");
    assert_eq!(idle.status.code(), Some(0));
    assert!(idle.stderr.is_empty());

    let ask = ask_as(&env, &session);
    let _second = ask_as(&env, &session);
    env.answer(&ask, "adapters");
    // The turn ended knowing that answer; only a new one may wake it.
    let quiet = spawn_rewake(&env, 3_000)
        .wait_with_output()
        .expect("rewake");
    assert_eq!(
        quiet.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert!(quiet.stderr.is_empty());
}

#[test]
fn codex_hooks_install_into_codex_hooks_json_and_keep_other_hooks() {
    let env = CliEnv::new("demo");
    let codex_dir = env.home.join(".codex");
    fs::create_dir_all(&codex_dir).expect("codex dir");
    let file = codex_dir.join("hooks.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({"hooks": {"PreToolUse": [{"matcher": "^Bash$", "hooks": [{"type": "command", "command": "someone-elses-hook"}]}]}}))
            .expect("json"),
    )
    .expect("existing hooks");
    let mut command = env.command();
    command.args(["install-hooks", "--harness", "codex"]);
    let plan = expect_install(&command.output().expect("plan"));
    let plan = &plan["data"]["plan"];
    assert_eq!(plan["harness"], "codex");
    assert!(
        plan["settings_path"]
            .as_str()
            .expect("path")
            .replace('\\', "/")
            .ends_with(".codex/hooks.json")
    );
    let changes = plan["changes"].as_array().expect("changes");
    assert_eq!(changes.len(), 4, "no rewake for Codex");
    assert!(changes.iter().all(|change| {
        change["command"]
            .as_str()
            .unwrap_or_default()
            .contains("hook --harness codex ")
    }));

    let mut command = env.command();
    command.args(["install-hooks", "--harness", "codex", "--yes"]);
    expect_install(&command.output().expect("install"));
    let written: Value =
        serde_json::from_slice(&fs::read(&file).expect("hooks.json")).expect("json");
    assert_eq!(
        written["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "someone-elses-hook",
        "existing Codex hooks are kept"
    );
    for event in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
        assert!(
            written["hooks"][event]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()),
            "{event} installed"
        );
    }
}

#[test]
fn a_codex_session_registers_through_its_own_hook() {
    let mut env = CliEnv::new("demo");
    env.init();
    let input = json!({
        "session_id": "thr_codex_1",
        "cwd": env.repo,
        "hook_event_name": "SessionStart",
        "source": "startup",
    });
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["hook", "--harness", "codex", "session-start"])
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(serde_json::to_vec(&input).expect("input").as_slice())
        .expect("write");
    let value = single_json(&child.wait_with_output().expect("hook"));
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let service = env.service();
    let runtime = service.runtime().expect("runtime");
    let registered = runtime
        .sessions
        .values()
        .find(|session| session.harness_session_id == "thr_codex_1")
        .expect("registered");
    assert_eq!(registered.harness, agendar_core::model::Harness::Codex);
}

#[test]
fn hook_installs_honour_claude_config_dir_and_codex_home() {
    let env = CliEnv::new("demo");
    let claude = env.home.join("custom-claude");
    let codex = env.home.join("custom-codex");
    let mut command = env.command();
    command
        .args(["install-hooks", "--harness", "claude_code"])
        .env("CLAUDE_CONFIG_DIR", &claude);
    let plan = expect_install(&command.output().expect("plan"));
    let path = plan["data"]["plan"]["settings_path"]
        .as_str()
        .expect("path")
        .to_owned();
    assert_eq!(
        std::path::PathBuf::from(&path),
        claude.join("settings.json")
    );

    let mut command = env.command();
    command
        .args(["install-hooks", "--harness", "codex"])
        .env("CODEX_HOME", &codex);
    let plan = expect_install(&command.output().expect("plan"));
    let path = plan["data"]["plan"]["settings_path"]
        .as_str()
        .expect("path")
        .to_owned();
    assert_eq!(std::path::PathBuf::from(&path), codex.join("hooks.json"));
}

#[test]
fn hook_installs_fall_back_to_userprofile_without_home() {
    let env = CliEnv::new("demo");
    let profile = env.home.join("profile");
    let mut command = env.command();
    command
        .args(["install-hooks", "--harness", "codex"])
        .env_remove("HOME")
        .env("USERPROFILE", &profile);
    let plan = expect_install(&command.output().expect("plan"));
    let path = plan["data"]["plan"]["settings_path"]
        .as_str()
        .expect("path")
        .to_owned();
    assert_eq!(
        std::path::PathBuf::from(&path),
        profile.join(".codex").join("hooks.json"),
        "Windows sets USERPROFILE, usually not HOME"
    );
}

#[test]
fn a_claude_hook_inside_cursor_stays_silent() {
    let mut env = CliEnv::new("demo");
    env.init();
    let input = json!({
        "session_id": "cursor-chat-1",
        "conversation_id": "cursor-chat-1",
        "cwd": env.repo,
        "hook_event_name": "sessionStart",
        "cursor_version": "2026.11.05",
    });
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["hook", "session-start"])
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(serde_json::to_vec(&input).expect("input").as_slice())
        .expect("write");
    let output = child.wait_with_output().expect("hook");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "no Claude context inside Cursor");
    let runtime = env.service().runtime().expect("runtime");
    assert!(
        runtime
            .sessions
            .values()
            .all(|session| session.harness_session_id != "cursor-chat-1"),
        "Cursor's chat is not registered as a Claude Code session"
    );
}

#[test]
fn every_checked_harness_is_a_cli_value() {
    let env = CliEnv::new("demo");
    for harness in ["opencode", "cursor", "dsh", "agy", "gemini", "pi"] {
        let mut command = env.command();
        command.args(["install-hooks", "--harness", harness]);
        let output = command.output().expect("run");
        // Not a usage error: the value parses (the installer may not exist yet).
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("E_USAGE"), "{harness}: {stderr}");
    }
}

fn install_mcp(env: &CliEnv, harness: &str, yes: bool) -> Output {
    let mut command = env.command();
    command.args(["install-mcp", "--harness", harness]);
    if yes {
        command.arg("--yes");
    }
    command.output().expect("install-mcp")
}

#[test]
fn install_mcp_merges_each_harness_config_and_keeps_other_servers() {
    let env = CliEnv::new("demo");
    let cursor = env.home.join(".cursor").join("mcp.json");
    fs::create_dir_all(cursor.parent().expect("dir")).expect("dir");
    fs::write(
        &cursor,
        serde_json::to_vec(&json!({"mcpServers": {"other": {"command": "x"}}})).expect("json"),
    )
    .expect("cursor mcp");

    // A plan writes nothing.
    let plan = expect_install(&install_mcp(&env, "cursor", false));
    assert_eq!(plan["data"]["installed"], false);
    assert!(
        !fs::read_to_string(&cursor)
            .expect("read")
            .contains("agendar")
    );

    expect_install(&install_mcp(&env, "cursor", true));
    let written: Value = serde_json::from_slice(&fs::read(&cursor).expect("read")).expect("json");
    assert_eq!(
        written["mcpServers"]["other"]["command"], "x",
        "other servers kept"
    );
    assert_eq!(
        written["mcpServers"]["agendar"]["args"],
        json!(["mcp", "--harness", "cursor"])
    );
    let again = expect_install(&install_mcp(&env, "cursor", false));
    assert_eq!(again["data"]["plan"]["already_installed"], true);

    // pi: a new file; opencode: its own `mcp` shape.
    expect_install(&install_mcp(&env, "pi", true));
    let pi: Value =
        serde_json::from_slice(&fs::read(env.home.join(".pi/agent/mcp.json")).expect("pi mcp"))
            .expect("json");
    assert_eq!(pi["mcpServers"]["agendar"]["args"][2], "pi");
    expect_install(&install_mcp(&env, "opencode", true));
    let opencode: Value = serde_json::from_slice(
        &fs::read(env.home.join(".config/opencode/opencode.json")).expect("opencode"),
    )
    .expect("json");
    assert_eq!(opencode["mcp"]["agendar"]["type"], "local");
    assert_eq!(opencode["mcp"]["agendar"]["command"][3], "opencode");
}

#[test]
fn install_mcp_refuses_a_config_with_comments_and_explains_dsh() {
    let env = CliEnv::new("demo");
    let agy = env.home.join(".gemini/config/mcp_config.json");
    fs::create_dir_all(agy.parent().expect("dir")).expect("dir");
    let original = "// user notes\n{\"mcpServers\": {}}\n";
    fs::write(&agy, original).expect("agy config");
    let refused = install_mcp(&env, "agy", true);
    assert_ne!(refused.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&agy).expect("read"),
        original,
        "left untouched"
    );

    let dsh = install_mcp(&env, "dsh", false);
    assert_ne!(dsh.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&dsh.stderr);
    assert!(stderr.contains("mcp --harness dsh"), "{stderr}");
}

fn bindings_of(env: &CliEnv) -> Vec<Value> {
    let mut command = env.command();
    command.arg("bindings");
    let value = expect_install(&command.output().expect("bindings"));
    value["data"]["bindings"]
        .as_array()
        .expect("bindings")
        .clone()
}

fn state_of(env: &CliEnv, harness: &str) -> String {
    bindings_of(env)
        .iter()
        .find(|binding| binding["harness"] == harness)
        .map(|binding| binding["state"].as_str().unwrap_or_default().to_owned())
        .unwrap_or_default()
}

#[test]
fn an_agent_binds_itself_and_settings_can_unbind_it() {
    let env = CliEnv::new("demo");
    assert_eq!(
        bindings_of(&env).len(),
        8,
        "every bindable harness is listed"
    );
    assert_eq!(state_of(&env, "claude_code"), "not_bound");

    // An old Agendar install path is recognised as stale, and bind replaces it.
    let settings = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().expect("dir")).expect("dir");
    fs::write(
        &settings,
        serde_json::to_vec(&json!({"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "'/old/agendar' hook stop"}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": "someone-elses-hook"}]}]
        }}))
        .expect("json"),
    )
    .expect("settings");
    assert_eq!(state_of(&env, "claude_code"), "stale");
    let mut command = env.command();
    command.args(["bind", "--harness", "claude_code"]);
    let bound = expect_install(&command.output().expect("bind"));
    assert_eq!(bound["data"]["binding"]["state"], "bound");
    assert!(
        bound["data"]["next"]
            .as_str()
            .unwrap_or_default()
            .contains("Restart")
    );
    let text = fs::read_to_string(&settings).expect("read");
    assert!(!text.contains("/old/agendar"), "the stale hook is gone");
    assert!(text.contains("someone-elses-hook"), "other hooks stay");

    let mut command = env.command();
    command.args(["unbind", "--harness", "claude_code"]);
    let unbound = expect_install(&command.output().expect("unbind"));
    assert_eq!(unbound["data"]["binding"]["state"], "not_bound");
    let text = fs::read_to_string(&settings).expect("read");
    assert!(!text.contains(" hook "), "no Agendar hooks left");
    assert!(text.contains("someone-elses-hook"));

    // Without --harness, bind detects the harness from its environment.
    let mut command = env.command();
    command.arg("bind").env("CURSOR_AGENT", "1");
    let cursor = expect_install(&command.output().expect("bind cursor"));
    assert_eq!(cursor["data"]["binding"]["harness"], "cursor");
    assert_eq!(cursor["data"]["binding"]["kind"], "mcp");
    assert_eq!(state_of(&env, "cursor"), "bound");

    // No harness in the environment: a clear usage error, nothing written.
    let mut command = env.command();
    command.arg("bind");
    let unknown = command.output().expect("bind unknown");
    assert_ne!(unknown.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("--harness"));
}

#[test]
fn a_question_left_in_chat_is_sent_back_once_to_agendar() {
    use agendar_cli::hooks::asks_human;
    assert!(asks_human(
        "Built and tested.\n\nShould I delete the build cache?"
    ));
    assert!(asks_human("全部完成。\n\n要不要现在安装？"));
    assert!(asks_human("Done. Let me know which one you prefer."));
    assert!(!asks_human("Built and tested. Everything passes."));
    assert!(!asks_human(
        "Why did it fail? The cache was full.\n\nFixed it; all green."
    ));

    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let input = |active: bool| {
        json!({
            "session_id": "harness-a",
            "cwd": env.repo,
            "hook_event_name": "Stop",
            "stop_hook_active": active,
            "last_assistant_message": "All done.\n\nDo you want me to install it now?",
        })
    };
    let run = |value: Value| {
        let mut command = env.command();
        command
            .arg("--vault")
            .arg(&env.vault)
            .arg("--data-dir")
            .arg(&env.data)
            .args(["hook", "stop"])
            .env("AGENDAR_HOOK_PID", std::process::id().to_string())
            .env("AGENDAR_HOOK_BUDGET_MS", "2000")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(serde_json::to_vec(&value).expect("json").as_slice())
            .expect("write");
        child.wait_with_output().expect("stop")
    };
    let held = single_json(&run(input(false)));
    assert_eq!(held["decision"], "block");
    let reason = held["reason"].as_str().expect("reason");
    assert!(reason.contains("ask --question"), "{reason}");
    assert_no_stdout(&run(input(true)));
}

#[test]
fn an_empty_agent_config_is_bound_like_a_missing_one() {
    let env = CliEnv::new("demo");
    let agy = env.home.join(".gemini/config/mcp_config.json");
    fs::create_dir_all(agy.parent().expect("dir")).expect("dir");
    fs::write(&agy, "").expect("empty config");
    assert_eq!(state_of(&env, "agy"), "not_bound");
    expect_install(&install_mcp(&env, "agy", true));
    let written: Value = serde_json::from_slice(&fs::read(&agy).expect("read")).expect("json");
    assert_eq!(written["mcpServers"]["agendar"]["args"][2], "agy");
}

#[test]
fn a_sent_back_delivery_reaches_the_agents_context_once_with_the_reason() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    let started = expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "10m..20m",
            ],
        ),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let ask = ask_as(&env, &session);
    let answer = env.answer(&ask, "adapters");
    // The answer itself is injected first; that is not what this test is about.
    single_json(&run_hook(&env, "post-tool-use", "harness-a", None, None));

    let demo = env.repo.join("demo.txt");
    fs::write(&demo, b"demo transcript\n").expect("demo");
    let input = write_json(
        &env,
        "delivery.json",
        &json!({
            "name": "Wake delivery",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo, "ext": "txt"}}],
            "criteria": [{"criterion": "continues", "status": "passed", "claim": "agent_says"}],
            "disclosure": {"proven": ["Agent ran it"], "not_proven": [], "faked": []}
        }),
    );
    let delivered = expect_success(
        &env.run_as(
            &session,
            &[
                "deliver",
                "--node",
                "core/wake",
                "--attempt",
                &attempt,
                "--receipt",
                &answer,
                "--json",
                &input,
            ],
        ),
        "deliver",
    );
    let delivery = agendar_core::model::EventId::parse(
        delivered["data"]["event_id"].as_str().expect("delivery"),
    )
    .expect("delivery id");
    env.service()
        .review_delivery(
            &agendar_core::model::ProjectSlug::parse("demo").expect("project"),
            &agendar_core::model::NodePath::parse("core/wake").expect("node"),
            &delivery,
            agendar_core::model::ReviewVerdict::SendBack,
            Some("The transcript stops before the result".to_owned()),
            None,
            {
                use agendar_core::model::{DiffSide, ReviewAnchor, ReviewComment};
                vec![
                    ReviewComment {
                        anchor: ReviewAnchor::DiffLine {
                            demo: 0,
                            file: "crates/x.rs".to_owned(),
                            side: DiffSide::New,
                            line: 840,
                            quote: "  let wake = 1;".to_owned(),
                        },
                        text: "Name the constant".to_owned(),
                    },
                    ReviewComment {
                        anchor: ReviewAnchor::TranscriptLine {
                            demo: 1,
                            line: 12,
                            quote: "waiting…".to_owned(),
                        },
                        text: "Never finishes".to_owned(),
                    },
                    ReviewComment {
                        anchor: ReviewAnchor::Region {
                            demo: 0,
                            x: 100,
                            y: 50,
                            w: 300,
                            h: 150,
                        },
                        text: "Clipped".to_owned(),
                    },
                ]
            },
            vec![agendar_core::model::EventId::parse(&answer).expect("answer")],
        )
        .expect("send back");

    let value = single_json(&run_hook(&env, "post-tool-use", "harness-a", None, None));
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(
        context.contains(
            "review on core/wake: the human sent your delivery back: The transcript stops before the result"
        ),
        "{context}"
    );
    assert!(context.contains("deliver again"), "{context}");
    let redeliver = format!(
        "\n  Redeliver with --supersedes {} and replies to the reason and to comments by number in \"replies\"",
        delivery.as_str()
    );
    for line in [
        "\n  1. crates/x.rs:840 (new) «let wake = 1;»: Name the constant",
        "\n  2. demo 2 transcript, line 12 «waiting…»: Never finishes",
        "\n  3. demo 1 screenshot, area x 10–40% y 5–20%: Clipped",
        redeliver.as_str(),
    ] {
        assert!(context.contains(line), "missing {line:?} in {context}");
    }
    // Shown once: the next tool call has nothing new.
    assert_no_stdout(&run_hook(&env, "post-tool-use", "harness-a", None, None));
    // If that one text was lost, the send-back is not: it has no ack, so a
    // prompt does not repeat it, but the Stop of every turn holds for it
    // until the rework starts, whatever was recorded as shown.
    expect_success(&env.run_as(&session, &["ack", &answer]), "ack");
    assert_no_stdout(&run_hook(
        &env,
        "user-prompt-submit",
        "harness-a",
        None,
        None,
    ));
    for _ in 0..2 {
        let held = single_json(&run_hook(
            &env,
            "stop",
            "harness-a",
            Some(false),
            Some(2000),
        ));
        assert_eq!(held["decision"], "block");
        let reason = held["reason"].as_str().expect("reason");
        assert!(
            reason.contains("The transcript stops before the result"),
            "{reason}"
        );
    }
}

#[test]
fn stop_passes_a_step_with_a_future_eta_or_in_review_and_holds_once_past_its_eta() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (session, _) = start(&env, "harness-a");
    declare_board(&env, &session);
    // Held inside its ETA: nothing owed, the turn may end.
    let started = expect_success(
        &env.run_as(
            &session,
            &[
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "1s..2s",
            ],
        ),
        "progress",
    );
    let attempt = started["data"]["attempt"].as_str().unwrap().to_owned();
    assert_no_stdout(&run_hook(
        &env,
        "stop",
        "harness-a",
        Some(false),
        Some(2000),
    ));
    // Past its ETA with no word: held once, with the way out.
    std::thread::sleep(Duration::from_millis(3600));
    let held = single_json(&run_hook(
        &env,
        "stop",
        "harness-a",
        Some(false),
        Some(2000),
    ));
    assert_eq!(held["decision"], "block");
    let reason = held["reason"].as_str().expect("reason");
    assert!(reason.contains("core/wake is"), "reason={reason}");
    assert!(reason.contains("settle"), "reason={reason}");
    assert_no_stdout(&run_hook(&env, "stop", "harness-a", Some(true), Some(2000)));
    // Delivered: in review, the owner holds it; Stop passes though the ETA
    // is long gone.
    let demo = env.repo.join("demo.txt");
    fs::write(&demo, b"demo transcript\n").expect("demo");
    let delivery = write_json(
        &env,
        "delivery.json",
        &json!({
            "name": "Wake",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo.to_string_lossy(), "ext": "txt"}}],
            "disclosure": {"proven": [], "not_proven": [], "faked": []}
        }),
    );
    expect_success(
        &env.run_as(
            &session,
            &[
                "deliver",
                "--node",
                "core/wake",
                "--attempt",
                &attempt,
                "--json",
                &delivery,
            ],
        ),
        "deliver",
    );
    assert_no_stdout(&run_hook(
        &env,
        "stop",
        "harness-a",
        Some(false),
        Some(2000),
    ));
}
