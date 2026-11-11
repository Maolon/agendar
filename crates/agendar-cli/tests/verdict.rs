//! A review verdict wakes the session that delivered, like an answer: the
//! Claude Code rewake wakes it once, a send-back holds Stop until the agent
//! acts, and `wait --delivery` returns the verdict for harnesses without hooks.

mod support;

use std::fs;
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use agendar_core::model::{
    DiffSide, EventId, NodePath, ProjectSlug, ReviewAnchor, ReviewComment, ReviewVerdict,
};
use support::{CliEnv, expect_success};

fn hook(env: &CliEnv, action: &str, harness_session: &str, active: Option<bool>) -> Output {
    let mut child = hook_command(env, action, None).spawn().expect("spawn hook");
    feed(&mut child, env, harness_session, action, active);
    child.wait_with_output().expect("hook")
}

fn hook_command(env: &CliEnv, action: &str, rewake_ms: Option<u64>) -> Command {
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("hook")
        .arg(action)
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .env("AGENDAR_HOOK_BUDGET_MS", "3000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(ms) = rewake_ms {
        command.env("AGENDAR_REWAKE_MS", ms.to_string());
    }
    command
}

fn feed(
    child: &mut Child,
    env: &CliEnv,
    harness_session: &str,
    action: &str,
    active: Option<bool>,
) {
    let input = json!({
        "session_id": harness_session,
        "cwd": env.repo,
        "hook_event_name": if action == "session-start" { "SessionStart" } else { "Stop" },
        "stop_hook_active": active,
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(serde_json::to_vec(&input).expect("input").as_slice())
        .expect("write stdin");
}

fn rewake(env: &CliEnv, harness_session: &str, ms: u64) -> Child {
    let mut child = hook_command(env, "rewake", Some(ms))
        .spawn()
        .expect("spawn rewake");
    feed(&mut child, env, harness_session, "rewake", Some(true));
    child
}

/// SessionStart registers the harness session; returns its Agendar id.
fn start(env: &CliEnv, harness_session: &str) -> String {
    let output = hook(env, "session-start", harness_session, None);
    let value: Value = serde_json::from_slice(&output.stdout).expect("session start json");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    let marker = "Agendar session ";
    let at = context.find(marker).expect("marker") + marker.len();
    context[at..].split_once('.').expect("id").0.to_owned()
}

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("json")).expect("write");
    path.to_string_lossy().into_owned()
}

fn declare(env: &CliEnv, session: &str, nodes: &[&str]) {
    let path = write_json(
        env,
        "project.json",
        &json!({"title": "Verdict", "brief": "Verdicts wake agents.", "goal": "No polling.", "glyph": "radar"}),
    );
    expect_success(
        &env.run_as(session, &["project", "declare", "--json", &path]),
        "project declare",
    );
    for node in nodes {
        let path = write_json(
            env,
            "feature.json",
            &json!({"title": node, "slice": "core", "demo_kind": "cli_transcript"}),
        );
        expect_success(
            &env.run_as(
                session,
                &["feature", "declare", "--node", node, "--json", &path],
            ),
            "feature declare",
        );
    }
}

/// Start the step and deliver it; returns the delivery id.
fn deliver(env: &CliEnv, session: &str, node: &str) -> String {
    let started = expect_success(
        &env.run_as(
            session,
            &["progress", "started", "--node", node, "--eta", "10m..20m"],
        ),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let demo = env.repo.join("demo.txt");
    fs::write(&demo, b"$ wake\nwoken\n").expect("demo");
    let input = write_json(
        env,
        "delivery.json",
        &json!({
            "name": "Wake",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo, "ext": "txt"}}],
            "disclosure": {"proven": ["Ran it"], "not_proven": [], "faked": []}
        }),
    );
    let delivered = expect_success(
        &env.run_as(
            session,
            &[
                "deliver",
                "--node",
                node,
                "--attempt",
                &attempt,
                "--json",
                &input,
            ],
        ),
        "deliver",
    );
    delivered["data"]["event_id"]
        .as_str()
        .expect("delivery")
        .to_owned()
}

