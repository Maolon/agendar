//! Real-binary CLI envelopes, validation and the ask/inbox/ack loop.

mod support;

use std::fs;

use serde_json::{Value, json};

use agendar_core::model::{EventId, Harness, OptionId, ProjectSlug};
use agendar_core::runtime::RuntimeState;

use support::{CliEnv, expect_error, expect_raw_json, expect_success};

fn write_json(env: &CliEnv, name: &str, value: &Value) -> String {
    let path = env.root.path().join(name);
    fs::write(&path, serde_json::to_vec(value).expect("serialize fixture")).expect("write json");
    path.to_string_lossy().into_owned()
}

fn project_patch() -> Value {
    json!({
        "title": "Scratch project",
        "brief": "A scratch board for the CLI loop.",
        "goal": "Prove ask, inbox and ack.",
        "glyph": "radar"
    })
}

fn feature_patch() -> Value {
    json!({
        "title": "Wake an idle agent",
        "slice": "core/wake",
        "demo_kind": "cli_transcript"
    })
}

fn prepare(env: &mut CliEnv) {
    env.init();
    let project = write_json(env, "project.json", &project_patch());
    expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    let feature = write_json(env, "feature.json", &feature_patch());
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            "core/wake",
            "--json",
            &feature,
        ]),
        "feature declare",
    );
}

fn ask_card() -> Value {
    let bytes = fs::read(CliEnv::fixture("ask.json")).expect("ask fixture");
    serde_json::from_slice(&bytes).expect("ask fixture JSON")
}

fn ask(env: &CliEnv, node: &str, card: &Value, extra: &[&str]) -> std::process::Output {
    let path = write_json(env, "ask-input.json", card);
    let mut args = vec!["ask", "--node", node, "--mode", "ask", "--json", &path];
    args.extend_from_slice(extra);
    env.run(&args)
}

fn remove_selected_project(env: &CliEnv) {
    let path = env.data.join("config.json");
    let mut config: Value =
        serde_json::from_slice(&fs::read(&path).expect("config")).expect("config json");
    if let Some(object) = config.as_object_mut() {
        object.remove("project");
    }
    fs::write(
        &path,
        serde_json::to_vec(&config).expect("serialize config"),
    )
    .expect("rewrite config");
}

/// Every directory entry below `root` with its bytes, for no-write assertions.
fn tree_snapshot(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &std::path::Path, current: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(current)
            .expect("read directory")
            .flatten()
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("path below root")
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push((relative, Vec::new()));
                walk(root, &path, out);
            } else {
                out.push((relative, fs::read(&path).expect("read file bytes")));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Run `project link` for a chosen data root, Vault flag and Vault environment.
fn link_output(
    env: &CliEnv,
    data_dir: &std::path::Path,
    vault: Option<&std::path::Path>,
    vault_env: Option<&std::path::Path>,
) -> std::process::Output {
    let mut command = env.command();
    if let Some(vault) = vault {
        command.arg("--vault").arg(vault);
    }
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--project", "demo", "project", "link", "--repo"])
        .arg(&env.repo);
    if let Some(vault) = vault_env {
        command.env("AGENDAR_VAULT", vault);
    }
    command.output().expect("run project link")
}

fn error_message(value: &Value) -> String {
    value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn human_answer(env: &CliEnv, ask_id: &str, option: &str) -> String {
    let service = env.service();
    let project = ProjectSlug::parse(&env.project).expect("project");
    let ask = EventId::parse(ask_id).expect("ask id");
    let option = OptionId::parse(option).expect("option");
    service
        .answer_decision(&project, &ask, &option, None, None, None)
        .expect("append human answer")
        .as_str()
        .to_owned()
}

#[test]
fn init_and_declarations_use_the_success_envelope() {
    let mut env = CliEnv::new("demo");
    env.init();
    assert!(!env.session.is_empty());
    assert!(env.host.starts_with("h-"));
    assert!(env.vault.join("vault.json").is_file());
    assert!(env.data.join("identity.json").is_file());

    let project = write_json(&env, "project.json", &project_patch());
    let value = expect_success(
        &env.run(&["project", "declare", "--json", &project]),
        "project declare",
    );
    assert_eq!(value["data"]["project"], "demo");
    assert_eq!(value["freshness"]["cached"], false);
    assert_eq!(
        value["freshness"]["event_set_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let feature_path = write_json(&env, "feature.json", &feature_patch());
    let value = expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            "core/wake",
            "--json",
            &feature_path,
        ]),
        "feature declare",
    );
    assert_eq!(value["data"]["node"], "core/wake");
    assert_eq!(value["data"]["rejected_edges"], json!([]));
}

#[test]
fn progress_commands_and_pulse_cover_every_status() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);

    // A second feature for done, a third for dropped and a fourth for verified.
    for (node, title) in [
        ("core/done", "Done feature"),
        ("core/dropped", "Dropped feature"),
        ("core/verified", "Verified feature"),
    ] {
        let mut patch = feature_patch();
        patch["title"] = json!(title);
        patch["slice"] = json!(node);
        let path = write_json(&env, &format!("{}.json", node.replace('/', "-")), &patch);
        expect_success(
            &env.run(&["feature", "declare", "--node", node, "--json", &path]),
            "feature declare",
        );
    }

    let started = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "10m..20m",
        ]),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    assert_eq!(started["data"]["progress"], "doing");

    let first_pulse = expect_success(
        &env.run(&[
            "pulse",
            "--node",
            "core/wake",
            "--attempt",
            &attempt,
            "--message",
            "first pass",
        ]),
        "pulse",
    );
    assert_eq!(first_pulse["data"]["rate_limited"], false);
    assert!(first_pulse["data"]["event_id"].is_string());

    let rate_limited = expect_success(
        &env.run(&[
            "pulse",
            "--node",
            "core/wake",
            "--attempt",
            &attempt,
            "--message",
            "a second non-boundary pulse",
        ]),
        "pulse",
    );
    assert_eq!(rate_limited["data"]["rate_limited"], true);
    assert!(rate_limited["data"]["event_id"].is_null());
    assert!(rate_limited["data"]["retry_after_s"].as_u64().unwrap_or(0) > 0);

    let pulse = expect_success(
        &env.run(&[
            "pulse",
            "--node",
            "core/wake",
            "--attempt",
            &attempt,
            "--message",
            "contract written",
            "--boundary",
        ]),
        "pulse",
    );
    assert_eq!(pulse["data"]["rate_limited"], false);
    assert!(pulse["data"]["event_id"].is_string());

    expect_success(
        &env.run(&[
            "progress",
            "done",
            "--node",
            "core/wake",
            "--attempt",
            &attempt,
            "--note",
            "finished",
        ]),
        "progress",
    );

    let done = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/done",
            "--eta",
            "1m..2m",
        ]),
        "progress",
    );
    let done_attempt = done["data"]["attempt"].as_str().unwrap().to_owned();
    expect_success(
        &env.run(&[
            "progress",
            "done",
            "--node",
            "core/done",
            "--attempt",
            &done_attempt,
        ]),
        "progress",
    );

    let dropped = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/dropped",
            "--eta",
            "1m..2m",
        ]),
        "progress",
    );
    let dropped_attempt = dropped["data"]["attempt"].as_str().unwrap().to_owned();
    expect_success(
        &env.run(&[
            "progress",
            "dropped",
            "--node",
            "core/dropped",
            "--attempt",
            &dropped_attempt,
        ]),
        "progress",
    );

    let verified = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/verified",
            "--eta",
            "1m..2m",
        ]),
        "progress",
    );
    let verified_attempt = verified["data"]["attempt"].as_str().unwrap().to_owned();
    let evidence = json!({
        "evidence": [{
            "criterion": "continues",
            "status": "passed",
            "claim": "agent_says",
            "evidence": {
                "host": env.host,
                "sha256": "0".repeat(64),
                "ext": "txt",
                "bytes": 3
            }
        }]
    });
    let evidence = write_json(&env, "verified.json", &evidence);
    let verified = expect_success(
        &env.run(&[
            "progress",
            "verified",
            "--node",
            "core/verified",
            "--attempt",
            &verified_attempt,
            "--json",
            &evidence,
        ]),
        "progress",
    );
    assert_eq!(verified["data"]["awaiting_review"], false);
    assert_eq!(verified["data"]["progress"], "verified");
}

