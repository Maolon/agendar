//! `agendar settle` and the debt line, end to end against a temp Vault: a
//! session holding a step past its ETA, a free step another agent abandoned,
//! a question on a step it dropped, a step the owner handed off after its
//! question was answered (continued with `--applies`), and planned steps
//! offered five at a time.
//!
//! Past starts are written through the core with a clock in the past (as if a
//! host's file synced late), so the test does not wait. Set
//! `AGENDAR_SETTLE_TRANSCRIPT` to a file path to save the CLI transcript.

mod support;

use std::fmt::Write as _;
use std::fs;

use serde_json::{Value, json};

use agendar_core::model::event::{
    Agent, EventPayload, LostReason, Refs, SessionLost, WorkProgress, WorkStatus,
};
use agendar_core::model::{
    EtaRange, Gate, Harness, HostId, NodePath, ProjectSlug, RescueAction, Seconds, SessionId,
    UtcTime, WriterId, WriterRole,
};
use agendar_core::runtime::LocalPaths;
use agendar_core::vault::{
    Clock, EventDraft, SystemClock, UlidIdSource, Vault, WriterKey, append_event,
};
use support::{CliEnv, expect_success};

/// A fixed clock for the backdated events.
struct At(UtcTime);

impl Clock for At {
    fn now(&self) -> UtcTime {
        self.0
    }
}

struct Transcript(String);

impl Transcript {
    fn run(&mut self, env: &CliEnv, who: &str, session: &str, args: &[&str]) -> Value {
        let output = env.run_as(session, args);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let _ = writeln!(
            self.0,
            "$ agendar --session <{who}> {}\n{}{}\n",
            args.join(" "),
            stdout.trim_end(),
            stderr.trim_end()
        );
        serde_json::from_str(&stdout).unwrap_or(Value::Null)
    }

    fn note(&mut self, text: &str) {
        let _ = writeln!(self.0, "# {text}\n");
    }
}

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("json")).expect("write json");
    path.to_string_lossy().into_owned()
}

fn key(env: &CliEnv, role: WriterRole) -> WriterKey {
    WriterKey {
        project: ProjectSlug::parse(&env.project).expect("project"),
        writer: WriterId::new(HostId::parse(&env.host).expect("host"), role),
    }
}

fn append(env: &CliEnv, role: WriterRole, draft: EventDraft, secs_ago: i64) -> String {
    let vault = Vault::open(&env.vault).expect("vault");
    let local = LocalPaths::new(&env.data).expect("local paths");
    let clock = At(SystemClock.now().add_seconds(-secs_ago));
    append_event(
        &vault,
        &local,
        &key(env, role),
        draft,
        &clock,
        &mut UlidIdSource,
    )
    .expect("append")
    .event
    .id
    .as_str()
    .to_owned()
}

/// `progress started` by `session` on `node`, `secs_ago` in the past.
fn backdated_start(env: &CliEnv, session: &str, node: &str, secs_ago: i64, note: &str) -> String {
    let session: SessionId = session.parse().expect("session");
    let draft = EventDraft {
        refs: Refs {
            node: Some(NodePath::parse(node).expect("node")),
            ..Refs::default()
        },
        payload: EventPayload::WorkProgress(WorkProgress {
            agent: Agent {
                session: session.clone(),
                harness: Harness::Generic,
            },
            status: WorkStatus::Started,
            eta: Some(EtaRange {
                min_s: Seconds::new(30 * 60).expect("min"),
                max_s: Seconds::new(60 * 60).expect("max"),
            }),
            evidence: Vec::new(),
            note: Some(note.to_owned()),
        }),
    };
    append(env, WriterRole::Agent(session), draft, secs_ago)
}

/// The host reports `session` gone (what the steward writes).
fn lost(env: &CliEnv, session: &str) -> String {
    let draft = EventDraft {
        refs: Refs::default(),
        payload: EventPayload::SessionLost(SessionLost {
            session: session.parse().expect("session"),
            process_start: 7,
            reason: LostReason::Exited,
            exit_code: None,
        }),
    };
    append(env, WriterRole::Steward, draft, 0)
}

