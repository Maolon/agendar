//! An owner's answer reaches a working agent by itself, and keeps showing
//! until the agent acks it.
//!
//! Observed: the owner answered while the agent was in the middle of a turn.
//! A PostToolUse hook recorded the packet as delivered, the text never
//! reached the agent, and every later hook stayed silent because the packet
//! counted as seen. A sighting is not an ack: the Stop hook holds for the
//! answer, and each UserPromptSubmit names it, until `ack`.
//!
//! Set `AGENDAR_UNACKED_TRANSCRIPT` to a file path to save the transcript of
//! the incident test.

mod support;

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::process::{Child, Output};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use agendar_core::model::{EventId, ProjectSlug};
use support::{CliEnv, expect_success};

const HARNESS: &str = "harness-a";
const NODE: &str = "core/wake";

fn hook_command(env: &CliEnv, action: &str, budget_ms: u64) -> std::process::Command {
    let mut command = env.command();
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .args(["hook", action])
        .env("AGENDAR_HOOK_PID", std::process::id().to_string())
        .env("AGENDAR_HOOK_BUDGET_MS", budget_ms.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

fn spawn_hook(env: &CliEnv, action: &str, budget_ms: u64, input: &Value) -> Child {
    let mut child = hook_command(env, action, budget_ms)
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(&serde_json::to_vec(input).expect("hook input"))
        .expect("write hook stdin");
    child
}

/// Run one hook entry point with the stdin Claude Code sends for it.
fn hook(env: &CliEnv, action: &str) -> Output {
    hook_within(env, action, 2_000)
}

fn hook_within(env: &CliEnv, action: &str, budget_ms: u64) -> Output {
    hook_as(env, action, budget_ms, None)
}

/// `agent`: the subagent the hook fires in (`agent_id` in the hook input).
fn hook_as(env: &CliEnv, action: &str, budget_ms: u64, agent: Option<&str>) -> Output {
    let event = match action {
        "session-start" => "SessionStart",
        "post-tool-use" => "PostToolUse",
        "user-prompt-submit" => "UserPromptSubmit",
        _ => "Stop",
    };
    let mut input = json!({
        "session_id": HARNESS,
        "cwd": env.repo,
        "hook_event_name": event,
    });
    match action {
        "session-start" => input["source"] = json!("startup"),
        "post-tool-use" => {
            input["tool_name"] = json!("Bash");
            input["tool_input"] = json!({"command": "cargo test"});
            input["tool_response"] = json!({"stdout": "ok"});
        }
        "user-prompt-submit" => input["prompt"] = json!("anything new?"),
        _ => {
            input["stop_hook_active"] = json!(false);
            input["last_assistant_message"] = json!("Still working through the list.");
        }
    }
    if let Some(agent) = agent {
        input["agent_id"] = json!(agent);
        input["agent_type"] = json!("general-purpose");
    }
    spawn_hook(env, action, budget_ms, &input)
        .wait_with_output()
        .expect("wait hook")
}

fn stdout(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The text a PostToolUse, UserPromptSubmit or SessionStart hook adds to the
/// agent's context: one JSON line, `hookSpecificOutput.additionalContext`,
/// under the event's own name.
fn context(output: &Output, event: &str) -> String {
    let text = stdout(output);
    assert_eq!(text.lines().count(), 1, "one JSON line: {text}");
    let value: Value = serde_json::from_str(&text).expect("hook JSON");
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], event);
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_owned()
}

/// The reason a Stop hook holds the turn with.
fn held(output: &Output) -> String {
    let value: Value = serde_json::from_str(&stdout(output)).expect("Stop JSON");
    assert_eq!(value["decision"], "block", "{value}");
    value["reason"].as_str().expect("reason").to_owned()
}

fn silent(output: &Output) {
    assert_eq!(stdout(output), "", "expected no hook output");
}

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("json")).expect("write");
    path.to_string_lossy().into_owned()
}

