//! `agendar mcp`: a stdio MCP server, the one adapter every MCP-capable harness
//! can load (pi, opencode, dsh, Cursor, Antigravity, Gemini CLI, and Claude
//! Code or Codex when hooks are not wanted).
//!
//! Each tool runs this same executable with the matching CLI command, so the
//! MCP path and the CLI path cannot drift apart: validation, request ids,
//! limits and errors are the CLI's own. The server registers one Agendar
//! session lazily, on the first tool call, for the harness named by
//! `--harness`; the harness's own session id comes from its environment when it
//! exposes one, otherwise from the server's parent process.
//!
//! MCP is pull only: reminders arrive when the agent calls a tool (`inbox`,
//! `board`), and an answer arrives through a bounded `wait`. Harnesses with
//! Claude-compatible hooks (Claude Code, Codex) can use hooks instead.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use agendar_core::model::Harness;

/// Protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Longest `wait` a tool call may block, so a harness's tool timeout is not hit.
const MAX_WAIT: &str = "30m";
const DEFAULT_WAIT: &str = "5m";

const INSTRUCTIONS: &str = "Agendar is the human's radar for this project. Keep it true while you work:\n\
1. `board` first: see the project, your steps and what you may start.\n\
2. Declare every step of your plan (`declare_step`), then `start` a step with an honest ETA range before working on it.\n\
3. While working, `pulse` at real milestones (new --eta when your estimate changes); `done` when a step is finished, `drop` when you abandon it.\n\
4. A decision only the human can make: `ask` with a card (shapes and limits: `schema` kind card), then `wait` for the answer; apply the receipt and `ack` it. If the receipt's chose.id is `own`, follow own_answer.\n\
5. Call `inbox` after long work: it carries answers and reminders (overdue steps, plan changes).\n\
6. Before you stop, call `settle`: every step you hold is done, dropped, delivered or has a future ETA.\n\
Never guess a human decision. Never invent progress.";

/// Run the server on stdin/stdout until stdin closes. Returns the exit code.
pub fn run(harness: Harness) -> i32 {
    let state = Arc::new(Server {
        harness,
        session: Mutex::new(None),
        out: Mutex::new(std::io::stdout()),
    });
    let stdin = std::io::stdin();
    let mut workers = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(_) => {
                state.send(&json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": "Parse error"},
                }));
                continue;
            }
        };
        let id = message.get("id").cloned();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a response.
        let Some(id) = id else { continue };
        match method.as_str() {
            "initialize" => state.reply(&id, initialize(&params)),
            "ping" => state.reply(&id, json!({})),
            "tools/list" => state.reply(&id, json!({"tools": tools()})),
            "tools/call" => {
                // A blocking `wait` must not stall pings or other calls.
                let state = Arc::clone(&state);
                workers.push(std::thread::spawn(move || {
                    let result = state.call(&params);
                    state.reply(&id, result);
                }));
            }
            _ => state.send(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("Method not found: {method}")},
            })),
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    0
}

struct Server {
    harness: Harness,
    /// The Agendar session this server registered, once a tool needed it.
    session: Mutex<Option<String>>,
    out: Mutex<std::io::Stdout>,
}

impl Server {
    fn send(&self, message: &Value) {
        if let Ok(mut out) = self.out.lock() {
            let _ = serde_json::to_writer(&mut *out, message);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
    }

    fn reply(&self, id: &Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn call(&self, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let empty = json!({});
        let arguments = params.get("arguments").unwrap_or(&empty);
        let Some((args, stdin)) = command_for(name, arguments) else {
            return tool_error(format!("Unknown tool or missing arguments for `{name}`."));
        };
        // `schema` needs no session; everything else runs as this session.
        let session = if name == "schema" {
            None
        } else {
            match self.ensure_session() {
                Ok(session) => Some(session),
                Err(error) => return tool_error(error),
            }
        };
        match run_cli(&args, session.as_deref(), stdin.as_deref()) {
            Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
            Err(text) => tool_error(text),
        }
    }

    /// Register (or reuse) this server's Agendar session.
    fn ensure_session(&self) -> Result<String, String> {
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "Session lock poisoned.".to_owned())?;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let harness_pid = harness_pid(self.harness);
        let harness_session = harness_session_id(self.harness, harness_pid);
        let repo = std::env::var_os("AGENDAR_REPO")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| "The MCP server has no working folder.".to_owned())?;
        let args = vec![
            "init".to_owned(),
            "--harness".to_owned(),
            harness_arg(self.harness).to_owned(),
            "--harness-session".to_owned(),
            harness_session,
            "--pid".to_owned(),
            harness_pid.to_string(),
            "--repo".to_owned(),
            repo.to_string_lossy().into_owned(),
        ];
        let text = run_cli(&args, None, None)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("Unreadable init output: {error}."))?;
        let session = value["data"]["session"]
            .as_str()
            .ok_or_else(|| format!("init returned no session: {text}"))?
            .to_owned();
        *guard = Some(session.clone());
        Ok(session)
    }
}