/// The operator's review, through the service (the CLI has no review command;
/// the app calls the same service method).
fn review(env: &CliEnv, node: &str, delivery: &str, verdict: ReviewVerdict) {
    let comments = if verdict == ReviewVerdict::SendBack {
        vec![ReviewComment {
            anchor: ReviewAnchor::DiffLine {
                demo: 0,
                file: "crates/wake.rs".to_owned(),
                side: DiffSide::New,
                line: 12,
                quote: "let wake = 1;".to_owned(),
            },
            text: "Name the constant".to_owned(),
        }]
    } else {
        Vec::new()
    };
    let reason =
        (verdict == ReviewVerdict::SendBack).then(|| "The transcript stops early".to_owned());
    env.service()
        .review_delivery(
            &ProjectSlug::parse(&env.project).expect("project"),
            &NodePath::parse(node).expect("node"),
            &EventId::parse(delivery).expect("delivery"),
            verdict,
            reason,
            None,
            comments,
            Vec::new(),
        )
        .expect("review");
}

fn stop_reason(output: &Output) -> Option<String> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).expect("stop json");
    assert_eq!(value["decision"], "block");
    Some(value["reason"].as_str().expect("reason").to_owned())
}

fn quiet(child: Child) {
    let output = child.wait_with_output().expect("rewake");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_accept_wakes_the_idle_session_once_and_never_holds_a_stop() {
    let mut env = CliEnv::new("demo");
    env.init();
    let session = start(&env, "harness-a");
    declare(&env, &session, &["core/wake"]);
    let delivery = deliver(&env, &session, "core/wake");
    // The turn ends with the delivery in review: Stop passes, rewake waits.
    assert_eq!(
        stop_reason(&hook(&env, "stop", "harness-a", Some(false))),
        None
    );
    let child = rewake(&env, "harness-a", 15_000);
    std::thread::sleep(Duration::from_millis(1500));
    review(&env, "core/wake", &delivery, ReviewVerdict::Accept);
    let output = child.wait_with_output().expect("rewake");
    assert_eq!(output.status.code(), Some(2), "exit 2 wakes Claude Code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reviewed your delivery"), "{stderr}");
    assert!(stderr.contains("review on core/wake: accepted"), "{stderr}");
    assert!(stderr.contains("nothing to do"), "{stderr}");
    // The woken turn ends: an accept never holds, and never wakes again.
    assert_eq!(
        stop_reason(&hook(&env, "stop", "harness-a", Some(false))),
        None
    );
    quiet(rewake(&env, "harness-a", 3_000));
}

#[test]
fn a_send_back_wakes_with_the_comment_and_holds_until_the_agent_acts() {
    let mut env = CliEnv::new("demo");
    env.init();
    let session = start(&env, "harness-a");
    declare(&env, &session, &["core/wake"]);
    let delivery = deliver(&env, &session, "core/wake");
    let child = rewake(&env, "harness-a", 15_000);
    std::thread::sleep(Duration::from_millis(1500));
    review(&env, "core/wake", &delivery, ReviewVerdict::SendBack);
    let output = child.wait_with_output().expect("rewake");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sent your delivery back: The transcript stops early"),
        "{stderr}"
    );
    assert!(
        stderr.contains("1. crates/wake.rs:12 (new) «let wake = 1;»: Name the constant"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("--supersedes {delivery}")),
        "{stderr}"
    );
    assert!(stderr.contains("Start the rework"), "{stderr}");

    // Until it acts, every turn's first Stop is held (once), like runnable work.
    for _ in 0..2 {
        let reason = stop_reason(&hook(&env, "stop", "harness-a", Some(false))).expect("held");
        assert!(reason.contains("Name the constant"), "{reason}");
        assert_eq!(
            stop_reason(&hook(&env, "stop", "harness-a", Some(true))),
            None
        );
    }
    // Seen once: the next idle period does not wake for it again.
    quiet(rewake(&env, "harness-a", 3_000));

    // Starting the rework releases the hold.
    expect_success(
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
    assert_eq!(
        stop_reason(&hook(&env, "stop", "harness-a", Some(false))),
        None
    );
}