/// A Claude Code session registered by its SessionStart hook, with a board
/// that has one step.
fn conversation(env: &mut CliEnv) -> String {
    env.init();
    let text = context(&hook(env, "session-start"), "SessionStart");
    let marker = "Agendar session ";
    let from = text.find(marker).expect("session marker") + marker.len();
    let session = text[from..].split_once('.').expect("end").0.to_owned();
    let project = write_json(
        env,
        "project.json",
        &json!({"title": "Unacked", "brief": "Scratch.", "goal": "Hooks.", "glyph": "radar"}),
    );
    expect_success(
        &env.run_as(&session, &["project", "declare", "--json", &project]),
        "project declare",
    );
    let feature = write_json(
        env,
        "feature.json",
        &json!({"title": "Wake", "slice": NODE, "demo_kind": "cli_transcript"}),
    );
    expect_success(
        &env.run_as(
            &session,
            &["feature", "declare", "--node", NODE, "--json", &feature],
        ),
        "feature declare",
    );
    session
}

fn ask(env: &CliEnv, session: &str) -> String {
    let path = CliEnv::fixture("ask.json");
    let value = expect_success(
        &env.run_as(
            session,
            &[
                "ask",
                "--node",
                NODE,
                "--mode",
                "ask",
                "--json",
                path.to_str().expect("fixture path"),
            ],
        ),
        "ask",
    );
    value["data"]["ask_id"].as_str().expect("ask id").to_owned()
}

fn ack(env: &CliEnv, session: &str, answer: &str) {
    expect_success(&env.run_as(session, &["ack", answer]), "ack");
}

fn vault_local_dir(env: &CliEnv) -> std::path::PathBuf {
    fs::read_dir(env.data.join("vaults"))
        .expect("vaults")
        .flatten()
        .next()
        .expect("one vault")
        .path()
}

/// The local delivery attempts that name `session`.
fn sightings(env: &CliEnv, session: &str) -> Vec<Value> {
    let text = fs::read_to_string(vault_local_dir(env).join("runtime.json")).expect("runtime");
    let runtime: Value = serde_json::from_str(&text).expect("runtime JSON");
    runtime["delivery_attempts"]
        .as_object()
        .expect("delivery_attempts")
        .iter()
        .filter(|(key, _)| key.ends_with(&format!(":{session}")))
        .map(|(_, attempt)| attempt.clone())
        .collect()
}

fn spawn_rewake(env: &CliEnv, wait_ms: u64) -> Child {
    let mut command = hook_command(env, "rewake", 2_000);
    command.env("AGENDAR_REWAKE_MS", wait_ms.to_string());
    let mut child = command.spawn().expect("spawn rewake");
    let input = json!({
        "session_id": HARNESS,
        "cwd": env.repo,
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&serde_json::to_vec(&input).expect("input"))
        .expect("write stdin");
    child
}

struct Transcript(String);

impl Transcript {
    fn note(&mut self, text: &str) {
        let _ = writeln!(self.0, "# {text}\n");
    }

    fn hook(&mut self, env: &CliEnv, action: &str) -> Output {
        let output = hook(env, action);
        let shown = String::from_utf8_lossy(&output.stdout);
        let shown = shown.trim();
        let _ = writeln!(
            self.0,
            "$ agendar hook {action}   (stdin: the harness JSON)\n{}\n",
            if shown.is_empty() {
                "(no output)"
            } else {
                shown
            }
        );
        output
    }

    fn save(&self) {
        if let Some(path) = std::env::var_os("AGENDAR_UNACKED_TRANSCRIPT") {
            // The scratch paths differ on every run; the transcript names them.
            fs::write(path, &self.0).expect("write transcript");
        }
    }
}

// Working mid-turn, about to stop, next prompt: the incident.

#[test]
fn an_answer_whose_post_tool_use_text_was_lost_still_reaches_the_agent() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let mut log = Transcript(String::new());
    log.note("The agent asked a question and went on working.");
    let asked = ask(&env, &session);
    silent(&log.hook(&env, "post-tool-use"));

    log.note("The owner answers while the agent is in the middle of a turn.");
    let answer = env.answer(&asked, "adapters");
    let _ = writeln!(
        log.0,
        "(owner) answer {asked} -> adapters   answer_id {answer}\n"
    );

    log.note("The next tool call's PostToolUse hook carries it. Suppose this text is lost.");
    let first = context(&log.hook(&env, "post-tool-use"), "PostToolUse");
    assert!(
        first.contains(&format!("answer on {NODE}: chose Use harness adapters")),
        "{first}"
    );
    assert!(first.contains(&format!("answer id {answer}")), "{first}");
    let seen = sightings(&env, &session);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["outcome"], "accepted");
    assert_eq!(
        seen[0]["via"], "PostToolUse",
        "the attempt names the hook that wrote the text"
    );

    log.note("PostToolUse shows an answer once: the rest of the turn is not flooded.");
    silent(&log.hook(&env, "post-tool-use"));

    log.note("Before the turn ends, the Stop hook holds for the answer: no chat message needed.");
    let reason = held(&log.hook(&env, "stop"));
    assert!(reason.contains(&format!("ack {answer}")), "{reason}");
    assert!(
        reason.contains(&format!("- {NODE}: chose Use harness adapters")),
        "the hold says what was answered: {reason}"
    );

    log.note("It holds once per answer. If the agent stops anyway, the answer is still not acked:");
    log.note("every prompt names it, one line, whatever was recorded as shown.");
    silent(&log.hook(&env, "stop"));
    for _ in 0..2 {
        let again = context(&log.hook(&env, "user-prompt-submit"), "UserPromptSubmit");
        assert!(
            again.contains(&format!(
                "- {NODE}: chose Use harness adapters — `ack {answer}`"
            )),
            "{again}"
        );
        assert!(again.contains("1 answer(s) from the human not acked yet"));
        assert!(
            !again.contains("acceptance:"),
            "the repeated form is the short one: {again}"
        );
        assert_eq!(
            again.trim().lines().count(),
            2,
            "a header and one line per answer: {again}"
        );
        // Inside the turn that follows, PostToolUse stays quiet about it.
        silent(&hook(&env, "post-tool-use"));
    }

    log.note("Reading the inbox is a sighting too, not an ack.");
    expect_success(&env.run_as(&session, &["inbox"]), "inbox");
    let after_inbox = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(
        after_inbox.contains(&format!("ack {answer}")),
        "{after_inbox}"
    );

    log.note("The agent applies the answer and acks it: every hook is silent again.");
    ack(&env, &session, &answer);
    let _ = writeln!(log.0, "$ agendar --session <agent> ack {answer}\n");
    silent(&log.hook(&env, "user-prompt-submit"));
    silent(&log.hook(&env, "post-tool-use"));
    silent(&log.hook(&env, "stop"));
    assert_eq!(env.count_kind("decision.ack"), 1, "only the agent acks");
    log.save();
}