fn tool_error(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|version| **version == requested)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "agendar", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

/// Run this executable with `args` (plus `--session`), feeding `stdin` when
/// given. Ok is the JSON envelope on stdout; Err the error text.
fn run_cli(args: &[String], session: Option<&str>, stdin: Option<&str>) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|error| format!("Cannot find agendar: {error}."))?;
    let mut command = Command::new(exe);
    command.args(args);
    if let Some(session) = session {
        command.arg("--session").arg(session);
    }
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot run agendar: {error}."))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("agendar did not finish: {error}."))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn str_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn request_id() -> String {
    format!("mcp-{}", ulid::Ulid::generate())
}

/// The CLI arguments (and stdin JSON) for one tool call.
fn command_for(name: &str, arguments: &Value) -> Option<(Vec<String>, Option<String>)> {
    let mut args: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| args.extend(items.iter().map(|item| (*item).to_owned()));
    let mut stdin = None;
    let mutating = !matches!(name, "board" | "settle" | "inbox" | "wait" | "schema");
    match name {
        "board" => {
            push(&["board"]);
            // An empty milestone is the page of steps without one.
            if let Some(milestone) = arguments.get("milestone").and_then(Value::as_str) {
                push(&["--milestone", milestone]);
            } else if arguments.get("all").and_then(Value::as_bool) == Some(true) {
                push(&["--all"]);
            }
            if let Some(out) = str_arg(arguments, "out") {
                push(&["--out", out]);
            }
        }
        "inbox" => push(&["inbox"]),
        "settle" => push(&["settle"]),
        "schema" => push(&["schema", str_arg(arguments, "kind").unwrap_or("card")]),
        "declare_project" => {
            push(&["project", "declare", "--json", "-"]);
            stdin = Some(arguments.to_string());
        }
        "declare_step" => {
            let node = str_arg(arguments, "node")?;
            let mut body = arguments.clone();
            body.as_object_mut()?.remove("node");
            push(&["feature", "declare", "--node", node, "--json", "-"]);
            stdin = Some(body.to_string());
        }
        "start" => {
            push(&[
                "progress",
                "started",
                "--node",
                str_arg(arguments, "node")?,
                "--eta",
                str_arg(arguments, "eta")?,
            ]);
            if let Some(note) = str_arg(arguments, "note") {
                push(&["--note", note]);
            }
            if let Some(applies) = str_arg(arguments, "applies") {
                push(&["--applies", applies]);
            }
        }
        "pulse" => {
            push(&[
                "pulse",
                "--node",
                str_arg(arguments, "node")?,
                "--attempt",
                str_arg(arguments, "attempt")?,
                "--message",
                str_arg(arguments, "message")?,
            ]);
            if let Some(eta) = str_arg(arguments, "eta") {
                push(&["--eta", eta]);
            }
            if arguments.get("boundary").and_then(Value::as_bool) == Some(true) {
                push(&["--boundary"]);
            }
        }
        "done" => push(&[
            "progress",
            "done",
            "--node",
            str_arg(arguments, "node")?,
            "--attempt",
            str_arg(arguments, "attempt")?,
            "--note",
            str_arg(arguments, "note")?,
        ]),
        "drop" => {
            push(&[
                "progress",
                "dropped",
                "--node",
                str_arg(arguments, "node")?,
                "--note",
                str_arg(arguments, "note")?,
            ]);
            if let Some(attempt) = str_arg(arguments, "attempt") {
                push(&["--attempt", attempt]);
            }
        }
        "ask" => {
            push(&["ask", "--mode", str_arg(arguments, "mode").unwrap_or("ask")]);
            if let Some(node) = str_arg(arguments, "node") {
                push(&["--node", node]);
            }
            if let Some(card) = arguments.get("card").filter(|card| card.is_object()) {
                push(&["--json", "-"]);
                stdin = Some(card.to_string());
            } else {
                // Quick form: a question and its options.
                push(&["--question", str_arg(arguments, "question")?]);
                for option in arguments.get("options")?.as_array()? {
                    push(&["--option", option.as_str()?]);
                }
                let recommend = arguments
                    .get("recommend")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string());
                if let Some(recommend) = recommend.as_deref() {
                    push(&["--recommend", recommend]);
                }
                if let Some(why) = str_arg(arguments, "why") {
                    push(&["--why", why]);
                }
            }
        }
        "wait" => {
            let timeout = str_arg(arguments, "timeout").unwrap_or(DEFAULT_WAIT);
            let timeout = if duration_seconds(timeout)? > duration_seconds(MAX_WAIT)? {
                MAX_WAIT
            } else {
                timeout
            };
            match str_arg(arguments, "delivery_id") {
                Some(delivery) => push(&["wait", "--delivery", delivery, "--timeout", timeout]),
                None => push(&["wait", str_arg(arguments, "ask_id")?, "--timeout", timeout]),
            }
        }
        "ack" => push(&["ack", str_arg(arguments, "answer_id")?]),
        "report" => {
            push(&[
                "report",
                "--title",
                str_arg(arguments, "title")?,
                "--summary",
                str_arg(arguments, "summary")?,
                "--file",
                str_arg(arguments, "file")?,
            ]);
            if let Some(node) = str_arg(arguments, "node") {
                push(&["--node", node]);
            }
            if let Some(kind) = str_arg(arguments, "kind") {
                push(&["--kind", kind]);
            }
            if let Some(domain) = str_arg(arguments, "domain") {
                push(&["--domain", domain]);
            }
        }
        _ => return None,
    }
    if mutating {
        args.push("--request-id".to_owned());
        args.push(request_id());
    }
    Some((args, stdin))
}

