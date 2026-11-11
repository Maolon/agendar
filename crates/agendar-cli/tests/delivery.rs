//! CLI delivery, attachment upload, house rules and SessionStart context.

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

use agendar_core::model::{ProjectSlug, RuleId, RuleSet};
use support::{CliEnv, expect_error, expect_success};

const PROJECT: &str = "demo";
const NODE: &str = "core/wake";

struct Board {
    env: CliEnv,
    attempt: String,
    receipt: String,
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).expect("json")).expect("write json");
}

fn card_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agendar-core/tests/oracle/examples/ask.json")
}

fn board() -> Board {
    let mut env = CliEnv::new(PROJECT);
    env.init();
    let project = env.root.path().join("project.json");
    write_json(
        &project,
        &json!({"title": "Delivery", "brief": "Delivery board.", "goal": "Prove delivery."}),
    );
    expect_success(
        &env.run(&["project", "declare", "--json", &project.to_string_lossy()]),
        "project declare",
    );
    let feature = env.root.path().join("feature.json");
    write_json(
        &feature,
        &json!({"title": "Wake delivery", "slice": "core", "demo_kind": "cli_transcript"}),
    );
    expect_success(
        &env.run(&[
            "feature",
            "declare",
            "--node",
            NODE,
            "--json",
            &feature.to_string_lossy(),
        ]),
        "feature declare",
    );
    let started = expect_success(
        &env.run(&["progress", "started", "--node", NODE, "--eta", "10m..20m"]),
        "progress",
    );
    let attempt = started["data"]["attempt"]
        .as_str()
        .expect("attempt")
        .to_owned();
    let asked = expect_success(
        &env.run(&[
            "ask",
            "--node",
            NODE,
            "--json",
            &card_path().to_string_lossy(),
            "--mode",
            "ask",
        ]),
        "ask",
    );
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask id").to_owned();
    let receipt = env.answer(&ask_id, "adapters");
    Board {
        env,
        attempt,
        receipt,
    }
}

fn delivery_input(env: &CliEnv) -> PathBuf {
    let demo = env.repo.join("demo.txt");
    let evidence = env.repo.join("evidence.txt");
    fs::write(&demo, b"demo transcript\n").expect("demo");
    fs::write(&evidence, b"aggregate: passed\n").expect("evidence");
    let input = env.root.path().join("delivery.json");
    write_json(
        &input,
        &json!({
            "name": "Wake delivery",
            "demos": [{
                "kind": "cli_transcript",
                "body": {
                    "file": demo.to_string_lossy(),
                    "ext": "txt",
                    "check": {
                        "criterion": "continues",
                        "argv": ["agendar-check-probe", "file_digest"],
                        "cwd": env.repo.to_string_lossy(),
                        "timeout_s": 30
                    }
                }
            }],
            "criteria": [{
                "criterion": "continues",
                "status": "passed",
                "claim": "agent_says",
                "evidence": {"file": evidence.to_string_lossy(), "ext": "txt"}
            }],
            "disclosure": {
                "proven": ["Agent compared bytes"],
                "not_proven": ["Independent check"],
                "faked": []
            }
        }),
    );
    input
}

fn blob_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(blob_files(&path));
        } else if path
            .components()
            .any(|component| component.as_os_str() == "blobs")
        {
            files.push(path);
        }
    }
    files
}