#[test]
fn the_owner_can_restart_a_finished_feature() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let first = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "1m..2m",
        ]),
        "progress",
    );
    let first_attempt = first["data"]["attempt"].as_str().unwrap().to_owned();
    expect_success(
        &env.run(&[
            "progress",
            "done",
            "--node",
            "core/wake",
            "--attempt",
            &first_attempt,
        ]),
        "progress",
    );
    let second = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "2m..3m",
        ]),
        "progress",
    );
    let second_attempt = second["data"]["attempt"].as_str().unwrap().to_owned();
    assert_ne!(first_attempt, second_attempt);
    assert_eq!(second["data"]["progress"], "doing");
}

#[test]
fn every_remaining_read_command_returns_a_success_envelope() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);

    let rules = expect_success(&env.run(&["rules"]), "rules");
    assert_eq!(rules["data"]["rules"], json!([]));

    let status = expect_success(&env.run(&["status"]), "status");
    assert!(status["data"]["generation"].as_u64().unwrap_or(0) > 0);
    assert!(status["data"]["files"].is_array());

    let rebuilt = expect_success(&env.run(&["index", "rebuild"]), "index rebuild");
    assert!(rebuilt["data"]["events"].as_u64().unwrap_or(0) >= 2);
    assert!(rebuilt["data"]["generation"].as_u64().is_some());

    let as_of = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let exported = expect_raw_json(&env.run(&["state", "export", "--as-of", &as_of]));
    assert_eq!(exported["projects"]["demo"]["summary"]["slug"], "demo");
    assert!(exported.get("event_set_digest").is_some());

    let skill = env.run(&["skill", "print"]);
    assert_eq!(skill.status.code(), Some(0));
    let text = String::from_utf8_lossy(&skill.stdout);
    assert!(text.starts_with("---\nname: agendar\ndescription:"));
    assert!(text.contains("\n# Agendar"));
    assert!(text.contains("Claude Code without hooks"));
    assert!(text.lines().count() <= 150);

    let info = expect_success(&env.run(&["skill", "info"]), "skill info");
    assert_eq!(info["data"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["data"]["sha256"].as_str().unwrap().len(), 64);

    let steward = expect_success(&env.run(&["steward", "--once"]), "steward");
    assert_eq!(steward["data"]["processed"], 1);
    assert_eq!(steward["data"]["lost"], 0);
    assert!(steward["data"]["capabilities"].is_array());
}

#[test]
fn ask_returns_immediately_and_an_over_limit_card_writes_nothing() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let mut card = ask_card();
    let before = env.count_kind("decision.ask");
    let value = expect_success(&ask(&env, "core/wake", &card, &[]), "ask");
    assert_eq!(value["data"]["effective_mode"], "ask");
    assert_eq!(value["data"]["should_wait"], true);
    assert_eq!(value["data"]["runnable_nodes"], json!([]));
    assert_eq!(value["data"]["waiting_on"].as_array().unwrap().len(), 1);
    assert!(value["data"].get("receipt_id").is_none());
    assert_eq!(env.count_kind("decision.ask"), before + 1);

    let before = env.count_kind("decision.ask");
    card["question"] = json!(
        "This question is deliberately far longer than the soft limit plus its quarter of grace, so the CLI must reject it before any write happens at all."
    );
    let error = expect_error(&ask(&env, "core/wake", &card, &[]), "E_VALIDATION", 2);
    assert!(error["error"]["field"].is_string());
    assert_eq!(env.count_kind("decision.ask"), before);
}

#[test]
fn ask_reports_other_runnable_work_while_the_question_is_open() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let mut other = feature_patch();
    other["title"] = json!("Independent work");
    other["slice"] = json!("core/other");
    let path = write_json(&env, "other.json", &other);
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            "core/other",
            "--json",
            &path,
        ]),
        "feature declare",
    );

    let value = expect_success(&ask(&env, "core/wake", &ask_card(), &[]), "ask");
    assert_eq!(value["data"]["should_wait"], false);
    assert_eq!(value["data"]["runnable_nodes"], json!(["core/other"]));
    assert_eq!(value["data"]["waiting_on"].as_array().unwrap().len(), 1);
}

#[test]
fn irreversible_cards_require_hold_and_never_auto_resolve() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let mut card = ask_card();
    card["reversibility"] = json!("irreversible");
    card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 14400});
    let before = env.count_kind("decision.ask");
    let error = expect_error(&ask(&env, "core/wake", &card, &[]), "E_POLICY", 4);
    assert_eq!(error["error"]["field"], "/on_timeout");
    assert_eq!(env.count_kind("decision.ask"), before);

    // The hard floor escalates an automatic request to ask; it never writes an
    // auto/assumed choice yet.
    card["on_timeout"] = json!({"kind": "hold"});
    let path = write_json(&env, "irreversible.json", &card);
    let value = expect_success(
        &env.run(&[
            "ask",
            "--node",
            "core/wake",
            "--mode",
            "auto",
            "--json",
            &path,
        ]),
        "ask",
    );
    assert_eq!(value["data"]["effective_mode"], "ask");
    assert!(value["data"].get("receipt_id").is_none());
    assert_eq!(env.count_kind("decision.auto"), 0);
    assert_eq!(env.count_kind("decision.assumed"), 0);
    assert_eq!(env.count_kind("decision.ask"), before + 1);
}