/// Seconds in a compound duration such as `90s`, `5m`, `1h30m` (enough to
/// cap `wait`; the CLI validates the real value).
fn duration_seconds(text: &str) -> Option<u64> {
    let mut total = 0_u64;
    let mut number = String::new();
    for character in text.trim().chars() {
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        let value: u64 = number.parse().ok()?;
        number.clear();
        total += value
            * match character {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86_400,
                _ => return None,
            };
    }
    if !number.is_empty() {
        total += number.parse::<u64>().ok()?;
    }
    Some(total)
}

/// The harness process: the nearest matching ancestor, else this server's
/// parent (the harness that started it).
fn harness_pid(harness: Harness) -> u32 {
    agendar_core::runtime::sessions::find_harness_process_for(harness)
        .map(|(pid, _)| pid)
        .or_else(|| {
            agendar_core::runtime::sessions::process_parent(std::process::id())
                .map(|(parent, _)| parent)
        })
        .unwrap_or_else(std::process::id)
}

/// The harness's own session id when its environment carries one; otherwise
/// one id per harness process and server.
fn harness_session_id(harness: Harness, harness_pid: u32) -> String {
    let names: &[&str] = match harness {
        Harness::Pi => &["PI_SESSION_ID"],
        Harness::Cursor => &["CURSOR_CONVERSATION_ID"],
        Harness::Gemini => &["GEMINI_SESSION_ID"],
        Harness::Agy => &["ANTIGRAVITY_CONVERSATION_ID"],
        Harness::Dsh => &["DSH_SESSION_ID"],
        Harness::Codex => &["CODEX_THREAD_ID"],
        _ => &[],
    };
    std::iter::once("AGENDAR_HARNESS_SESSION")
        .chain(names.iter().copied())
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| format!("mcp-{harness_pid}-{}", std::process::id()))
}

fn harness_arg(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude_code",
        Harness::Codex => "codex",
        Harness::Generic => "generic",
        Harness::Pi => "pi",
        Harness::Herdr => "herdr",
        Harness::OpenCode => "opencode",
        Harness::Cursor => "cursor",
        Harness::Dsh => "dsh",
        Harness::Agy => "agy",
        Harness::Gemini => "gemini",
    }
}