#[test]
fn deliver_publishes_blobs_before_the_event_and_retries_idempotently() {
    let board = board();
    let input = delivery_input(&board.env);
    let request_id = ulid::Ulid::generate().to_string();
    let args = [
        "--request-id",
        &request_id,
        "deliver",
        "--node",
        NODE,
        "--attempt",
        &board.attempt,
        "--receipt",
        &board.receipt,
        "--json",
        &input.to_string_lossy(),
    ];
    let first = expect_success(&board.env.run(&args), "deliver");
    let event_id = first["data"]["event_id"]
        .as_str()
        .expect("event id")
        .to_owned();
    assert_eq!(first["data"]["review"], "pending");
    let uploaded = first["data"]["uploaded"].as_array().expect("uploaded");
    assert_eq!(uploaded.len(), 2, "demo and criterion evidence");
    for blob in uploaded {
        let name = format!("{}.txt", blob["sha256"].as_str().expect("digest"));
        assert!(
            blob_files(&board.env.vault)
                .iter()
                .any(|path| path.file_name().and_then(|file| file.to_str()) == Some(&name)),
            "the blob {name} exists in the Vault"
        );
    }
    assert_eq!(board.env.count_kind("delivery"), 1);

    let retry = expect_success(&board.env.run(&args), "deliver");
    assert_eq!(retry["data"]["event_id"], event_id);
    assert_eq!(
        board.env.count_kind("delivery"),
        1,
        "request-id retry never duplicates the delivery"
    );

    // The projected delivery has the receipt criteria and no Seen claim.
    let service = board.env.service();
    let project = ProjectSlug::parse(PROJECT).expect("project");
    let fresh = service
        .refresh(
            agendar_core::model::UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let delivery = fresh.state.projects[&project]
        .deliveries
        .values()
        .next()
        .expect("delivery projection");
    assert_eq!(delivery.criteria[0].criterion.id.as_str(), "continues");
    assert_eq!(
        delivery.criteria[0].provenance,
        agendar_core::model::Provenance::AgentSays
    );
    match &delivery.demos[0] {
        agendar_core::model::Demo::CliTranscript(demo) => {
            let check = demo
                .check
                .as_ref()
                .expect("the proposed check is projected");
            assert_eq!(check.criterion.as_str(), "continues");
            assert_eq!(check.timeout_s, 30);
        }
        other => panic!("unexpected demo: {other:?}"),
    }
}

#[test]
fn deliver_rejects_criteria_that_do_not_match_the_receipt() {
    let board = board();
    let input = delivery_input(&board.env);
    let mut value: Value = serde_json::from_slice(&fs::read(&input).expect("input")).expect("json");
    value["criteria"][0]["criterion"] = json!("something_else");
    write_json(&input, &value);
    let error = expect_error(
        &board.env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &board.attempt,
            "--receipt",
            &board.receipt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "E_VALIDATION",
        2,
    );
    assert_eq!(error["error"]["field"], "/criteria");
    assert_eq!(
        board.env.count_kind("delivery"),
        0,
        "no event when validation fails"
    );
}

/// Finished work no decision asked for: no receipt, no criteria, no check.
fn receipt_free_input(env: &CliEnv) -> PathBuf {
    let input = delivery_input(env);
    let mut value: Value = serde_json::from_slice(&fs::read(&input).expect("input")).expect("json");
    value["demos"][0]["body"]
        .as_object_mut()
        .expect("demo body")
        .remove("check");
    value.as_object_mut().expect("input").remove("criteria");
    value["disclosure"]["faked"] = json!(["Sample schedule"]);
    write_json(&input, &value);
    input
}

#[test]
fn deliver_without_a_receipt_appends_a_delivery_with_no_receipt_ref() {
    let board = board();
    let input = receipt_free_input(&board.env);
    let delivered = expect_success(
        &board.env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &board.attempt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "deliver",
    );
    assert_eq!(delivered["data"]["review"], "pending");
    let line = board
        .env
        .event_lines()
        .into_iter()
        .find(|line| line.contains("\"kind\":\"delivery\""))
        .expect("delivery line");
    let event: Value = serde_json::from_str(&line).expect("event json");
    assert!(event["refs"].get("receipt").is_none(), "{event}");
    assert_eq!(event["body"]["criteria"], json!([]));
    assert_eq!(
        event["body"]["disclosure"]["faked_for_now"],
        json!(["Sample schedule"])
    );
}

#[test]
fn deliver_without_a_receipt_rejects_criteria() {
    let board = board();
    let input = receipt_free_input(&board.env);
    let mut value: Value = serde_json::from_slice(&fs::read(&input).expect("input")).expect("json");
    value["criteria"] = json!([{"criterion": "continues", "status": "passed"}]);
    write_json(&input, &value);
    let error = expect_error(
        &board.env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &board.attempt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "E_VALIDATION",
        2,
    );
    assert_eq!(error["error"]["field"], "/criteria");
    assert_eq!(
        error["error"]["message"],
        "A delivery without a receipt must list no criteria."
    );
    assert_eq!(board.env.count_kind("delivery"), 0);
}

#[test]
fn deliver_does_not_append_when_an_upload_fails() {
    let board = board();
    let input = delivery_input(&board.env);
    let mut value: Value = serde_json::from_slice(&fs::read(&input).expect("input")).expect("json");
    value["criteria"][0]["evidence"]["file"] = json!("/nonexistent/evidence.txt");
    write_json(&input, &value);
    expect_error(
        &board.env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &board.attempt,
            "--receipt",
            &board.receipt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "E_VALIDATION",
        2,
    );
    assert_eq!(
        board.env.count_kind("delivery"),
        0,
        "the delivery event is last, never written after a failed upload"
    );
}

#[test]
fn blob_put_publishes_an_attachment_and_rejects_unsupported_media() {
    let board = board();
    let file = board.env.repo.join("attachment.txt");
    fs::write(&file, b"attachment body\n").expect("attachment");
    let published = expect_success(
        &board
            .env
            .run(&["blob", "put", &file.to_string_lossy(), "--ext", "txt"]),
        "blob put",
    );
    let blob = &published["data"]["blob"];
    assert_eq!(blob["ext"], "txt");
    assert!(blob["bytes"].as_u64().expect("bytes") > 0);
    assert_eq!(
        board.env.count_kind("delivery"),
        0,
        "an attachment utility writes no event"
    );

    expect_error(
        &board
            .env
            .run(&["blob", "put", &file.to_string_lossy(), "--ext", "mp4"]),
        "E_UNSUPPORTED",
        9,
    );
}

#[test]
fn house_rules_appear_in_the_cli_and_session_start_context() {
    let mut board = board();
    let service = board.env.service();
    let project = ProjectSlug::parse(PROJECT).expect("project");
    service
        .set_rule(
            &project,
            RuleSet {
                rule_id: RuleId::parse("check-bytes").expect("rule id"),
                text: "Compare bytes before delivery".to_owned(),
                enabled: true,
            },
            Vec::new(),
        )
        .expect("set rule");
    drop(service);

    let rules = expect_success(&board.env.run(&["rules"]), "rules");
    let rows = rules["data"]["rules"].as_array().expect("rules");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["rule"]["rule_id"], "check-bytes");
    assert_eq!(rows[0]["rule"]["text"], "Compare bytes before delivery");

    // Register a Claude Code harness session bound to this test process, then
    // call the real SessionStart shim with the same harness session id.
    let pid = std::process::id().to_string();
    let repo = board.env.repo.to_string_lossy().into_owned();
    let registered = expect_success(
        &board.env.run(&[
            "init",
            "--harness",
            "claude_code",
            "--harness-session",
            "hook-session",
            "--pid",
            &pid,
            "--repo",
            &repo,
        ]),
        "init",
    );
    let session = registered["data"]["session"]
        .as_str()
        .expect("session")
        .to_owned();
    board.env.session = session.clone();

    let mut command = board.env.command();
    command
        .arg("--vault")
        .arg(&board.env.vault)
        .arg("--data-dir")
        .arg(&board.env.data)
        .arg("--project")
        .arg(PROJECT)
        .arg("hook")
        .arg("session-start")
        .env("AGENDAR_HOOK_PID", &pid)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("hook");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        let payload = json!({
            "session_id": "hook-session",
            "cwd": repo,
            "hook_event_name": "SessionStart",
        });
        writeln!(stdin, "{payload}").expect("write stdin");
    }
    let output = child.wait_with_output().expect("hook output");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("hook JSON");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext");
    assert!(context.contains("check-bytes: Compare bytes before delivery"));
    assert!(
        context.contains(&session),
        "the assigned session is printed"
    );
    assert!(context.contains("--vault"));
}