fn declare(env: &CliEnv, node: &str, milestone: Option<&str>) {
    let mut patch = json!({"title": node, "slice": "s", "demo_kind": "cli_transcript"});
    if let Some(milestone) = milestone {
        patch["milestone"] = json!(milestone);
    }
    let path = write_json(env, "feature.json", &patch);
    expect_success(
        &env.run(&["feature", "declare", "--node", node, "--json", &path]),
        "feature declare",
    );
}

fn ask(env: &CliEnv, session: &str, node: &str) -> String {
    let asked = expect_success(
        &env.run_as(
            session,
            &[
                "ask",
                "--node",
                node,
                "--question",
                "Which store?",
                "--option",
                "SQLite: one file",
                "--option",
                "JSON lines: diffable",
            ],
        ),
        "ask",
    );
    asked["data"]["ask_id"].as_str().expect("ask id").to_owned()
}

fn lines(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["say"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn nodes(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["node"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn settle_says_what_a_session_owes_and_boundary_commands_carry_one_debt_line() {
    let mut env = CliEnv::new("demo");
    env.init();
    let a = env.session.clone();
    let project = write_json(
        &env,
        "project.json",
        &json!({"title": "Settle demo", "brief": "b", "goal": "g"}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    // 30 steps: four in play (declared by A), 26 planned in two milestones
    // (declared by B).
    declare(&env, "core/held", Some("MVP"));
    declare(&env, "core/gone", Some("MVP"));
    declare(&env, "core/wake", Some("MVP"));
    declare(&env, "core/handoff", Some("MVP"));
    env.init();
    let b = env.session.clone();
    for index in 1..=26 {
        let milestone = if index % 2 == 0 { "MVP" } else { "Later" };
        declare(&env, &format!("plan/s{index:02}"), Some(milestone));
    }
    env.init();
    let c = env.session.clone();

    let mut log = Transcript(String::new());
    log.note("A started core/held 3 hours ago with an ETA of 30m..1h and said nothing since.");
    let held = backdated_start(&env, &a, "core/held", 3 * 60 * 60, "first pass");
    log.note("B started core/wake two days ago and went silent: the step is free.");
    backdated_start(
        &env,
        &b,
        "core/wake",
        2 * 24 * 60 * 60,
        "wiring the wake adapter",
    );

    log.note("A starts core/gone, asks about it, then drops it: its question is on a closed step.");
    let gone = log.run(
        &env,
        "A",
        &a,
        &[
            "progress",
            "started",
            "--node",
            "core/gone",
            "--eta",
            "10m..20m",
        ],
    );
    assert!(
        gone["data"].get("debt").is_none(),
        "no debt line on a start"
    );
    let gone_attempt = gone["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let question = ask(&env, &a, "core/gone");
    let dropped = log.run(
        &env,
        "A",
        &a,
        &[
            "progress",
            "dropped",
            "--node",
            "core/gone",
            "--attempt",
            &gone_attempt,
            "--note",
            "covered by core/held",
        ],
    );
    assert_eq!(
        dropped["data"]["debt"],
        "open: 1 yours, 1 free to take, 1 question on a closed step, 27 planned · agendar settle",
        "{dropped}"
    );

    log.note("C asks on core/handoff, the owner answers, C acks and its process is gone; the owner hands the step off.");
    let started = expect_success(
        &env.run_as(
            &c,
            &[
                "progress",
                "started",
                "--node",
                "core/handoff",
                "--eta",
                "1h..2h",
            ],
        ),
        "progress",
    );
    assert!(started["data"]["attempt"].is_string());
    let handoff_ask = ask(&env, &c, "core/handoff");
    let answer = env.answer(&handoff_ask, "a");
    expect_success(&env.run_as(&c, &["ack", &answer]), "ack");
    lost(&env, &c);
    let service = env.service();
    let slug = ProjectSlug::parse("demo").expect("slug");
    let state = service.export_state(SystemClock.now()).expect("state");
    let row = state.projects[&slug]
        .needs_you
        .iter()
        .find(|row| row.node.as_ref().map(NodePath::as_str) == Some("core/handoff"))
        .map(|row| row.id.clone())
        .expect("rescue row");
    service
        .resolve_rescue(&slug, &row, RescueAction::Handoff)
        .expect("hand off");

    log.note("A runs settle.");
    let settled = log.run(&env, "A", &a, &["settle"]);
    let settle_bytes = serde_json::to_string(&settled).expect("json").len();
    let data = &settled["data"];
    assert_eq!(
        data["open"],
        "open: 1 yours, 2 to take, 1 question on a closed step, 26 planned"
    );
    // Its open question holds its steps for the owner (until it folds).
    assert_eq!(
        lines(&data["yours"]),
        [format!(
            "core/held: held while the owner answers {question}"
        )]
    );
    let take = lines(&data["take"]);
    assert_eq!(
        take,
        [
            format!(
                "you may continue core/handoff (handed off by the owner); apply its answer: progress started --node core/handoff --eta <range> --applies {answer}"
            ),
            "you may take core/wake (abandoned 23h00m ago): progress started --node core/wake --eta <range>".to_owned(),
        ]
    );
    let questions = lines(&data["questions"]);
    assert_eq!(questions.len(), 1);
    assert!(
        questions[0].starts_with(&format!(
            "your question {question} on core/gone is on a closed step: withdraw {question} --reason <why>, or it folds in 23h59m"
        )),
        "{}",
        questions[0]
    );
    assert_eq!(data["planned"], 26);
    // All agent-written, no stronger signal separates them: the oldest touch first.
    assert_eq!(
        nodes(&data["candidates"]),
        ["plan/s01", "plan/s02", "plan/s03", "plan/s04", "plan/s05"]
    );

    log.note("A's board, for size: settle is a fraction of it.");
    let output = env.run_as(&a, &["board"]);
    let board_bytes = String::from_utf8_lossy(&output.stdout).trim_end().len();
    let board: Value = serde_json::from_slice(&output.stdout).expect("board");
    let _ = writeln!(
        log.0,
        "# bytes on a 30-step project: board {board_bytes}, settle {settle_bytes}\n\
         # board excerpt: runnable {}, applies {}, free {}\n",
        board["data"]["runnable"], board["data"]["applies"], board["data"]["free"]
    );
    assert!(
        settle_bytes * 2 < board_bytes,
        "settle {settle_bytes} vs board {board_bytes}"
    );
    // reconcile names only what A may act on: no lost owner's step it cannot take.
    let reconcile: Vec<(String, String)> = board["data"]["reconcile"]
        .as_array()
        .expect("reconcile")
        .iter()
        .map(|step| {
            (
                step["node"].as_str().unwrap_or_default().to_owned(),
                step["owner"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .filter(|(node, _)| node.starts_with("core/"))
        .collect();
    assert_eq!(
        reconcile,
        [
            ("core/handoff".to_owned(), "handed_off".to_owned()),
            ("core/held".to_owned(), "you".to_owned()),
            ("core/wake".to_owned(), "free".to_owned()),
        ]
    );
    let _ = writeln!(log.0, "# board reconcile (core/*): {reconcile:?}\n");
    assert!(
        board["data"]["runnable"]
            .as_array()
            .expect("runnable")
            .iter()
            .any(|node| node == "core/handoff"),
        "{board}"
    );
    assert_eq!(board["data"]["applies"]["core/handoff"], json!([answer]));

    log.note(
        "A withdraws the question on the dropped step, as settle suggested, and settles again.",
    );
    let withdrawn = log.run(
        &env,
        "A",
        &a,
        &["withdraw", &question, "--reason", "core/gone was dropped"],
    );
    assert_eq!(withdrawn["ok"], true, "{withdrawn}");
    let again = log.run(&env, "A", &a, &["settle"]);
    let data = &again["data"];
    assert_eq!(
        data["open"],
        "open: 1 yours (1 past ETA), 2 to take, 26 planned"
    );
    let yours = lines(&data["yours"]);
    assert!(
        (yours[0].starts_with("core/held: 2h00m past ETA, freed in 2")
            || yours[0].starts_with("core/held: 1h59m past ETA, freed in 2"))
            && yours[0].contains(" if no word: pulse --node core/held"),
        "{}",
        yours[0]
    );
    assert!(yours[0].contains(&format!("--attempt {held}")));
    assert!(data.get("questions").is_none());
    // The five planned steps shown rest for a week; the next five come.
    assert_eq!(
        nodes(&data["candidates"]),
        ["plan/s06", "plan/s07", "plan/s08", "plan/s09", "plan/s10"]
    );

    log.note("A pulses core/held with a new ETA: no debt line on a pulse.");
    let pulse = log.run(
        &env,
        "A",
        &a,
        &[
            "pulse",
            "--node",
            "core/held",
            "--attempt",
            &held,
            "--eta",
            "20m..40m",
            "--message",
            "tests pass, wiring the CLI",
        ],
    );
    assert_eq!(pulse["ok"], true, "{pulse}");
    assert!(pulse["data"].get("debt").is_none());

    log.note("A continues the handed-off step, applying the answer.");
    let continued = log.run(
        &env,
        "A",
        &a,
        &[
            "progress",
            "started",
            "--node",
            "core/handoff",
            "--eta",
            "20m..40m",
            "--applies",
            &answer,
        ],
    );
    assert_eq!(continued["ok"], true, "{continued}");
    let state = service.export_state(SystemClock.now()).expect("state");
    let step = &state.projects[&slug].features[&NodePath::parse("core/handoff").expect("node")];
    assert_eq!(step.gate, Gate::Open);
    assert_eq!(
        step.session.as_ref().map(SessionId::as_str),
        Some(a.as_str())
    );
    let attempt = continued["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();

    log.note("A finishes it: the boundary command says what is still open.");
    let done = log.run(
        &env,
        "A",
        &a,
        &[
            "progress",
            "done",
            "--node",
            "core/handoff",
            "--attempt",
            &attempt,
        ],
    );
    assert_eq!(
        done["data"]["debt"], "open: 1 yours, 1 free to take, 26 planned · agendar settle",
        "{done}"
    );

    if let Ok(path) = std::env::var("AGENDAR_SETTLE_TRANSCRIPT") {
        let redacted = log
            .0
            .replace(env.root.path().to_string_lossy().as_ref(), "<tmp>");
        fs::write(path, redacted).expect("write transcript");
    }
}

#[test]
fn the_debt_line_is_only_on_boundary_commands_and_only_when_something_is_owed() {
    let mut env = CliEnv::new("demo");
    env.init();
    let project = write_json(
        &env,
        "project.json",
        &json!({"title": "Debt", "brief": "b", "goal": "g"}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    declare(&env, "core/a", None);
    let started = expect_success(
        &env.run(&["progress", "started", "--node", "core/a", "--eta", "1h..2h"]),
        "progress",
    );
    assert!(started["data"].get("debt").is_none());
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();

    // Delivered: in review, nothing owed, so no line.
    let demo = env.repo.join("demo.txt");
    fs::write(&demo, b"demo transcript\n").expect("demo");
    let delivery = write_json(
        &env,
        "delivery.json",
        &json!({
            "name": "A delivered",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo.to_string_lossy(), "ext": "txt"}}],
            "disclosure": {"proven": [], "not_proven": [], "faked": []}
        }),
    );
    let delivered = expect_success(
        &env.run(&[
            "deliver",
            "--node",
            "core/a",
            "--attempt",
            &attempt,
            "--json",
            &delivery,
        ]),
        "deliver",
    );
    assert!(delivered["data"].get("debt").is_none(), "{delivered}");
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert!(inbox["data"].get("debt").is_none(), "{inbox}");
    let settle = expect_success(&env.run(&["settle"]), "settle");
    assert_eq!(
        settle["data"],
        json!({"open": "open: nothing", "in_review": {"n": 1, "say": "nothing to do: the owner holds it"}, "planned": 0})
    );

    // A new planned step is something to look at once: inbox carries the line,
    // reads do not.
    declare(&env, "core/b", None);
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(inbox["data"]["debt"], "open: 1 planned · agendar settle");
    for (args, command) in [
        (vec!["board"], "board"),
        (vec!["status"], "status"),
        (vec!["rules"], "rules"),
    ] {
        let read = expect_success(&env.run(&args), command);
        assert!(read["data"].get("debt").is_none(), "{command}: {read}");
    }
    let settle = expect_success(&env.run(&["settle"]), "settle");
    assert_eq!(nodes(&settle["data"]["candidates"]), ["core/b"]);
    // Shown: nothing owed for a week.
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert!(inbox["data"].get("debt").is_none(), "{inbox}");
    // Dropping it is a boundary; nothing left, so no line.
    let dropped = expect_success(
        &env.run(&[
            "progress",
            "dropped",
            "--node",
            "core/b",
            "--note",
            "not needed",
        ]),
        "progress",
    );
    assert!(dropped["data"].get("debt").is_none(), "{dropped}");
}