/// The tool list with input schemas. Text limits match `agendar schema`.
fn tools() -> Vec<Value> {
    let node =
        json!({"type": "string", "description": "Step path, e.g. core/wake (lowercase segments)."});
    let attempt = json!({"type": "string", "description": "The attempt id `start` returned."});
    let eta = json!({"type": "string", "description": "Honest range MIN..MAX, e.g. 30m..1h30m."});
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        let mut schema = BTreeMap::new();
        schema.insert("type", json!("object"));
        schema.insert("properties", properties);
        schema.insert("required", json!(required));
        json!({"name": name, "description": description, "inputSchema": schema})
    };
    vec![
        tool(
            "board",
            "The project as you need it now: open steps, your steps, what you may start, others' work, reminders, and `pages`: one per milestone with its counts. Pass `milestone` for one page (finished steps short), `all` for every step, `out` to save the JSON to a file. Read-only.",
            json!({
                "milestone": {"type": "string", "description": "One milestone page; \"\" is the steps without a milestone."},
                "all": {"type": "boolean", "description": "Every step, finished ones included (default false; ignored with milestone)."},
                "out": {"type": "string", "description": "Absolute file path (folder must exist, not in the Vault): write the JSON there and return only path, bytes, steps."}
            }),
            &[],
        ),
        tool(
            "settle",
            "What you still owe the board, one line each with the command: steps you hold (past ETA, or when they free), steps you may take, your questions on closed steps, a few planned steps to leave, patch or drop. Read-only; call it before you stop.",
            json!({}),
            &[],
        ),
        tool(
            "inbox",
            "Answers to your questions and reminders (overdue steps, plan changes). Read-only; never acks.",
            json!({}),
            &[],
        ),
        tool(
            "schema",
            "Exact JSON shapes, enums and text limits for card, feature, project or eta.",
            json!({"kind": {"type": "string", "enum": ["card", "feature", "project", "eta"]}}),
            &["kind"],
        ),
        tool(
            "declare_project",
            "Declare or patch the project: title (≤80), brief and goal (≤160), glyph.",
            json!({
                "title": {"type": "string", "maxLength": 80},
                "brief": {"type": "string", "maxLength": 160},
                "goal": {"type": "string", "maxLength": 160},
                "glyph": {"type": "string"},
            }),
            &[],
        ),
        tool(
            "declare_step",
            "Declare or patch one step of your plan (see `schema` kind feature).",
            json!({
                "node": node,
                "title": {"type": "string", "maxLength": 80},
                "depends_on": {"type": "array", "items": {"type": "string"}},
                "milestone": {"type": "string", "maxLength": 80},
                "slice": {"type": "string", "maxLength": 80},
                "demo_kind": {"type": "string", "enum": ["screenshot", "cli_transcript", "preview_url", "recording", "diff"]},
            }),
            &["node"],
        ),
        tool(
            "start",
            "Start working on a step with an ETA range; returns the attempt id. `applies` cites an answer this work applies.",
            json!({"node": node, "eta": eta, "note": {"type": "string", "maxLength": 160}, "applies": {"type": "string"}}),
            &["node", "eta"],
        ),
        tool(
            "pulse",
            "Report progress on a running step; give a new eta when your estimate changes.",
            json!({"node": node, "attempt": attempt, "message": {"type": "string", "maxLength": 160}, "eta": eta, "boundary": {"type": "boolean"}}),
            &["node", "attempt", "message"],
        ),
        tool(
            "done",
            "Mark a step finished, with a short note of what was done.",
            json!({"node": node, "attempt": attempt, "note": {"type": "string", "maxLength": 160}}),
            &["node", "attempt", "note"],
        ),
        tool(
            "drop",
            "Abandon a step (omit attempt when it never started).",
            json!({"node": node, "attempt": attempt, "note": {"type": "string", "maxLength": 160}}),
            &["node", "note"],
        ),
        tool(
            "ask",
            "Ask the human. Every question for them goes here, never in chat: a choice, a go-ahead, a consent. Quick form: question + 2-4 options (\"Label: what happens\"), optional recommend (1-based) and why; or a full `card` (see `schema`). node defaults to the step you are doing. Returns ask_id; then `wait`.",
            json!({
                "question": {"type": "string", "maxLength": 137},
                "options": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 4},
                "recommend": {"type": "integer", "minimum": 1, "maximum": 4},
                "why": {"type": "string", "maxLength": 175},
                "node": node,
                "card": {"type": "object"},
                "mode": {"type": "string", "enum": ["ask", "assume", "auto"]},
            }),
            &[],
        ),
        tool(
            "wait",
            "Block until the answer to ask_id exists, or with delivery_id until the human accepted or sent back that delivery (default 5m, at most 30m); call again if it times out. After `deliver`, wait here instead of polling.",
            json!({"ask_id": {"type": "string"}, "delivery_id": {"type": "string", "description": "The event_id deliver returned."}, "timeout": {"type": "string", "description": "e.g. 5m"}}),
            &[],
        ),
        tool(
            "ack",
            "Acknowledge an answer after you applied its receipt.",
            json!({"answer_id": {"type": "string"}}),
            &["answer_id"],
        ),
        tool(
            "report",
            "Publish a report: a short summary (≤600) and a document (html, md or txt) the human opens in a browser. kind proposal: read before a choice; finding: an experiment, audit or investigation.",
            json!({"title": {"type": "string", "maxLength": 80}, "summary": {"type": "string", "maxLength": 600}, "file": {"type": "string", "description": "Absolute path."}, "node": node, "kind": {"type": "string", "enum": ["proposal", "finding"]}, "domain": {"type": "string", "enum": ["arch", "code", "ux"]}}),
            &["title", "summary", "file"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_passes_its_kind_and_domain() {
        let (args, _) = command_for(
            "report",
            &json!({"title": "Audit", "summary": "s", "file": "/tmp/a.md", "kind": "finding", "domain": "code"}),
        )
        .expect("report");
        let at = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .map(|index| args[index + 1].as_str())
        };
        assert_eq!(at("--kind"), Some("finding"));
        assert_eq!(at("--domain"), Some("code"));
        let (args, _) = command_for(
            "report",
            &json!({"title": "Audit", "summary": "s", "file": "/tmp/a.md"}),
        )
        .expect("report");
        assert!(!args.iter().any(|arg| arg == "--kind" || arg == "--domain"));
    }

    #[test]
    fn board_maps_milestone_all_and_out() {
        let args = |arguments: Value| command_for("board", &arguments).expect("board").0;
        assert_eq!(args(json!({})), ["board"]);
        assert_eq!(args(json!({"all": false})), ["board"]);
        assert_eq!(args(json!({"all": true})), ["board", "--all"]);
        assert_eq!(
            args(json!({"milestone": "Next batch 1"})),
            ["board", "--milestone", "Next batch 1"]
        );
        assert_eq!(args(json!({"milestone": ""})), ["board", "--milestone", ""]);
        assert_eq!(
            args(json!({"all": true, "out": "/tmp/b.json"})),
            ["board", "--all", "--out", "/tmp/b.json"]
        );
        let board = tools()
            .into_iter()
            .find(|tool| tool["name"] == "board")
            .expect("board tool");
        let properties = &board["inputSchema"]["properties"];
        assert_eq!(properties["all"]["type"], "boolean");
        assert_eq!(properties["milestone"]["type"], "string");
        assert_eq!(properties["out"]["type"], "string");
    }

    #[test]
    fn tools_map_onto_cli_commands() {
        let (args, stdin) = command_for(
            "start",
            &json!({"node": "core/wake", "eta": "30m..1h", "note": "go"}),
        )
        .expect("start");
        assert_eq!(
            &args[..6],
            [
                "progress",
                "started",
                "--node",
                "core/wake",
                "--eta",
                "30m..1h"
            ]
        );
        assert!(args.contains(&"--request-id".to_owned()));
        assert!(stdin.is_none());
        let (args, stdin) = command_for(
            "declare_step",
            &json!({"node": "core/wake", "title": "Wake"}),
        )
        .expect("declare");
        assert_eq!(
            &args[..6],
            ["feature", "declare", "--node", "core/wake", "--json", "-"]
        );
        assert_eq!(stdin.as_deref(), Some(r#"{"title":"Wake"}"#));
        let (args, _) =
            command_for("wait", &json!({"ask_id": "A", "timeout": "5h"})).expect("wait");
        assert_eq!(args, ["wait", "A", "--timeout", "30m"], "capped, read-only");
        let (args, _) = command_for("wait", &json!({"delivery_id": "D"})).expect("verdict wait");
        assert_eq!(args, ["wait", "--delivery", "D", "--timeout", "5m"]);
        assert!(
            command_for("wait", &json!({})).is_none(),
            "a target is required"
        );
        assert!(
            command_for("start", &json!({"node": "core/wake"})).is_none(),
            "eta required"
        );
        let (args, stdin) = command_for(
            "ask",
            &json!({"question": "Which store?", "options": ["SQLite: one file", "JSON: diffable"], "recommend": 2}),
        )
        .expect("quick ask");
        assert_eq!(
            &args[..11],
            [
                "ask",
                "--mode",
                "ask",
                "--question",
                "Which store?",
                "--option",
                "SQLite: one file",
                "--option",
                "JSON: diffable",
                "--recommend",
                "2"
            ]
        );
        assert!(stdin.is_none());
        assert!(command_for("nope", &json!({})).is_none(), "unknown tool");
        assert!(command_for("nope", &json!({})).is_none());
    }
}