/// A tiny valid PNG (one solid colour) for option image tests.
fn tiny_png(rgb: [u8; 3]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).expect("len").to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffff_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    // Stored (uncompressed) deflate block holding one scanline: filter 0 + RGB.
    let raw = [0, rgb[0], rgb[1], rgb[2]];
    let mut zlib = vec![0x78, 0x01, 0x01, 0x04, 0x00, 0xfb, 0xff];
    zlib.extend_from_slice(&raw);
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn image_card(env: &CliEnv, images: [Value; 2]) -> PathBuf {
    let mut card: Value =
        serde_json::from_slice(&fs::read(card_path()).expect("card")).expect("card json");
    for (index, image) in images.into_iter().enumerate() {
        card["options"][index]["image"] = image;
    }
    let path = env.root.path().join("image-card.json");
    write_json(&path, &card);
    path
}

#[test]
fn ask_publishes_option_images_and_refuses_bad_ones_before_uploading() {
    let board = board();
    let env = &board.env;
    let first = env.repo.join("first.png");
    let second = env.repo.join("second.png");
    fs::write(&first, tiny_png([20, 20, 20])).expect("first");
    fs::write(&second, tiny_png([232, 137, 12])).expect("second");
    let put = expect_success(
        &env.run(&["blob", "put", &second.to_string_lossy(), "--ext", "png"]),
        "blob put",
    );
    let card = image_card(
        env,
        [
            json!({"file": first.to_string_lossy(), "ext": "png"}),
            put["data"].clone(),
        ],
    );
    let asks = env.count_kind("decision.ask");
    let asked = expect_success(
        &env.run(&[
            "ask",
            "--node",
            NODE,
            "--json",
            &card.to_string_lossy(),
            "--mode",
            "ask",
        ]),
        "ask",
    );
    assert_eq!(env.count_kind("decision.ask"), asks + 1);
    let ask_id = asked["data"]["ask_id"].as_str().expect("ask id");
    let line = env
        .event_lines()
        .into_iter()
        .find(|line| line.contains(ask_id) && line.contains("\"kind\":\"decision.ask\""))
        .expect("ask event");
    let event: Value = serde_json::from_str(&line).expect("event json");
    let options = &event["body"]["card"]["options"];
    assert_eq!(options[0]["image"]["ext"], "png");
    assert!(options[0]["image"]["sha256"].as_str().is_some());
    assert_eq!(options[1]["image"], put["data"]["blob"]);
    let blobs = blob_files(&env.vault);
    assert!(
        blobs.iter().any(|path| path
            .to_string_lossy()
            .contains(options[0]["image"]["sha256"].as_str().expect("sha"))),
        "the local image file was published to the Vault"
    );

    // A text file is not an option image.
    let note = env.repo.join("note.txt");
    fs::write(&note, b"not an image\n").expect("note");
    let bad_ext = image_card(
        env,
        [
            json!({"file": note.to_string_lossy(), "ext": "txt"}),
            json!(null),
        ],
    );
    let mut bad: Value = serde_json::from_slice(&fs::read(&bad_ext).expect("bad")).expect("json");
    bad["options"][1]
        .as_object_mut()
        .expect("option")
        .remove("image");
    write_json(&bad_ext, &bad);
    expect_error(
        &env.run(&[
            "ask",
            "--node",
            NODE,
            "--json",
            &bad_ext.to_string_lossy(),
            "--mode",
            "ask",
        ]),
        "E_VALIDATION",
        2,
    );

    // An invalid card publishes nothing, not even its images.
    let fresh = env.repo.join("fresh.png");
    fs::write(&fresh, tiny_png([1, 2, 3])).expect("fresh");
    let invalid = image_card(
        env,
        [
            json!({"file": fresh.to_string_lossy(), "ext": "png"}),
            json!({"file": fresh.to_string_lossy(), "ext": "png"}),
        ],
    );
    let mut card: Value = serde_json::from_slice(&fs::read(&invalid).expect("card")).expect("json");
    card["question"] = json!("x".repeat(200));
    write_json(&invalid, &card);
    let before = blob_files(&env.vault).len();
    expect_error(
        &env.run(&[
            "ask",
            "--node",
            NODE,
            "--json",
            &invalid.to_string_lossy(),
            "--mode",
            "ask",
        ]),
        "E_VALIDATION",
        2,
    );
    assert_eq!(
        blob_files(&env.vault).len(),
        before,
        "no image was published"
    );
    assert_eq!(env.count_kind("decision.ask"), asks + 1);
}