#[test]
fn heuristic_keywords_escalate_easy_cards_to_irreversible() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let mut card = ask_card();
    card["question"] = json!("Should we deploy the new wake path?");
    let path = write_json(&env, "heuristic.json", &card);
    let value = expect_success(
        &env.run(&[
            "ask",
            "--node",
            "core/wake",
            "--mode",
            "auto",
            "--json",
            &path,
        ]),
        "ask",
    );
    assert_eq!(value["data"]["effective_mode"], "ask");
    assert_eq!(value["data"]["policy"]["reversibility"], "irreversible");
    assert_eq!(value["data"]["policy"]["heuristic_hits"], json!(["deploy"]));
}

#[test]
fn inbox_never_acks_and_ack_commits_the_current_answer() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let card = ask_card();
    let asked = expect_success(&ask(&env, "core/wake", &card, &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().unwrap().to_owned();
    let answer = human_answer(&env, &ask_id, "adapters");

    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    let packets = inbox["data"]["packets"].as_array().expect("packets");
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0]["ask"], ask_id);
    assert_eq!(packets[0]["answer"], answer);
    assert_eq!(packets[0]["receipt"]["chose"]["id"], "adapters");
    assert_eq!(env.count_kind("decision.ack"), 0);

    // A repeated inbox still delivers the packet; it never acks.
    let repeated = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(repeated["data"]["packets"].as_array().unwrap().len(), 1);
    assert_eq!(env.count_kind("decision.ack"), 0);

    let acked = expect_success(&env.run(&["ack", &answer]), "ack");
    assert_eq!(acked["data"]["ask_id"], ask_id);
    assert_eq!(acked["data"]["answer_id"], answer);
    assert_eq!(acked["data"]["lifecycle"], "acked");
    assert_eq!(env.count_kind("decision.ack"), 1);

    let after = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(after["data"]["packets"], json!([]));
}

#[test]
fn withdraw_closes_an_unacked_ask() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let card = ask_card();
    let asked = expect_success(&ask(&env, "core/wake", &card, &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().unwrap().to_owned();
    let withdrawn = expect_success(
        &env.run(&["withdraw", &ask_id, "--reason", "answered by the spike"]),
        "withdraw",
    );
    assert_eq!(withdrawn["data"]["ask_id"], ask_id);
    assert_eq!(withdrawn["data"]["lifecycle"], "withdrawn");

    let again = expect_error(
        &env.run(&["withdraw", &ask_id, "--reason", "again"]),
        "E_WITHDRAWN",
        8,
    );
    assert!(
        again["error"]["message"]
            .as_str()
            .unwrap()
            .contains("withdrawn")
    );
}

#[test]
fn ack_rejects_a_stale_or_foreign_answer() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let card = ask_card();
    let asked = expect_success(&ask(&env, "core/wake", &card, &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().unwrap().to_owned();
    let first = human_answer(&env, &ask_id, "inbox");
    let second = human_answer(&env, &ask_id, "adapters");

    // The later answer is the current one before any ack.
    let error = expect_error(&env.run(&["ack", &first]), "E_STALE_ANSWER", 8);
    assert!(error["error"]["hint"].is_string());
    let acked = expect_success(&env.run(&["ack", &second]), "ack");
    assert_eq!(acked["data"]["answer_id"], second);
}

#[test]
fn error_cases_use_the_stable_envelope_and_exit_codes() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);

    // Missing project context: the selected project is cleared and the command
    // runs outside every linked repository, so only the repo link could supply
    // one.
    remove_selected_project(&env);
    let outside = env.root.path().join("outside");
    fs::create_dir_all(&outside).expect("outside directory");
    let mut command = env.command();
    command
        .current_dir(&outside)
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("rules");
    let output = command.output().expect("run without project");
    expect_error(&output, "E_CONTEXT", 3);
    let missing = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        missing.contains("project link"),
        "the missing-project error names --project and the link command: {missing}"
    );

    // Unknown feature.
    let output = env.run(&[
        "progress",
        "started",
        "--node",
        "core/nope",
        "--eta",
        "1m..2m",
    ]);
    expect_error(&output, "E_NOT_FOUND", 3);

    // Malformed JSON input.
    let path = env.root.path().join("broken.json");
    fs::write(&path, b"{not json").expect("write broken json");
    let output = env.run(&[
        "ask",
        "--node",
        "core/wake",
        "--mode",
        "ask",
        "--json",
        path.to_str().unwrap(),
    ]);
    expect_error(&output, "E_VALIDATION", 2);

    // The long-running steward is implemented: it emits one startup line and
    // then runs until a signal, so this error case no longer applies.

    // Wrong attempt.
    let started = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "1m..2m",
        ]),
        "progress",
    );
    let attempt = started["data"]["attempt"].as_str().unwrap().to_owned();
    let unknown = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let output = env.run(&[
        "progress",
        "done",
        "--node",
        "core/wake",
        "--attempt",
        unknown,
    ]);
    let error = expect_error(&output, "E_VALIDATION", 2);
    assert_eq!(error["error"]["field"], "/attempt");
    assert_ne!(attempt, unknown);
}

#[test]
fn clap_parse_errors_use_the_usage_envelope() {
    let env = CliEnv::new("demo");
    let output = env
        .command()
        .arg("--definitely-not-a-flag")
        .output()
        .expect("run bad command");
    let error = expect_error(&output, "E_USAGE", 2);
    assert_eq!(error["error"]["field"], "/args");
}

#[test]
fn request_id_retries_are_idempotent_and_conflicts_are_rejected() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let card = ask_card();
    let path = write_json(&env, "ask-input.json", &card);
    let request_id = ulid::Ulid::generate().to_string();
    let before = env.count_kind("decision.ask");
    let first = expect_success(
        &env.run(&[
            "ask",
            "--node",
            "core/wake",
            "--mode",
            "ask",
            "--json",
            &path,
            "--request-id",
            &request_id,
        ]),
        "ask",
    );
    let retry = expect_success(
        &env.run(&[
            "ask",
            "--node",
            "core/wake",
            "--mode",
            "ask",
            "--json",
            &path,
            "--request-id",
            &request_id,
        ]),
        "ask",
    );
    assert_eq!(first["data"]["ask_id"], retry["data"]["ask_id"]);
    assert_eq!(env.count_kind("decision.ask"), before + 1);

    // Same request id with different input is a conflict, not a second event.
    let mut changed = card.clone();
    changed["why_now"] = json!("A different input for the same request id.");
    let changed_path = write_json(&env, "changed.json", &changed);
    let output = env.run(&[
        "ask",
        "--node",
        "core/wake",
        "--mode",
        "ask",
        "--json",
        &changed_path,
        "--request-id",
        &request_id,
    ]);
    expect_error(&output, "E_REQUEST_CONFLICT", 6);
    assert_eq!(env.count_kind("decision.ask"), before + 1);
}

