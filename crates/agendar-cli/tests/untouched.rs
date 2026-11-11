//! `agendar board` marks a planned step only agents wrote and nobody touched
//! for 14 days as `untouched`; the owner's Keep (`update_feature` with the
//! step's own priority and revision, the same call the app makes) clears it.
//!
//! The agent's declaration is written through the core with a clock 15 days in
//! the past, so the test does not wait. Set `AGENDAR_UNTOUCHED_TRANSCRIPT` to
//! a file path to save the CLI transcript.

mod support;

use std::fmt::Write as _;
use std::fs;

use serde_json::{Value, json};

use agendar_core::model::cards::DemoKind;
use agendar_core::model::event::{Agent, EventPayload, PlanFeature, Refs};
use agendar_core::model::{
    Harness, HostId, NodePath, ProjectSlug, SessionId, UtcTime, WriterId, WriterRole,
    feature_field_revisions,
};
use agendar_core::runtime::LocalPaths;
use agendar_core::vault::{
    Clock, EventDraft, SystemClock, UlidIdSource, Vault, WriterKey, append_event,
};
use support::{CliEnv, expect_success};

const DAY: i64 = 24 * 60 * 60;

struct At(UtcTime);

impl Clock for At {
    fn now(&self) -> UtcTime {
        self.0
    }
}

fn run(env: &CliEnv, log: &mut String, args: &[&str]) -> Value {
    let output = env.run_as(&env.session, args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = writeln!(log, "$ agendar {}\n{}\n", args.join(" "), stdout.trim_end());
    serde_json::from_str(&stdout).unwrap_or(Value::Null)
}

fn step_of<'a>(board: &'a Value, node: &str) -> &'a Value {
    board["data"]["features"]
        .as_array()
        .expect("features")
        .iter()
        .find(|feature| feature["node"] == node)
        .unwrap_or_else(|| panic!("{node} on the board: {board}"))
}

#[test]
fn board_marks_an_untouched_step_and_keep_clears_it() {
    let mut env = CliEnv::new("demo");
    env.init();
    let mut log = String::new();
    let project = env.root.path().join("project.json");
    fs::write(
        &project,
        serde_json::to_vec(&json!({"title": "Untouched demo", "brief": "b", "goal": "g"}))
            .expect("json"),
    )
    .expect("write");
    expect_success(
        &env.run(&["project", "declare", "--json", &project.to_string_lossy()]),
        "project declare",
    );

    let _ = writeln!(
        log,
        "# An agent declared plan/old fifteen days ago and plan/new just now; nobody touched either since.\n"
    );
    let session: SessionId = env.session.parse().expect("session");
    let declare = |node: &str, secs_ago: i64| {
        let vault = Vault::open(&env.vault).expect("vault");
        let local = LocalPaths::new(&env.data).expect("local paths");
        let key = WriterKey {
            project: ProjectSlug::parse("demo").expect("project"),
            writer: WriterId::new(
                HostId::parse(&env.host).expect("host"),
                WriterRole::Agent(session.clone()),
            ),
        };
        let draft = EventDraft {
            refs: Refs {
                node: Some(NodePath::parse(node).expect("node")),
                ..Refs::default()
            },
            payload: EventPayload::PlanFeature(PlanFeature {
                agent: Some(Agent {
                    session: session.clone(),
                    harness: Harness::Generic,
                }),
                title: Some(node.to_owned()),
                slice: Some("s".to_owned()),
                demo_kind: Some(DemoKind::CliTranscript),
                ..PlanFeature::default()
            }),
        };
        let clock = At(SystemClock.now().add_seconds(-secs_ago));
        append_event(&vault, &local, &key, draft, &clock, &mut UlidIdSource).expect("append");
    };
    declare("plan/old", 15 * DAY);
    declare("plan/new", 0);

    let board = run(&env, &mut log, &["board"]);
    assert_eq!(step_of(&board, "plan/old")["untouched"], true, "{board}");
    assert!(step_of(&board, "plan/new").get("untouched").is_none());

    let _ = writeln!(
        log,
        "# The owner presses Keep on plan/old: update_feature with the step's own priority and its revision.\n"
    );
    let service = env.service();
    let slug = ProjectSlug::parse("demo").expect("slug");
    let state = service.export_state(SystemClock.now()).expect("state");
    let feature = &state.projects[&slug].features[&NodePath::parse("plan/old").expect("node")];
    assert!(feature.untouched);
    let revision = feature_field_revisions(&slug, feature)["priority"].clone();
    service
        .update_feature(
            &slug,
            &feature.node,
            PlanFeature {
                priority: Some(feature.priority),
                ..PlanFeature::default()
            },
            &[("priority".to_owned(), revision)].into_iter().collect(),
            None,
        )
        .expect("keep");

    let board = run(&env, &mut log, &["board"]);
    assert!(
        step_of(&board, "plan/old").get("untouched").is_none(),
        "{board}"
    );
    assert_eq!(step_of(&board, "plan/old")["progress"], "planned");

    if let Ok(path) = std::env::var("AGENDAR_UNTOUCHED_TRANSCRIPT") {
        fs::write(path, log).expect("transcript");
    }
}