#[test]
fn an_answer_first_seen_at_a_prompt_is_shown_in_full_and_not_twice() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let answer = env.answer(&asked, "adapters");
    let first = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(first.contains("acceptance:"), "the full form: {first}");
    assert!(
        !first.contains("not acked yet"),
        "not also the short form: {first}"
    );
    assert_eq!(sightings(&env, &session)[0]["via"], "UserPromptSubmit");
    let second = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(second.contains("not acked yet"), "{second}");
    assert!(second.contains(&format!("ack {answer}")), "{second}");
}

// Working mid-turn, with subagents: their tool calls run the same hook.

#[test]
fn a_subagents_tool_call_neither_gets_the_answer_nor_records_a_sighting() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let answer = env.answer(&asked, "adapters");
    // A subagent's tool call fires PostToolUse first: the text would
    // land in the subagent's context, not the agent's.
    silent(&hook_as(&env, "post-tool-use", 2_000, Some("agent-7f3")));
    assert!(sightings(&env, &session).is_empty(), "nothing recorded");
    // The agent's own next tool call carries the answer.
    let carried = context(&hook(&env, "post-tool-use"), "PostToolUse");
    assert!(
        carried.contains(&format!("answer id {answer}")),
        "{carried}"
    );
    assert_eq!(sightings(&env, &session)[0]["via"], "PostToolUse");
}

// Idle, and a rewake still alive while the session is busy again.