#[test]
fn project_link_resolves_the_project_from_the_session_directory() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    // The selected config project is not what resolves commands anymore; the
    // durable repo link is.
    remove_selected_project(&env);

    let nested = env.repo.join("crates").join("app");
    fs::create_dir_all(&nested).expect("nested repo directory");
    let mut command = env.command();
    command
        .current_dir(&nested)
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("rules");
    let output = command.output().expect("run from the linked repo");
    expect_success(&output, "rules");

    // A directory outside every link is a clear context error that names the
    // link command.
    let outside = env.root.path().join("outside");
    fs::create_dir_all(&outside).expect("outside directory");
    let mut command = env.command();
    command
        .current_dir(&outside)
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("rules");
    let output = command.output().expect("run outside every link");
    expect_error(&output, "E_CONTEXT", 3);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("project link"),
        "the error names the link command"
    );

    // The explicit link command records another repository in local config and
    // does not need an initialized Vault.
    let other = env.root.path().join("other-repo");
    fs::create_dir_all(&other).expect("other repo");
    let mut command = env.command();
    command
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg("demo")
        .arg("project")
        .arg("link")
        .arg("--repo")
        .arg(&other);
    let output = command.output().expect("project link");
    let value = expect_success(&output, "project link");
    assert_eq!(value["data"]["project"], "demo");

    let mut command = env.command();
    command
        .current_dir(&other)
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("rules");
    expect_success(
        &command.output().expect("run in the newly linked repo"),
        "rules",
    );

    // A nested link wins over the parent link: the CLI resolves `nested`, which
    // does not exist in the board yet, instead of quietly using `demo`.
    let nested = other.join("nested");
    fs::create_dir_all(&nested).expect("nested repo");
    let mut command = env.command();
    command
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg("nested")
        .arg("project")
        .arg("link")
        .arg("--repo")
        .arg(&nested);
    expect_success(&command.output().expect("nested link"), "project link");
    let mut command = env.command();
    command
        .current_dir(&nested)
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("rules");
    expect_error(
        &command.output().expect("run in the nested repo"),
        "E_NOT_FOUND",
        3,
    );
}

#[test]
fn project_link_rejects_vault_overlap_before_writing_local_state() {
    let mut env = CliEnv::new("demo");
    env.init();
    let vault_before = tree_snapshot(&env.vault);

    // Equality: the data root is the selected Vault itself (the reviewed bug).
    let value = expect_error(
        &link_output(&env, &env.vault, Some(&env.vault), None),
        "E_VALIDATION",
        2,
    );
    assert!(
        error_message(&value).contains("different directories"),
        "equality is reported as such: {value}"
    );
    assert_eq!(
        tree_snapshot(&env.vault),
        vault_before,
        "an equal link leaves every Vault byte unchanged"
    );

    // The data root inside the Vault.
    let inside = env.vault.join("link-data");
    let value = expect_error(
        &link_output(&env, &inside, Some(&env.vault), None),
        "E_VALIDATION",
        2,
    );
    assert!(
        error_message(&value).contains("inside the Vault"),
        "the nesting is reported: {value}"
    );
    assert!(
        !inside.exists(),
        "no data directory is created inside the Vault"
    );
    assert_eq!(
        tree_snapshot(&env.vault),
        vault_before,
        "a nested data root leaves every Vault byte unchanged"
    );

    // The same nesting selected through AGENDAR_VAULT instead of --vault.
    let value = expect_error(
        &link_output(&env, &inside, None, Some(&env.vault)),
        "E_VALIDATION",
        2,
    );
    assert!(error_message(&value).contains("inside the Vault"));
    assert!(!inside.exists());
    assert_eq!(tree_snapshot(&env.vault), vault_before);

    // The Vault inside the data root (the flag wins over the config Vault).
    let selected = env.data.join("selected-vault");
    let data_before = tree_snapshot(&env.data);
    let value = expect_error(
        &link_output(&env, &env.data, Some(&selected), None),
        "E_VALIDATION",
        2,
    );
    assert!(
        error_message(&value).contains("The Vault must not be inside"),
        "the inverse nesting is reported: {value}"
    );
    assert!(
        !selected.exists(),
        "no Vault is created below the data root"
    );
    assert_eq!(
        tree_snapshot(&env.data),
        data_before,
        "a rejected link does not rewrite the local config"
    );

    // The Vault selected only by the local configuration.
    let configured = env.root.path().join("configured-data");
    fs::create_dir_all(&configured).expect("configured data root");
    let selected_by_config = configured.join("vault-inside");
    fs::write(
        configured.join("config.json"),
        serde_json::to_vec(&json!({
            "v": 1,
            "revision": 1,
            "vault": selected_by_config.to_str().expect("utf-8 path"),
        }))
        .expect("serialize config"),
    )
    .expect("write config");
    let configured_before = tree_snapshot(&configured);
    let value = expect_error(
        &link_output(&env, &configured, None, None),
        "E_VALIDATION",
        2,
    );
    assert!(error_message(&value).contains("The Vault must not be inside"));
    assert!(!selected_by_config.exists());
    assert_eq!(
        tree_snapshot(&configured),
        configured_before,
        "a rejected link leaves the selecting config untouched"
    );

    // A data root that is itself an existing Vault, with no Vault selected.
    let value = expect_error(
        &link_output(&env, &env.vault, None, None),
        "E_VALIDATION",
        2,
    );
    assert!(
        error_message(&value).contains("must not be a Vault"),
        "an existing Vault is never a data root: {value}"
    );
    assert!(!env.vault.join("config.json").exists());
    assert!(!env.vault.join("config.lock").exists());
    assert_eq!(
        tree_snapshot(&env.vault),
        vault_before,
        "no local config is created in the Vault"
    );
}