#[test]
fn a_sent_back_delivery_reaches_the_agent_through_inbox_with_the_reason() {
    let board = board();
    let input = delivery_input(&board.env);
    let delivered = expect_success(
        &board.env.run(&[
            "deliver",
            "--node",
            NODE,
            "--attempt",
            &board.attempt,
            "--receipt",
            &board.receipt,
            "--json",
            &input.to_string_lossy(),
        ]),
        "deliver",
    );
    let delivery = agendar_core::model::EventId::parse(
        delivered["data"]["event_id"].as_str().expect("event id"),
    )
    .expect("delivery id");
    let reviews = |inbox: &Value| -> Vec<Value> {
        inbox["data"]["packets"]
            .as_array()
            .expect("packets")
            .iter()
            .filter(|packet| packet["kind"] == "review")
            .cloned()
            .collect()
    };
    assert!(reviews(&expect_success(&board.env.run(&["inbox"]), "inbox")).is_empty());

    // The app's Send back button writes this event through the same service.
    let project = ProjectSlug::parse(PROJECT).expect("project");
    let node = agendar_core::model::NodePath::parse(NODE).expect("node");
    let receipt = agendar_core::model::EventId::parse(&board.receipt).expect("receipt");
    let (review, _) = board
        .env
        .service()
        .review_delivery(
            &project,
            &node,
            &delivery,
            agendar_core::model::ReviewVerdict::SendBack,
            Some("The transcript stops before the result".to_owned()),
            None,
            vec![agendar_core::model::ReviewComment {
                anchor: agendar_core::model::ReviewAnchor::TranscriptLine {
                    demo: 0,
                    line: 2,
                    quote: "exit 0".to_owned(),
                },
                text: "Show the result line".to_owned(),
            }],
            vec![receipt],
        )
        .expect("send back");

    let inbox = expect_success(&board.env.run(&["inbox"]), "inbox");
    let packets = reviews(&inbox);
    assert_eq!(packets.len(), 1, "{inbox}");
    assert_eq!(packets[0]["node"], NODE);
    assert_eq!(packets[0]["source"], review.as_str());
    assert_eq!(packets[0]["review"]["verdict"], "send_back");
    assert_eq!(
        packets[0]["review"]["reason"],
        "The transcript stops before the result"
    );
    assert_eq!(
        packets[0]["review"]["comments"],
        serde_json::json!([{
            "anchor": {"kind": "transcript_line", "demo": 0, "line": 2, "quote": "exit 0"},
            "text": "Show the result line"
        }])
    );
    assert!(
        packets[0].get("answer").is_none(),
        "a review carries no ack"
    );

    // Reading it leaves a delivered record that names the delivery.
    let records: Vec<Value> = board
        .env
        .event_lines()
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).expect("event json"))
        .filter(|event| event["kind"] == "delivered")
        .collect();
    assert!(
        records.iter().any(|event| {
            event["refs"]["delivery"] == delivery.as_str()
                && event["refs"]["causes"][0] == review.as_str()
        }),
        "{records:?}"
    );
    expect_success(&board.env.run(&["status"]), "status");
}

