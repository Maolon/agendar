//! A free step is open to any agent: agent A starts and goes silent, a day
//! later agent B finds the step in `board`'s runnable list and starts it with
//! no hand-off, and A's next word is refused with a pointer to the board.
//!
//! A's start is written two days in the past (as if its host's file synced
//! late), so the test does not wait a day. Set `AGENDAR_TAKEOVER_TRANSCRIPT`
//! to a file path to save the CLI transcript.

mod support;

use std::fmt::Write as _;
use std::fs;

use serde_json::{Value, json};

use agendar_core::model::event::{Agent, EventPayload, Refs, WorkProgress, WorkStatus};
use agendar_core::model::{
    EtaRange, Harness, HostId, NodePath, ProjectSlug, Seconds, SessionId, UtcTime, WriterId,
    WriterRole,
};
use agendar_core::reduce::nudges::NudgeKind;
use agendar_core::runtime::LocalPaths;
use agendar_core::vault::{
    Clock, EventDraft, SystemClock, UlidIdSource, Vault, WriterKey, append_event,
};
use support::{CliEnv, expect_error, expect_success};

const NODE: &str = "core/wake";

/// A fixed clock for the one backdated event.
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
        let _ = writeln!(
            self.0,
            "$ agendar --session <{who}> {}\n{}\n",
            args.join(" "),
            stdout.trim_end()
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

/// Agent A's `progress started` on `core/wake`, written `days_ago` in the past.
fn backdated_start(env: &CliEnv, session: &str, days_ago: i64) -> String {
    let vault = Vault::open(&env.vault).expect("vault");
    let local = LocalPaths::new(&env.data).expect("local paths");
    let session: SessionId = session.parse().expect("session");
    let key = WriterKey {
        project: ProjectSlug::parse(&env.project).expect("project"),
        writer: WriterId::new(
            HostId::parse(&env.host).expect("host"),
            WriterRole::Agent(session.clone()),
        ),
    };
    let draft = EventDraft {
        refs: Refs {
            node: Some(NodePath::parse(NODE).expect("node")),
            ..Refs::default()
        },
        payload: EventPayload::WorkProgress(WorkProgress {
            agent: Agent {
                session,
                harness: Harness::Generic,
            },
            status: WorkStatus::Started,
            eta: Some(EtaRange {
                min_s: Seconds::new(30 * 60).expect("min"),
                max_s: Seconds::new(60 * 60).expect("max"),
            }),
            evidence: Vec::new(),
            note: Some("wiring the wake adapter".to_owned()),
        }),
    };
    let clock = At(SystemClock.now().add_seconds(-days_ago * 24 * 60 * 60));
    let appended =
        append_event(&vault, &local, &key, draft, &clock, &mut UlidIdSource).expect("append");
    appended.event.id.as_str().to_owned()
}

#[test]
fn an_abandoned_step_is_picked_up_by_another_agent_without_a_hand_off() {
    let mut env = CliEnv::new("demo");
    env.init();
    let a = env.session.clone();
    let project = write_json(
        &env,
        "project.json",
        &json!({"title": "Takeover demo", "brief": "b", "goal": "g"}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    let feature = write_json(
        &env,
        "feature.json",
        &json!({"title": "Wake an idle agent", "slice": "core/wake", "demo_kind": "cli_transcript"}),
    );
    expect_success(
        &env.run(&["feature", "declare", "--node", NODE, "--json", &feature]),
        "feature declare",
    );
    env.init();
    let b = env.session.clone();
    assert_ne!(a, b);

    let mut log = Transcript(String::new());
    log.note("Agent A started core/wake two days ago (ETA 30m..1h) and has said nothing since.");
    let attempt = backdated_start(&env, &a, 2);

    log.note("Agent B arrives and reads the board.");
    let board = log.run(&env, "B", &b, &["board"]);
    let board = &board["data"];
    assert!(
        board["runnable"]
            .as_array()
            .expect("runnable")
            .iter()
            .any(|node| node == NODE),
        "{board}"
    );
    let free = &board["free"][0];
    assert_eq!(free["node"], NODE);
    assert_eq!(free["from"], a.as_str());
    assert_eq!(free["note"], "wiring the wake adapter");

    log.note("B starts it: no hand-off, the start cites A's attempt.");
    let started = log.run(
        &env,
        "B",
        &b,
        &["progress", "started", "--node", NODE, "--eta", "20m..40m"],
    );
    assert_eq!(started["ok"], true, "{started}");
    let state = env
        .service()
        .export_state(SystemClock.now())
        .expect("state");
    let project_state = &state.projects[&ProjectSlug::parse("demo").expect("slug")];
    let step = &project_state.features[&NodePath::parse(NODE).expect("node")];
    assert_eq!(step.session.as_ref().map(|s| s.as_str()), Some(b.as_str()));
    assert_eq!(
        step.taken_from.as_ref().map(|from| from.attempt.as_str()),
        Some(attempt.as_str())
    );
    assert!(
        project_state
            .needs_you
            .iter()
            .all(|row| !row.id.starts_with("rescue:")),
        "{:?}",
        project_state.needs_you
    );

    log.note("A comes back and pulses its old attempt: refused, with a pointer to the board.");
    let output = env.run_as(
        &a,
        &[
            "pulse",
            "--node",
            NODE,
            "--attempt",
            &attempt,
            "--message",
            "back at it",
        ],
    );
    let _ = writeln!(
        log.0,
        "$ agendar --session <A> pulse --node {NODE} --attempt <A's attempt> --message 'back at it'\n{}\n",
        String::from_utf8_lossy(&output.stderr).trim_end()
    );
    let refused = expect_error(&output, "E_VALIDATION", 2);
    assert_eq!(refused["error"]["field"], "/attempt");
    assert_eq!(
        refused["error"]["message"],
        "The attempt does not match the feature's current attempt."
    );
    let hint = refused["error"]["hint"].as_str().expect("hint");
    assert!(
        hint.contains("taken over") && hint.contains(&b) && hint.contains("board"),
        "{hint}"
    );

    log.note("A's board carries the taken-over nudge.");
    let board = log.run(&env, "A", &a, &["board"]);
    let nudge = board["data"]["nudges"]
        .as_array()
        .expect("nudges")
        .iter()
        .find(|nudge| nudge["kind"] == "taken_over")
        .cloned()
        .expect("taken-over nudge");
    assert!(
        nudge["text"]
            .as_str()
            .is_some_and(|text| text.contains("no word came from you for a day")),
        "{nudge}"
    );

    // A hook shows it once: after it is marked shown, it is not fresh again.
    let service = env.service();
    let slug = ProjectSlug::parse("demo").expect("slug");
    let a_id: SessionId = a.parse().expect("session");
    let (fresh, cursor, keys) = service.fresh_reminders(&slug, &a_id).expect("reminders");
    assert_eq!(
        fresh
            .iter()
            .filter(|nudge| nudge.kind == NudgeKind::TakenOver)
            .count(),
        1
    );
    service
        .mark_reminded(&a_id, &keys, cursor.as_ref())
        .expect("mark");
    let (again, _, _) = service.fresh_reminders(&slug, &a_id).expect("reminders");
    assert!(again.iter().all(|nudge| nudge.kind != NudgeKind::TakenOver));

    if let Ok(path) = std::env::var("AGENDAR_TAKEOVER_TRANSCRIPT") {
        let redacted = log
            .0
            .replace(env.root.path().to_string_lossy().as_ref(), "<tmp>");
        fs::write(path, redacted).expect("write transcript");
    }
}

/// A project with one step, and two agent sessions (A, then B).
fn two_agents() -> (CliEnv, String, String) {
    let mut env = CliEnv::new("demo");
    env.init();
    let a = env.session.clone();
    let project = write_json(
        &env,
        "project.json",
        &json!({"title": "Takeover demo", "brief": "b", "goal": "g"}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    let feature = write_json(
        &env,
        "feature.json",
        &json!({"title": "Wake an idle agent", "slice": "core/wake", "demo_kind": "cli_transcript"}),
    );
    expect_success(
        &env.run(&["feature", "declare", "--node", NODE, "--json", &feature]),
        "feature declare",
    );
    env.init();
    let b = env.session.clone();
    assert_ne!(a, b);
    (env, a, b)
}

impl Transcript {
    /// A refused command: its error envelope goes to the transcript.
    fn refused(&mut self, env: &CliEnv, who: &str, session: &str, args: &[&str]) -> Value {
        let output = env.run_as(session, args);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let _ = writeln!(
            self.0,
            "$ agendar --session <{who}> {}\n{}\n",
            args.join(" "),
            stderr.trim_end()
        );
        expect_error(&output, "E_POLICY", 4)
    }
}

/// The path for a step that is stopped but not free: agent A holds it,
/// agent B is refused with the exact way to ask, asks the owner saying what
/// it read, and the owner's answer lets it start with `--applies`.
///
/// Set `AGENDAR_TAKEOVER_ASK_TRANSCRIPT` to a file path to save the CLI
/// transcript.
#[test]
fn another_agent_asks_the_owner_for_a_held_step_and_the_answer_lets_it_start() {
    let (env, a, b) = two_agents();
    let mut log = Transcript(String::new());

    log.note("Agent A starts core/wake and says where it is. Its claim is held.");
    let started = log.run(
        &env,
        "A",
        &a,
        &["progress", "started", "--node", NODE, "--eta", "30m..1h"],
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    log.run(
        &env,
        "A",
        &a,
        &[
            "pulse",
            "--node",
            NODE,
            "--attempt",
            &attempt,
            "--message",
            "adapter wired; the retry loop is left",
        ],
    );

    log.note(
        "Agent B wants the step. It is not free, so the start is refused and says how to ask.",
    );
    let start = ["progress", "started", "--node", NODE, "--eta", "20m..40m"];
    let refused = log.refused(&env, "B", &b, &start);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("is held by another agent")),
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains(NODE) && text.contains(&a)),
        "{refused}"
    );
    let hint = refused["error"]["hint"].as_str().expect("hint");
    assert!(
        hint.contains(&format!("agendar ask --node {NODE} --takeover"))
            && hint.contains("what you read")
            && hint.contains(&format!(
                "agendar progress started --node {NODE} --eta <range> --applies <answer_id>"
            ))
            && hint.contains("Its own conversation continues it by itself"),
        "{hint}"
    );

    log.note("B asks the owner on that step and says what it read.");
    let asked = log.run(
        &env,
        "B",
        &b,
        &[
            "ask",
            "--node",
            NODE,
            "--takeover",
            "Read A's last pulse: adapter wired, retry loop left. I have the retry design open.",
        ],
    );
    assert_eq!(asked["ok"], true, "{asked}");
    let ask = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    let service = env.service();
    let slug = ProjectSlug::parse("demo").expect("slug");
    let node = NodePath::parse(NODE).expect("node");
    let state = service.export_state(SystemClock.now()).expect("state");
    let card = &state.projects[&slug].decisions
        [&agendar_core::model::EventId::parse(&ask).expect("id")]
        .card;
    assert_eq!(card.question, format!("May I continue {NODE}?"));
    assert_eq!(
        card.options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        ["takeover", "leave"]
    );
    assert!(
        card.already_checked[0]
            .text
            .starts_with("Read A's last pulse")
    );
    // The question cites what B read: A's attempt.
    let asked_line = env
        .event_lines()
        .into_iter()
        .find(|line| line.contains(&format!("\"id\":\"{ask}\"")))
        .expect("the ask line");
    assert!(asked_line.contains(&attempt), "{asked_line}");

    log.note("Until the owner answers, B is still refused, with the question to wait on.");
    let refused = log.refused(&env, "B", &b, &start);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("is held by another agent")),
        "{refused}"
    );
    assert!(
        refused["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(&format!("agendar wait {ask}"))),
        "{refused}"
    );

    log.note("The owner answers on the card: Let this agent continue.");
    let answer = env.answer(&ask, "takeover");
    let _ = writeln!(
        log.0,
        "(owner) answer {ask} -> takeover   answer_id {answer}\n"
    );

    log.note("B's settle and board name the step and the answer to apply.");
    let settle = log.run(&env, "B", &b, &["settle"]);
    let take = settle["data"]["take"]
        .as_array()
        .expect("take")
        .iter()
        .find(|take| take["node"] == NODE)
        .cloned()
        .expect("the granted step");
    assert_eq!(take["why"], "granted");
    assert_eq!(take["applies"], json!([answer]));
    assert!(
        take["say"]
            .as_str()
            .is_some_and(|say| say.contains("the owner let you continue it")
                && say.contains(&format!("--applies {answer}"))),
        "{take}"
    );
    let board = env.run_as(&b, &["board"]);
    let board = expect_success(&board, "board");
    assert_eq!(board["data"]["applies"][NODE], json!([answer]));

    log.note("B starts by applying the answer.");
    let started = log.run(
        &env,
        "B",
        &b,
        &[
            "progress",
            "started",
            "--node",
            NODE,
            "--eta",
            "20m..40m",
            "--applies",
            &answer,
        ],
    );
    assert_eq!(started["ok"], true, "{started}");
    let state = service.export_state(SystemClock.now()).expect("state");
    let project_state = &state.projects[&slug];
    let step = &project_state.features[&node];
    assert_eq!(step.session.as_ref().map(|s| s.as_str()), Some(b.as_str()));
    let from = step.taken_from.as_ref().expect("taken from");
    assert_eq!(from.attempt.as_str(), attempt);
    assert_eq!(
        from.granted_by.as_ref().map(|id| id.as_str()),
        Some(answer.as_str())
    );
    assert!(
        project_state.needs_you.is_empty(),
        "{:?}",
        project_state.needs_you
    );

    log.note("A comes back: its old attempt is refused and its board says why.");
    let output = env.run_as(
        &a,
        &[
            "pulse",
            "--node",
            NODE,
            "--attempt",
            &attempt,
            "--message",
            "back at it",
        ],
    );
    let _ = writeln!(
        log.0,
        "$ agendar --session <A> pulse --node {NODE} --attempt <A's attempt> --message 'back at it'\n{}\n",
        String::from_utf8_lossy(&output.stderr).trim_end()
    );
    let refused = expect_error(&output, "E_VALIDATION", 2);
    assert!(
        refused["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("taken over") && hint.contains(&b)),
        "{refused}"
    );
    let board = log.run(&env, "A", &a, &["board"]);
    let nudge = board["data"]["nudges"]
        .as_array()
        .expect("nudges")
        .iter()
        .find(|nudge| nudge["kind"] == "taken_over")
        .cloned()
        .expect("taken-over nudge");
    assert!(
        nudge["text"]
            .as_str()
            .is_some_and(|text| text.contains("the owner let another agent continue it")),
        "{nudge}"
    );

    if let Ok(path) = std::env::var("AGENDAR_TAKEOVER_ASK_TRANSCRIPT") {
        let redacted = log
            .0
            .replace(env.root.path().to_string_lossy().as_ref(), "<tmp>")
            .replace(&a, "<A>")
            .replace(&b, "<B>");
        fs::write(path, redacted).expect("write transcript");
    }
}

#[test]
fn the_owner_leaving_a_held_step_with_its_agent_keeps_the_other_agent_out() {
    let (env, a, b) = two_agents();
    let start = ["progress", "started", "--node", NODE, "--eta", "20m..40m"];
    expect_success(&env.run_as(&a, &start), "progress");
    let asked = expect_success(
        &env.run_as(
            &b,
            &["ask", "--node", NODE, "--takeover", "Read its start note."],
        ),
        "ask",
    );
    let ask = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    let answer = env.answer(&ask, "leave");
    let output = env.run_as(
        &b,
        &[
            "progress",
            "started",
            "--node",
            NODE,
            "--eta",
            "20m..40m",
            "--applies",
            &answer,
        ],
    );
    let refused = expect_error(&output, "E_POLICY", 4);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("is held by another agent")),
        "{refused}"
    );

    // The takeover form is for a step another agent holds, and needs it named.
    let own = env.run_as(&a, &["ask", "--node", NODE, "--takeover", "It is mine."]);
    let refused = expect_error(&own, "E_VALIDATION", 2);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("nobody else holds this one")),
        "{refused}"
    );
    let unnamed = env.run_as(&b, &["ask", "--takeover", "Which step?"]);
    assert_ne!(unnamed.status.code(), Some(0));
    // A policy answer is not the owner's: the question must reach them.
    let assumed = env.run_as(
        &b,
        &[
            "ask",
            "--node",
            NODE,
            "--takeover",
            "Read it.",
            "--mode",
            "auto",
        ],
    );
    assert_ne!(assumed.status.code(), Some(0));
}