#[test]
fn skill_print_and_info_work_before_any_vault_exists() {
    // A fresh machine: temporary HOME, no Vault, no local config.
    let env = CliEnv::new("demo");
    let home_before = tree(&env.home);
    let print = env
        .command()
        .args(["skill", "print"])
        .output()
        .expect("run skill print");
    assert_eq!(print.status.code(), Some(0), "skill print needs no Vault");
    let text = String::from_utf8_lossy(&print.stdout);
    assert!(text.starts_with("---\nname: agendar\ndescription:"));

    let info = env
        .command()
        .args(["skill", "info"])
        .output()
        .expect("run skill info");
    let value = expect_success(&info, "skill info");
    assert_eq!(value["data"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        tree(&env.home),
        home_before,
        "reading the skill writes no local state"
    );
}

/// Every path under `root`, relative and sorted.
fn tree(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            out.push(path.strip_prefix(root).expect("relative").to_path_buf());
            if path.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn project_link_accepts_disjoint_paths_and_no_vault() {
    let mut env = CliEnv::new("demo");
    env.init();
    let vault_before = tree_snapshot(&env.vault);

    // A disjoint data root with the Vault selected explicitly.
    let disjoint = env.root.path().join("disjoint-data");
    let value = expect_success(
        &link_output(&env, &disjoint, Some(&env.vault), None),
        "project link",
    );
    assert_eq!(value["data"]["project"], "demo");
    assert!(disjoint.join("config.json").is_file());
    assert_eq!(
        tree_snapshot(&env.vault),
        vault_before,
        "linking writes no local state in the Vault"
    );

    // No Vault selected at all: linking stays valid before `init`.
    let fresh = env.root.path().join("fresh-data");
    let value = expect_success(&link_output(&env, &fresh, None, None), "project link");
    assert_eq!(value["data"]["project"], "demo");
    assert!(fresh.join("config.json").is_file());
    assert!(
        !fresh.join("vault.json").exists(),
        "linking does not create a Vault"
    );

    // A selected Vault that does not exist yet: setup runs before `init`.
    let pre_init_data = env.root.path().join("pre-init-data");
    let pre_init_vault = env.root.path().join("pre-init-vault");
    let value = expect_success(
        &link_output(&env, &pre_init_data, Some(&pre_init_vault), None),
        "project link",
    );
    assert_eq!(value["data"]["project"], "demo");
    assert!(
        !pre_init_vault.exists(),
        "linking never creates the selected Vault"
    );
}

#[test]
fn init_uses_the_codex_thread_id_from_the_environment() {
    let env = CliEnv::new("demo");
    let pid = std::process::id().to_string();
    let repo = env.repo.to_string_lossy().into_owned();
    let register = ["init", "--harness", "codex", "--pid", &pid, "--repo", &repo];

    // Without --harness-session and without CODEX_THREAD_ID the registration is
    // refused with a hint that names the variable.
    let missing = env.run(&register);
    let value = expect_error(&missing, "E_USAGE", 2);
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("CODEX_THREAD_ID"),
        "the hint names CODEX_THREAD_ID: {value}"
    );

    // Codex sets CODEX_THREAD_ID inside every session; `init --harness codex`
    // uses it as the durable thread id.
    let output = env.run_with_env(&register, &[("CODEX_THREAD_ID", "thread-from-env")]);
    let value = expect_success(&output, "init");
    let session = value["data"]["session"]
        .as_str()
        .expect("registered session")
        .to_owned();
    let service = env.service();
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let session_id = agendar_core::model::SessionId::parse(&session).expect("session id");
    let record = runtime.sessions.get(&session_id).expect("registration");
    assert_eq!(record.harness, Harness::Codex);
    assert_eq!(record.harness_session_id, "thread-from-env");

    // An explicit --harness-session always wins over the environment.
    let explicit = env.run_with_env(
        &[
            "init",
            "--harness",
            "codex",
            "--harness-session",
            "thread-from-flag",
            "--pid",
            &pid,
            "--repo",
            &repo,
        ],
        &[("CODEX_THREAD_ID", "thread-from-env")],
    );
    let value = expect_success(&explicit, "init");
    let explicit_session = value["data"]["session"]
        .as_str()
        .expect("registered session")
        .to_owned();
    assert_ne!(
        explicit_session, session,
        "a different thread is a new session"
    );
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let explicit_id = agendar_core::model::SessionId::parse(&explicit_session).expect("session id");
    assert_eq!(
        runtime
            .sessions
            .get(&explicit_id)
            .expect("registration")
            .harness_session_id,
        "thread-from-flag"
    );

    // A generic registration still requires an explicit harness session.
    let generic = env.run(&[
        "init",
        "--harness",
        "generic",
        "--pid",
        &pid,
        "--repo",
        &repo,
    ]);
    let value = expect_error(&generic, "E_USAGE", 2);
    assert!(
        !value["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("CODEX_THREAD_ID"),
        "only Codex reads the thread id from the environment"
    );
}

#[test]
fn skill_only_claude_init_returns_a_reusable_context_prefix() {
    let env = CliEnv::new("demo");
    let pid = std::process::id().to_string();
    let repo = env.repo.to_string_lossy().into_owned();
    let request_id = ulid::Ulid::generate().to_string();
    let init = [
        "init",
        "--harness",
        "claude_code",
        "--pid",
        &pid,
        "--repo",
        &repo,
        "--request-id",
        &request_id,
    ];
    let output = env.run(&init);
    let value = expect_success(&output, "init");
    let session = value["data"]["session"].as_str().expect("session");
    let prefix = value["data"]["context_prefix"]
        .as_str()
        .expect("context prefix");
    assert_eq!(value["data"]["project"], "demo");
    assert!(prefix.contains("--vault"));
    assert!(prefix.contains("--data-dir"));
    assert!(prefix.contains(&format!(
        "--project {}",
        agendar_core::service::shell_quote("demo")
    )));
    assert!(prefix.contains(&format!(
        "--session {}",
        agendar_core::service::shell_quote(session)
    )));

    let service = env.service();
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    let session_id = agendar_core::model::SessionId::parse(session).expect("session id");
    let record = runtime.sessions.get(&session_id).expect("registration");
    assert_eq!(record.harness, Harness::ClaudeCode);
    assert_eq!(record.harness_session_id, request_id);

    let repeated = env.run(&init);
    let repeated = expect_success(&repeated, "init");
    assert_eq!(repeated["data"]["session"].as_str(), Some(session));

    // Without --pid, Skill-only Claude discovers the real harness ancestry.
    let harness_name = std::env::current_exe()
        .expect("test executable")
        .file_name()
        .expect("executable name")
        .to_string_lossy()
        .into_owned();
    let automatic = env.run_with_env(
        &["init", "--harness", "claude_code", "--repo", &repo],
        &[("AGENDAR_HARNESS_EXE", &harness_name)],
    );
    let value = expect_success(&automatic, "init");
    let automatic_session = value["data"]["session"].as_str().expect("auto session");
    assert!(value["data"]["context_prefix"].is_string());
    let automatic_id =
        agendar_core::model::SessionId::parse(automatic_session).expect("auto session id");
    let runtime = RuntimeState::load(service.local(), service.vault()).expect("runtime");
    assert_eq!(
        runtime
            .sessions
            .get(&automatic_id)
            .expect("auto registration")
            .pid,
        std::process::id()
    );
}

#[test]
fn a_long_running_steward_emits_one_startup_line_then_runs() {
    let mut env = CliEnv::new("demo");
    env.init();
    let mut child = env
        .command()
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg(&env.project)
        .arg("steward")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn steward");
    let stdout = child.stdout.take().expect("steward stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut line = String::new();
    use std::io::BufRead as _;
    reader.read_line(&mut line).expect("startup line");
    let value: Value = serde_json::from_str(line.trim()).expect("startup JSON");
    assert_eq!(value["v"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "steward");
    assert_eq!(value["data"]["delivered"], 0);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn board_shows_an_arriving_agent_the_declared_project_and_what_it_owns() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let board = expect_success(&env.run(&["board"]), "board");
    assert_eq!(
        board["data"]["project"]["declared"], true,
        "declared once, never again"
    );
    assert_eq!(board["data"]["yours"], json!([]), "nothing started yet");
    let wake = board["data"]["features"]
        .as_array()
        .expect("features")
        .iter()
        .find(|feature| feature["node"] == "core/wake")
        .expect("core/wake")
        .clone();
    assert_eq!(wake["owner"], Value::Null);

    expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "10m..20m",
        ]),
        "progress",
    );
    let board = expect_success(&env.run(&["board"]), "board");
    assert_eq!(board["data"]["yours"], json!(["core/wake"]));
    let wake = board["data"]["features"]
        .as_array()
        .expect("features")
        .iter()
        .find(|feature| feature["node"] == "core/wake")
        .expect("core/wake")
        .clone();
    assert_eq!(wake["owner"]["you"], true);
    assert_eq!(board["data"]["nudges"], json!([]), "inside its ETA");
}

#[test]
fn board_lists_steps_to_reconcile_and_a_never_started_step_drops_without_an_attempt() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let feature = write_json(&env, "spare.json", &feature_patch());
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            "core/spare",
            "--json",
            &feature,
        ]),
        "feature declare",
    );
    let board = expect_success(&env.run(&["board"]), "board");
    let open: Vec<&str> = board["data"]["reconcile"]
        .as_array()
        .expect("reconcile")
        .iter()
        .map(|step| step["node"].as_str().expect("node"))
        .collect();
    assert!(open.contains(&"core/wake"), "{board}");

    let started = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "10m..20m",
        ]),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let board = expect_success(&env.run(&["board"]), "board");
    let first = &board["data"]["reconcile"][0];
    assert_eq!(first["node"], "core/wake", "running steps come first");
    assert_eq!(first["owner"], "you");

    // A started step needs its attempt to be dropped.
    let refused = env.run(&["progress", "dropped", "--node", "core/wake"]);
    let refused = expect_error(&refused, "E_VALIDATION", 2);
    assert_eq!(refused["error"]["field"], "/attempt");
    assert!(
        refused["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(&attempt))
    );

    // A planned step nobody started leaves the plan with no attempt.
    let spare = &board["data"]["reconcile"][1];
    assert_eq!(spare["node"], "core/spare");
    assert_eq!(spare["owner"], "nobody");
    let dropped = expect_success(
        &env.run(&[
            "progress",
            "dropped",
            "--node",
            "core/spare",
            "--note",
            "not needed",
        ]),
        "progress",
    );
    assert_eq!(dropped["data"]["progress"], "dropped");
    let board = expect_success(&env.run(&["board"]), "board");
    let open: Vec<&str> = board["data"]["reconcile"]
        .as_array()
        .expect("reconcile")
        .iter()
        .map(|step| step["node"].as_str().expect("node"))
        .collect();
    assert_eq!(open, ["core/wake"], "a dropped step is settled");
}