// --- Diff demo: hunks cut verbatim from a commit ----------------------------

fn git(env: &CliEnv, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&env.repo)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .env("HOME", &env.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8")
}

fn numbered(change: &[(usize, &str)]) -> String {
    (1..=30)
        .map(|line| {
            change
                .iter()
                .find(|(at, _)| *at == line)
                .map_or_else(|| format!("line {line}\n"), |(_, text)| format!("{text}\n"))
        })
        .collect()
}

/// A repo with two commits; the second changes a.txt in two far-apart hunks
/// and adds b.txt. Returns the second commit's sha.
fn diff_repo(env: &CliEnv) -> String {
    git(env, &["init", "-q"]);
    fs::write(env.repo.join("a.txt"), numbered(&[])).expect("a");
    git(env, &["add", "a.txt"]);
    git(env, &["commit", "-q", "-m", "first"]);
    fs::write(
        env.repo.join("a.txt"),
        numbered(&[(2, "line two"), (25, "line twenty-five")]),
    )
    .expect("a");
    fs::write(env.repo.join("b.txt"), "new\nfile\n").expect("b");
    git(env, &["add", "a.txt", "b.txt"]);
    git(env, &["commit", "-q", "-m", "second"]);
    git(env, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn diff_input(env: &CliEnv, body: Value) -> PathBuf {
    let input = env.root.path().join("diff-delivery.json");
    write_json(
        &input,
        &json!({
            "name": "Wake delivery",
            "demos": [{"kind": "diff", "body": body}],
            "disclosure": {"proven": [], "not_proven": [], "faked": []}
        }),
    );
    input
}

fn deliver_diff(board: &Board, input: &Path) -> std::process::Output {
    board.env.run(&[
        "deliver",
        "--node",
        NODE,
        "--attempt",
        &board.attempt,
        "--json",
        &input.to_string_lossy(),
    ])
}

#[test]
fn deliver_cuts_the_picked_hunk_verbatim_from_the_commit() {
    let board = board();
    let sha = diff_repo(&board.env);
    let short = &sha[..7];
    let input = diff_input(
        &board.env,
        json!({"commit": short, "hunks": [{"file": "a.txt", "hunk": 2, "why": "The late line."}]}),
    );
    expect_success(&deliver_diff(&board, &input), "deliver");

    let line = board
        .env
        .event_lines()
        .into_iter()
        .find(|line| line.contains("\"kind\":\"delivery\""))
        .expect("delivery line");
    let event: Value = serde_json::from_str(&line).expect("event json");
    let demo = &event["body"]["demos"][0];
    assert_eq!(demo["kind"], "diff");
    let body = &demo["body"];
    assert_eq!(body["commit"], sha.as_str());
    assert_eq!(body["files_total"], 2);
    assert_eq!(body["hunks_total"], 3);
    assert_eq!(body["added_total"], 4);
    assert_eq!(body["removed_total"], 2);
    assert_eq!(
        body["shown"],
        json!([{"path": "a.txt", "hunk": 2, "why": "The late line."}])
    );
    assert_eq!(
        body["omitted"],
        json!([{"path": "b.txt", "added": 2, "removed": 0}])
    );

    // The excerpt is a.txt's header plus its second hunk, byte for byte.
    let shown = git(&board.env, &["show", "--format=", "--no-color", &sha]);
    let section_start = shown.find("diff --git a/a.txt").expect("a.txt section");
    let section_end = shown.find("diff --git a/b.txt").expect("b.txt section");
    let section = &shown[section_start..section_end];
    let first_hunk = section.find("\n@@ ").expect("first hunk") + 1;
    let second_hunk = first_hunk + 1 + section[first_hunk + 1..].find("\n@@ ").expect("second") + 1;
    let expected = format!("{}{}", &section[..first_hunk], &section[second_hunk..]);

    let blob: agendar_core::model::BlobRef =
        serde_json::from_value(body["blob"].clone()).expect("blob ref");
    assert_eq!(blob.ext.as_str(), "txt");
    let bytes = agendar_core::vault::blobs::read_blob(
        board.env.service().vault(),
        &ProjectSlug::parse(PROJECT).expect("project"),
        &blob,
    )
    .expect("excerpt blob");
    assert_eq!(String::from_utf8(bytes).expect("utf8"), expected);
}

#[test]
fn deliver_refuses_an_unknown_hunk_and_a_merge_commit() {
    let board = board();
    let sha = diff_repo(&board.env);
    let input = diff_input(
        &board.env,
        json!({"commit": sha, "repo": board.env.repo.to_string_lossy(), "hunks": [{"file": "a.txt", "hunk": 3, "why": "x"}]}),
    );
    let error = expect_error(&deliver_diff(&board, &input), "E_VALIDATION", 2);
    assert_eq!(error["error"]["field"], "/demos/0/body/hunks/0/hunk");
    let message = error["error"]["message"].as_str().expect("message");
    assert!(message.contains("has 2 hunks"), "{message}");

    // A merge commit: one side branch merged with --no-ff.
    git(&board.env, &["checkout", "-q", "-b", "side"]);
    fs::write(board.env.repo.join("c.txt"), "side\n").expect("c");
    git(&board.env, &["add", "c.txt"]);
    git(&board.env, &["commit", "-q", "-m", "side"]);
    git(&board.env, &["checkout", "-q", &sha]);
    git(
        &board.env,
        &["merge", "-q", "--no-ff", "-m", "merge", "side"],
    );
    let merge = git(&board.env, &["rev-parse", "HEAD"]).trim().to_owned();
    let input = diff_input(
        &board.env,
        json!({"commit": merge, "hunks": [{"file": "c.txt", "hunk": 1, "why": "x"}]}),
    );
    let error = expect_error(&deliver_diff(&board, &input), "E_VALIDATION", 2);
    assert_eq!(error["error"]["field"], "/demos/0/body/commit");
    assert_eq!(board.env.count_kind("delivery"), 0);
}