#[test]
fn a_rewake_that_fires_while_the_session_is_busy_does_not_consume_the_answer() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    // The turn ended with the question open: its rewake waits. The session is
    // then woken by something that runs no UserPromptSubmit hook (a background
    // task ending), so the rewake is still alive while the agent works.
    let rewake = spawn_rewake(&env, 20_000);
    std::thread::sleep(Duration::from_millis(1_000));
    let answer = env.answer(&asked, "adapters");
    let woke = rewake.wait_with_output().expect("rewake");
    assert_eq!(woke.status.code(), Some(2), "it fires");
    let said = String::from_utf8_lossy(&woke.stderr).into_owned();
    assert!(said.contains(&format!("ack {answer}")), "{said}");
    // Suppose the busy session never got that text. The rewake recorded no
    // sighting, so the turn's next PostToolUse carries the answer in full.
    assert!(sightings(&env, &session).is_empty(), "nothing consumed");
    let carried = context(&hook(&env, "post-tool-use"), "PostToolUse");
    assert!(
        carried.contains(&format!("answer on {NODE}: chose Use harness adapters")),
        "{carried}"
    );
    // And if that is lost as well, the Stop of that turn holds for it.
    let reason = held(&hook(&env, "stop"));
    assert!(reason.contains(&format!("ack {answer}")), "{reason}");
    // After that, each prompt, until the ack.
    let prompt = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(prompt.contains(&format!("ack {answer}")), "{prompt}");
    ack(&env, &session, &answer);
    silent(&hook(&env, "user-prompt-submit"));
    silent(&hook(&env, "stop"));
}

#[test]
fn a_new_prompt_retires_the_rewake_of_the_turn_before() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let rewake = spawn_rewake(&env, 20_000);
    std::thread::sleep(Duration::from_millis(1_000));
    // The owner types a prompt: the session is busy, nothing to wake.
    silent(&hook(&env, "user-prompt-submit"));
    let started = Instant::now();
    let retired = rewake.wait_with_output().expect("rewake");
    assert_eq!(retired.status.code(), Some(0), "silent, no wake");
    assert!(retired.stderr.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "it ends at its next poll, not at its timeout"
    );
    // The answer that arrives in that turn comes through PostToolUse.
    let answer = env.answer(&asked, "adapters");
    let carried = context(&hook(&env, "post-tool-use"), "PostToolUse");
    assert!(
        carried.contains(&format!("answer id {answer}")),
        "{carried}"
    );
    assert_eq!(sightings(&env, &session)[0]["via"], "PostToolUse");
}

#[test]
fn an_idle_session_is_woken_and_the_answer_stays_owed_until_the_ack() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let rewake = spawn_rewake(&env, 20_000);
    std::thread::sleep(Duration::from_millis(1_000));
    let answer = env.answer(&asked, "adapters");
    let woke = rewake.wait_with_output().expect("rewake");
    assert_eq!(woke.status.code(), Some(2), "exit 2 wakes Claude Code");
    let said = String::from_utf8_lossy(&woke.stderr).into_owned();
    assert!(said.contains("answered your question"), "{said}");
    assert!(
        said.contains(&format!("answer on {NODE}: chose Use harness adapters")),
        "{said}"
    );
    // The woken turn does not ack: its Stop holds once.
    let reason = held(&hook(&env, "stop"));
    assert!(reason.contains(&format!("ack {answer}")), "{reason}");
    silent(&hook(&env, "stop"));
}

// About to stop: a step the session holds, a question it did not ask.

#[test]
fn stop_holds_once_for_an_unacked_answer_on_a_step_the_session_holds_but_did_not_ask_about() {
    let mut env = CliEnv::new("demo");
    let holder = conversation(&mut env);
    // `env.session` is another agent (registered by `init`).
    let other = env.session.clone();
    assert_ne!(holder, other);
    expect_success(
        &env.run_as(
            &holder,
            &["progress", "started", "--node", NODE, "--eta", "30m..60m"],
        ),
        "progress",
    );
    // The other session asks the owner about the held step.
    let asked = expect_success(
        &env.run_as(
            &other,
            &["ask", "--node", NODE, "--takeover", "Read its start note."],
        ),
        "ask",
    );
    let asked = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    // Nothing is answered yet: the holder stops freely (it asked nothing).
    silent(&hook(&env, "stop"));
    let answer = env.answer(&asked, "leave");

    let reason = held(&hook(&env, "stop"));
    assert!(reason.contains("New answer available"), "{reason}");
    assert!(reason.contains(&format!("ack {answer}")), "{reason}");
    assert!(reason.contains(&format!("- {NODE}: chose ")), "{reason}");
    // Once per answer.
    silent(&hook(&env, "stop"));
    // The answer was never shown by a PostToolUse or a prompt hook before:
    // the first sighting is the full form, then one line per prompt.
    let first = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(first.contains(&format!("answer id {answer}")), "{first}");
    let again = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(again.contains(&format!("`ack {answer}`")), "{again}");

    ack(&env, &holder, &answer);
    silent(&hook(&env, "user-prompt-submit"));
    silent(&hook(&env, "stop"));
}