#[test]
fn an_overdue_eta_shows_as_a_nudge_in_inbox_and_board_until_re_estimated() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(inbox["data"]["nudges"], json!([]));
    let started = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "1s..1s",
        ]),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    std::thread::sleep(std::time::Duration::from_millis(2100));

    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    let nudges = inbox["data"]["nudges"].as_array().expect("nudges");
    assert_eq!(nudges.len(), 1, "{inbox}");
    assert_eq!(nudges[0]["kind"], "eta_overdue");
    assert_eq!(nudges[0]["node"], "core/wake");
    assert_eq!(nudges[0]["attempt"], attempt.as_str());
    assert!(nudges[0]["late_by_s"].as_u64().expect("late") >= 1);
    let board = expect_success(&env.run(&["board"]), "board");
    // Same nudge; its age may have ticked a second between the two reads.
    for field in ["kind", "node", "attempt", "due_at"] {
        assert_eq!(
            board["data"]["nudges"][0][field], nudges[0][field],
            "{field}"
        );
    }
    let other_node = expect_success(&env.run(&["inbox", "--node", "core/other"]), "inbox");
    assert_eq!(
        other_node["data"]["nudges"],
        json!([]),
        "node filter applies"
    );

    expect_success(
        &env.run(&[
            "pulse",
            "--node",
            "core/wake",
            "--attempt",
            &attempt,
            "--eta",
            "10m..20m",
            "--message",
            "re-estimated",
            "--boundary",
        ]),
        "pulse",
    );
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(
        inbox["data"]["nudges"],
        json!([]),
        "a re-estimate clears it"
    );
}

#[test]
fn schema_prints_the_exact_input_shapes_without_a_vault() {
    let env = CliEnv::new("demo");
    let card = expect_success(&env.run(&["schema", "card"]), "schema");
    assert_eq!(card["data"]["limits"]["why_now"], 140);
    assert_eq!(card["data"]["limits"]["options[].consequence"], 130);
    let icons = card["data"]["enums"]["options[].icon"]
        .as_array()
        .expect("icons");
    assert!(icons.iter().any(|icon| icon == "radio"));
    // The example is a card the CLI accepts as is.
    assert!(
        card["data"]["example"]["options"]
            .as_array()
            .expect("options")
            .len()
            >= 2
    );
    let all = expect_success(&env.run(&["schema"]), "schema");
    for kind in ["card", "feature", "project", "eta"] {
        assert!(all["data"][kind].is_object(), "{kind}");
    }
    assert_ne!(env.run(&["schema", "nope"]).status.code(), Some(0));
}

#[test]
fn a_readable_request_id_replays_on_retry_and_is_scoped_to_its_session() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let first_session = env.session.clone();
    let feature = write_json(&env, "spare.json", &feature_patch());
    let declare = |env: &CliEnv, session: &str, node: &str| {
        env.run_as(
            session,
            &[
                "feature",
                "declare",
                "--node",
                node,
                "--json",
                &feature,
                "--request-id",
                "plan-1",
            ],
        )
    };
    let before = env.count_kind("plan.feature");
    let first = expect_success(&declare(&env, &first_session, "core/a"), "feature declare");
    let retry = expect_success(&declare(&env, &first_session, "core/a"), "feature declare");
    assert_eq!(
        first["data"], retry["data"],
        "the retry replays the committed result"
    );
    assert_eq!(env.count_kind("plan.feature"), before + 1);

    // Another agent picking the same key is a different request.
    env.init();
    let second_session = env.session.clone();
    assert_ne!(first_session, second_session);
    expect_success(&declare(&env, &second_session, "core/b"), "feature declare");
    assert_eq!(env.count_kind("plan.feature"), before + 2);

    let bad = env.run_as(
        &first_session,
        &["pulse", "--message", "x", "--request-id", "two words"],
    );
    expect_error(&bad, "E_USAGE", 2);
}