#[test]
fn a_verdict_on_another_sessions_delivery_wakes_nobody_else() {
    let mut env = CliEnv::new("demo");
    env.init();
    let mine = start(&env, "harness-a");
    let other = start(&env, "harness-b");
    assert_ne!(mine, other);
    declare(&env, &mine, &["core/wake", "core/other"]);
    let delivery = deliver(&env, &mine, "core/wake");
    // The other session waits on its own delivery.
    let _own = deliver(&env, &other, "core/other");
    let bystander = rewake(&env, "harness-b", 4_000);
    let owner = rewake(&env, "harness-a", 15_000);
    std::thread::sleep(Duration::from_millis(1500));
    review(&env, "core/wake", &delivery, ReviewVerdict::SendBack);
    let woken = owner.wait_with_output().expect("owner rewake");
    assert_eq!(woken.status.code(), Some(2));
    quiet(bystander);
    assert_eq!(
        stop_reason(&hook(&env, "stop", "harness-b", Some(false))),
        None
    );
}

/// `deliver` → the operator reviews → `inbox` shows the packet → `wait
/// --delivery` returns it. Set AGENDAR_DEMO_OUT to save the transcript.
#[test]
fn wait_for_a_delivery_returns_its_verdict_cli_transcript() {
    let mut env = CliEnv::new("demo");
    env.init();
    let session = env.session.clone();
    declare(&env, &session, &["core/wake"]);
    let mut transcript = String::new();
    let log = |transcript: &mut String, args: &[&str], output: &Output| {
        transcript.push_str(&format!("$ agendar {}\n", args.join(" ")));
        transcript.push_str(&String::from_utf8_lossy(&output.stdout));
        transcript.push_str(&String::from_utf8_lossy(&output.stderr));
        transcript.push_str(&format!(
            "[exit {}]\n\n",
            output.status.code().unwrap_or(-1)
        ));
    };
    let delivery = deliver(&env, &session, "core/wake");
    transcript.push_str(&format!("# delivered core/wake as {delivery}\n\n"));

    let args = ["wait", "--delivery", &delivery, "--timeout", "1s"];
    let pending = env.run_as(&session, &args);
    log(&mut transcript, &args, &pending);
    assert_ne!(pending.status.code(), Some(0), "no verdict yet: timeout");

    // A blocked wait returns as soon as the operator sends it back.
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg(&env.project)
        .arg("--session")
        .arg(&session)
        .args(["wait", "--delivery", &delivery, "--timeout", "30s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let blocked = command.spawn().expect("spawn wait");
    std::thread::sleep(Duration::from_millis(1000));
    transcript.push_str(
        "# operator (app → Service::review_delivery): send back with one diff comment\n\n",
    );
    review(&env, "core/wake", &delivery, ReviewVerdict::SendBack);
    let waited = blocked.wait_with_output().expect("wait");
    log(
        &mut transcript,
        &["wait", "--delivery", &delivery, "--timeout", "30s"],
        &waited,
    );
    let value: Value = serde_json::from_slice(&waited.stdout).expect("wait json");
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["data"]["delivery_id"], delivery.as_str());
    assert_eq!(value["data"]["packet"]["kind"], "review");
    assert_eq!(value["data"]["packet"]["review"]["verdict"], "send_back");
    assert_eq!(
        value["data"]["packet"]["review"]["comments"][0]["text"],
        "Name the constant"
    );

    let args = ["inbox"];
    let inbox = env.run_as(&session, &args);
    log(&mut transcript, &args, &inbox);
    let value: Value = serde_json::from_slice(&inbox.stdout).expect("inbox json");
    let packets = value["data"]["packets"].as_array().expect("packets");
    assert!(
        packets
            .iter()
            .any(|packet| packet["kind"] == "review" && packet["delivery"] == delivery.as_str()),
        "{value}"
    );
    if let Ok(path) = std::env::var("AGENDAR_DEMO_OUT") {
        fs::write(path, &transcript).expect("save transcript");
    }
}