/// The holder delivers after the owner let another agent continue: the step
/// is in review, the owner's. The granted start is refused with the reason,
/// the review stays, and a takeover question on it is refused too.
#[test]
fn a_grant_does_not_pass_a_delivery_in_review() {
    let (env, a, b) = two_agents();
    let started = expect_success(
        &env.run_as(
            &a,
            &["progress", "started", "--node", NODE, "--eta", "20m..40m"],
        ),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let asked = expect_success(
        &env.run_as(
            &b,
            &["ask", "--node", NODE, "--takeover", "Read its start note."],
        ),
        "ask",
    );
    let ask = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    let answer = env.answer(&ask, "takeover");
    let settle = expect_success(&env.run_as(&b, &["settle"]), "settle");
    assert_eq!(settle["data"]["take"][0]["why"], "granted", "{settle}");

    let demo = env.repo.join("demo.txt");
    fs::write(&demo, b"$ wake\nwoken\n").expect("demo");
    let input = write_json(
        &env,
        "delivery.json",
        &json!({
            "name": "Wake",
            "demos": [{"kind": "cli_transcript", "body": {"file": demo, "ext": "txt"}}],
            "disclosure": {"proven": ["Ran it"], "not_proven": [], "faked": []}
        }),
    );
    let delivered = expect_success(
        &env.run_as(
            &a,
            &[
                "deliver",
                "--node",
                NODE,
                "--attempt",
                &attempt,
                "--json",
                &input,
            ],
        ),
        "deliver",
    );
    let delivery = delivered["data"]["event_id"]
        .as_str()
        .expect("delivery")
        .to_owned();

    let output = env.run_as(
        &b,
        &[
            "progress",
            "started",
            "--node",
            NODE,
            "--eta",
            "20m..40m",
            "--applies",
            &answer,
        ],
    );
    let refused = expect_error(&output, "E_POLICY", 4);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("is in review") && text.contains(&a)),
        "{refused}"
    );
    let settle = expect_success(&env.run_as(&b, &["settle"]), "settle");
    assert!(
        settle["data"]["take"]
            .as_array()
            .is_none_or(|take| take.iter().all(|take| take["node"] != NODE)),
        "{settle}"
    );
    let board = expect_success(&env.run_as(&b, &["board"]), "board");
    assert!(
        board["data"]["runnable"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().all(|node| node != NODE)),
        "{board}"
    );
    let state = env
        .service()
        .export_state(SystemClock.now())
        .expect("state");
    let project_state = &state.projects[&ProjectSlug::parse("demo").expect("slug")];
    let step = &project_state.features[&NodePath::parse(NODE).expect("node")];
    assert_eq!(step.session.as_ref().map(|s| s.as_str()), Some(a.as_str()));
    assert_eq!(
        step.attempt.as_ref().map(|id| id.as_str()),
        Some(attempt.as_str())
    );
    assert!(step.takeover_grants.is_empty());
    assert!(
        project_state
            .needs_you
            .iter()
            .any(|row| row.id == format!("review:{delivery}")),
        "{:?}",
        project_state.needs_you
    );

    // Asking again on a step in review is refused with the reason.
    let again = env.run_as(
        &b,
        &["ask", "--node", NODE, "--takeover", "Read the delivery."],
    );
    let refused = expect_error(&again, "E_VALIDATION", 2);
    assert!(
        refused["error"]["message"].as_str().is_some_and(
            |text| text.contains("is in review") && text.contains("accept it or send it back")
        ),
        "{refused}"
    );
}

#[test]
fn a_takeover_question_on_a_paused_step_is_refused_with_the_reason() {
    let (env, a, b) = two_agents();
    expect_success(
        &env.run_as(
            &a,
            &["progress", "started", "--node", NODE, "--eta", "20m..40m"],
        ),
        "progress",
    );
    let slug = ProjectSlug::parse("demo").expect("slug");
    let node = NodePath::parse(NODE).expect("node");
    let expected = std::collections::BTreeMap::from([(
        "paused".to_owned(),
        agendar_core::model::feature_field_revision(&slug, &node, "paused", None),
    )]);
    env.service()
        .update_feature(
            &slug,
            &node,
            agendar_core::model::PlanFeature {
                paused: Some(true),
                ..agendar_core::model::PlanFeature::default()
            },
            &expected,
            None,
        )
        .expect("the owner pauses the step");
    let output = env.run_as(
        &b,
        &["ask", "--node", NODE, "--takeover", "Read its start note."],
    );
    let refused = expect_error(&output, "E_VALIDATION", 2);
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("is paused by the owner")),
        "{refused}"
    );
    assert_eq!(env.count_kind("decision.ask"), 0);
}