#[test]
fn an_answered_step_starts_with_applies_before_its_ack() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let asked = expect_success(&ask(&env, "core/wake", &ask_card(), &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask").to_owned();
    let answer = human_answer(&env, &ask_id, "adapters");

    // Without citing the answer the start is refused, and the hint says how.
    let refused = expect_error(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "10m..20m",
        ]),
        "E_POLICY",
        4,
    );
    assert_eq!(
        refused["error"]["message"],
        "An answer for this step is not applied yet."
    );
    assert!(
        refused["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(&format!("--applies {answer}"))),
        "{refused}"
    );

    // Citing it starts the work that applies it; the ack follows.
    let started = expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "10m..20m",
            "--applies",
            &answer,
        ]),
        "progress",
    );
    assert_eq!(started["data"]["progress"], "doing");
    expect_success(&env.run(&["ack", &answer]), "ack");

    // Only a current answer can be cited.
    let bogus = env.run(&[
        "progress",
        "started",
        "--node",
        "core/wake",
        "--eta",
        "10m..20m",
        "--applies",
        &ask_id,
    ]);
    assert_ne!(bogus.status.code(), Some(0));
}

#[test]
fn a_human_plan_edit_reaches_each_agent_once_through_inbox() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(
        inbox["data"]["nudges"],
        json!([]),
        "nothing changed since it joined"
    );

    let service = env.service();
    let project = ProjectSlug::parse(&env.project).expect("project");
    let now = agendar_core::model::UtcTime::parse("2099-01-01T00:00:00.000Z").expect("time");
    let state = service.export_state(now).expect("state");
    let summary = &state.projects[&project].summary;
    let revisions = agendar_core::model::state::project_field_revisions(&project, summary);
    let expected: std::collections::BTreeMap<_, _> =
        [("goal".to_owned(), revisions["goal"].clone())]
            .into_iter()
            .collect();
    let goal: agendar_core::model::ProjectDeclared =
        serde_json::from_value(json!({"goal": "Ship only the wake path first."})).expect("patch");
    service
        .update_project(&project, goal, &expected)
        .expect("human edits the goal");

    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    let nudges = inbox["data"]["nudges"].as_array().expect("nudges");
    assert_eq!(nudges.len(), 1, "{inbox}");
    assert_eq!(nudges[0]["kind"], "plan_changed");
    assert!(
        nudges[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Ship only the wake path first.")),
        "{inbox}"
    );
    let again = expect_success(&env.run(&["inbox"]), "inbox");
    assert_eq!(again["data"]["nudges"], json!([]), "shown once per session");

    let wake = agendar_core::model::NodePath::parse("core/wake").expect("node");
    let state = service.export_state(now).expect("state");
    let feature = &state.projects[&project].features[&wake];
    let revisions = agendar_core::model::state::feature_field_revisions(&project, feature);
    let expected: std::collections::BTreeMap<_, _> =
        [("paused".to_owned(), revisions["paused"].clone())]
            .into_iter()
            .collect();
    let pause: agendar_core::model::PlanFeature =
        serde_json::from_value(json!({"paused": true})).expect("patch");
    service
        .update_feature(&project, &wake, pause, &expected, None)
        .expect("human pauses the step");
    let inbox = expect_success(&env.run(&["inbox"]), "inbox");
    let nudges = inbox["data"]["nudges"].as_array().expect("nudges");
    assert_eq!(nudges.len(), 1);
    assert_eq!(nudges[0]["node"], "core/wake");
    assert!(
        nudges[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("paused"))
    );
}

#[test]
fn an_agent_sets_the_project_icon_from_a_png_file() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    // A 1x1 PNG.
    let png: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let icon = env.root.path().join("icon.png");
    fs::write(&icon, png).expect("png");
    let patch = write_json(
        &env,
        "icon.json",
        &json!({"icon": {"file": icon.to_string_lossy(), "ext": "png"}}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &patch]),
        "project declare",
    );
    let service = env.service();
    let now = agendar_core::model::UtcTime::parse("2099-01-01T00:00:00.000Z").expect("time");
    let state = service.export_state(now).expect("state");
    let summary = &state.projects[&ProjectSlug::parse("demo").expect("slug")].summary;
    let set = summary.icon.as_ref().expect("icon set");
    assert_eq!(set.ext.as_str(), "png");

    let text = env.root.path().join("icon.txt");
    fs::write(&text, "not an image").expect("txt");
    let bad = write_json(
        &env,
        "bad.json",
        &json!({"icon": {"file": text.to_string_lossy(), "ext": "txt"}}),
    );
    assert_ne!(
        env.run(&["project", "declare", "--json", &bad])
            .status
            .code(),
        Some(0),
        "only images"
    );
}

#[test]
fn a_report_publishes_its_document_and_a_summary() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let html = env.root.path().join("wake-options.html");
    fs::write(
        &html,
        "<!doctype html><h1>Wake options</h1><p>Three ways compared.</p>",
    )
    .expect("html");
    let html_path = html.to_string_lossy().into_owned();
    let published = expect_success(
        &env.run(&[
            "report",
            "--title",
            "Wake options compared",
            "--summary",
            "Hooks wake fastest; inbox polling is simplest.\nDetails in the report.",
            "--file",
            &html_path,
            "--node",
            "core/wake",
        ]),
        "report",
    );
    assert_eq!(published["data"]["report"]["ext"], "html");
    let service = env.service();
    let now = agendar_core::model::UtcTime::parse("2099-01-01T00:00:00.000Z").expect("time");
    let state = service.export_state(now).expect("state");
    let project = &state.projects[&ProjectSlug::parse("demo").expect("slug")];
    assert_eq!(project.reports.len(), 1);
    assert_eq!(project.reports[0].title, "Wake options compared");
    assert_eq!(
        project.reports[0].node.as_ref().map(|node| node.as_str()),
        Some("core/wake")
    );

    let png = env.root.path().join("x.png");
    fs::write(&png, [0x89, 0x50, 0x4e, 0x47]).expect("png");
    let refused = env.run(&[
        "report",
        "--title",
        "Not a document",
        "--summary",
        "x",
        "--file",
        &png.to_string_lossy(),
    ]);
    assert_ne!(refused.status.code(), Some(0), "a report is a document");
}