#[test]
fn an_answer_on_a_step_nobody_here_holds_or_asked_about_is_shown_once_and_never_holds() {
    let mut env = CliEnv::new("demo");
    let bystander = conversation(&mut env);
    let asker = env.session.clone();
    let asked = ask(&env, &asker);
    let answer = env.answer(&asked, "adapters");
    // Until someone applies it, any session may pick it up: one sighting.
    let first = context(&hook(&env, "post-tool-use"), "PostToolUse");
    assert!(first.contains(&format!("answer id {answer}")), "{first}");
    assert_eq!(sightings(&env, &bystander).len(), 1);
    // It is not this session's to ack: no hold, no repetition.
    silent(&hook(&env, "stop"));
    silent(&hook(&env, "user-prompt-submit"));
}

// Own-words answers.

#[test]
fn an_own_words_answer_shows_its_text_and_the_repeated_line_its_first_80_characters() {
    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let words = "Neither. Keep the hooks, but make the Stop hook hold for every answer \
                 the session has not acked, and write down which hook showed it.";
    let answer = env
        .service()
        .answer_own(
            &ProjectSlug::parse(&env.project).expect("project"),
            &EventId::parse(&asked).expect("ask"),
            words.to_owned(),
            None,
            None,
        )
        .expect("own-words answer")
        .as_str()
        .to_owned();
    let full = context(&hook(&env, "post-tool-use"), "PostToolUse");
    assert!(
        full.contains(&format!(
            "the human answered in their own words: {}",
            words.split_whitespace().collect::<Vec<_>>().join(" ")
        )) || full.contains("the human answered in their own words: Neither. Keep the hooks"),
        "{full}"
    );
    let reason = held(&hook(&env, "stop"));
    let short: String = words
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    let line = format!("- {NODE}: in their own words: {short}… — `ack {answer}`");
    assert!(reason.contains(&line), "{reason}");
    let again = context(&hook(&env, "user-prompt-submit"), "UserPromptSubmit");
    assert!(again.contains(&line), "{again}");
    assert!(
        !again.contains("write down which hook"),
        "only the first 80 characters: {again}"
    );
}

// A busy lock: nothing printed means nothing recorded.

#[test]
fn a_hook_that_cannot_read_or_record_under_a_held_lock_never_records_a_sighting_it_did_not_print() {
    use fs2::FileExt as _;

    let mut env = CliEnv::new("demo");
    let session = conversation(&mut env);
    let asked = ask(&env, &session);
    let answer = env.answer(&asked, "adapters");
    let locks = vault_local_dir(&env).join("locks");
    let mut names: Vec<String> = fs::read_dir(&locks)
        .expect("locks")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.iter().any(|name| name == "runtime.lock"), "{names:?}");
    let mut printed_once = false;
    for name in &names {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(locks.join(name))
            .expect("lock file");
        lock.lock_exclusive().expect("hold the lock");
        let started = Instant::now();
        let output = hook_within(&env, "post-tool-use", 1_500);
        let elapsed = started.elapsed();
        fs2::FileExt::unlock(&lock).expect("unlock");
        drop(lock);
        assert!(
            elapsed < Duration::from_secs(9),
            "{name}: the hook stays inside its installed 10 s timeout: {elapsed:?}"
        );
        let printed = String::from_utf8_lossy(&output.stdout).contains(&answer);
        printed_once |= printed;
        let recorded = !sightings(&env, &session).is_empty();
        assert!(
            printed_once || !recorded,
            "{name}: a sighting was recorded for text that was never printed"
        );
        if !printed {
            assert_eq!(
                output.status.code(),
                Some(0),
                "{name}: a busy lock is not a hook error: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty(), "{name}: nothing half-printed");
        }
    }
    // Whatever the locks did, the answer still reaches the agent: the next
    // PostToolUse (if no sighting was recorded) or the Stop of the turn.
    let later = String::from_utf8_lossy(&hook(&env, "post-tool-use").stdout).contains(&answer);
    assert!(
        printed_once || later,
        "a PostToolUse shows the answer once the locks are free"
    );
    let reason = held(&hook(&env, "stop"));
    assert!(reason.contains(&format!("ack {answer}")), "{reason}");
}