#[test]
fn wait_rides_out_a_briefly_held_lock_instead_of_failing_busy() {
    use fs2::FileExt as _;
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let asked = expect_success(&ask(&env, "core/wake", &ask_card(), &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask").to_owned();
    let answer = human_answer(&env, &ask_id, "adapters");

    let vault_dir = fs::read_dir(env.data.join("vaults"))
        .expect("vaults")
        .flatten()
        .next()
        .expect("one vault")
        .path();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(vault_dir.join("locks").join("runtime.lock"))
        .expect("runtime lock file");
    lock.lock_exclusive().expect("hold runtime lock");
    // Held past the 5 s lock wait, then released.
    let holder = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(6500));
        fs2::FileExt::unlock(&lock).expect("unlock");
    });
    let waited = expect_success(&env.run(&["wait", &ask_id, "--timeout", "30s"]), "wait");
    holder.join().expect("holder");
    assert_eq!(waited["data"]["packet"]["answer"], answer.as_str());
}

#[test]
fn an_svg_is_accepted_as_a_project_icon_and_nowhere_else() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let svg = env.root.path().join("logo.svg");
    fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8" fill="#f80"/></svg>"##,
    )
    .expect("svg");
    let svg_path = svg.to_string_lossy().into_owned();
    let patch = write_json(
        &env,
        "icon.json",
        &json!({"icon": {"file": svg_path, "ext": "svg"}}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &patch]),
        "project declare",
    );
    let service = env.service();
    let now = agendar_core::model::UtcTime::parse("2099-01-01T00:00:00.000Z").expect("time");
    let state = service.export_state(now).expect("state");
    let summary = &state.projects[&ProjectSlug::parse("demo").expect("slug")].summary;
    assert_eq!(summary.icon.as_ref().expect("icon").ext.as_str(), "svg");

    let attachment = env.run(&["blob", "put", "--file", &svg_path, "--ext", "svg"]);
    assert_ne!(
        attachment.status.code(),
        Some(0),
        "an svg is not an attachment"
    );
}

#[test]
fn an_answer_applied_elsewhere_leaves_unrelated_inboxes() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let pid = std::process::id().to_string();
    let repo = env.repo.to_string_lossy().into_owned();
    let other = expect_success(
        &env.run(&[
            "init",
            "--harness",
            "codex",
            "--pid",
            &pid,
            "--repo",
            &repo,
            "--harness-session",
            "other-agent",
        ]),
        "init",
    );
    let other = other["data"]["session"]
        .as_str()
        .expect("other session")
        .to_owned();
    let asked = expect_success(&ask(&env, "core/wake", &ask_card(), &[]), "ask");
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask").to_owned();
    let answer = human_answer(&env, &ask_id, "adapters");
    let packets = |session: &str| -> usize {
        let value = expect_success(&env.run_as(session, &["inbox"]), "inbox");
        value["data"]["packets"].as_array().map_or(0, Vec::len)
    };
    assert_eq!(packets(&other), 1, "unapplied, any session may pick it up");
    expect_success(&env.run(&["ack", &answer]), "ack");
    assert_eq!(packets(&env.session), 0, "applied by the asker");
    assert_eq!(
        packets(&other),
        0,
        "settled elsewhere, not this session's step"
    );
}

#[test]
fn the_mcp_server_speaks_mcp_and_runs_tools_as_its_own_session() {
    use std::io::Write as _;
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    let mut command = env.command();
    command
        .args(["mcp", "--harness", "pi"])
        .env("AGENDAR_VAULT", &env.vault)
        .env("AGENDAR_DATA_DIR", &env.data)
        .env("AGENDAR_HARNESS_SESSION", "pi-session-1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn mcp");
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "pi", "version": "1.5.0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "start", "arguments": {"node": "core/wake", "eta": "30m..1h"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "start", "arguments": {"node": "core/wake"}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}),
    ];
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for message in &messages {
            writeln!(stdin, "{message}").expect("write");
            // Let the start call finish before the next one reads the board.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    let output = child.wait_with_output().expect("mcp");
    let responses: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();
    let by_id = |id: i64| {
        responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap_or_else(|| panic!("no response {id}: {responses:?}"))
            .clone()
    };
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    assert!(
        by_id(1)["result"]["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("board")
    );
    let tools: Vec<String> = by_id(2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    for name in [
        "board", "start", "pulse", "done", "ask", "wait", "ack", "report",
    ] {
        assert!(tools.contains(&name.to_owned()), "{name} listed");
    }
    let started = by_id(3);
    assert_eq!(started["result"]["isError"], false, "{started}");
    let text = started["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let envelope: Value = serde_json::from_str(text).expect("cli envelope");
    assert!(envelope["data"]["attempt"].is_string(), "{envelope}");
    assert_eq!(
        by_id(4)["result"]["isError"],
        true,
        "missing eta is an error result"
    );
    assert_eq!(by_id(5)["error"]["code"], -32601);

    let runtime = env.service().runtime().expect("runtime");
    let session = runtime
        .sessions
        .values()
        .find(|session| session.harness_session_id == "pi-session-1")
        .expect("the server registered its own session");
    assert_eq!(session.harness, agendar_core::model::Harness::Pi);
}

#[test]
fn a_quick_ask_reaches_the_human_from_one_line() {
    let mut env = CliEnv::new("demo");
    prepare(&mut env);
    // No step in progress and no --node: a clear usage error.
    let orphan = env.run(&[
        "ask",
        "--question",
        "Which store?",
        "--option",
        "SQLite",
        "--option",
        "JSON lines",
    ]);
    assert_ne!(orphan.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&orphan.stderr).contains("--node"));

    expect_success(
        &env.run(&[
            "progress",
            "started",
            "--node",
            "core/wake",
            "--eta",
            "30m..1h",
        ]),
        "progress",
    );
    let asked = expect_success(
        &env.run(&[
            "ask",
            "--question",
            "Which store should exports use?",
            "--option",
            "SQLite: one file, needs a reader",
            "--option",
            "JSON lines: readable and diffable",
            "--recommend",
            "2",
        ]),
        "ask",
    );
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    let service = env.service();
    let now = agendar_core::model::UtcTime::parse("2099-01-01T00:00:00.000Z").expect("time");
    let state = service.export_state(now).expect("state");
    let project = &state.projects[&ProjectSlug::parse("demo").expect("slug")];
    let decision = &project.decisions[&EventId::parse(&ask_id).expect("id")];
    assert_eq!(decision.node.as_str(), "core/wake", "the step being done");
    assert_eq!(
        decision.mode,
        agendar_core::model::Mode::Ask,
        "ask mode by default: the human sees it"
    );
    assert_eq!(decision.card.options.len(), 2);
    assert_eq!(decision.card.options[1].label, "JSON lines");
    assert_eq!(
        decision.card.options[1].consequence,
        "readable and diffable"
    );
    assert_eq!(decision.card.recommended.option.as_str(), "b");
}
