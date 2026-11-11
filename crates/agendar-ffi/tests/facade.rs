#![forbid(unsafe_code)]

use agendar_core::service::Service;
use agendar_ffi::{FfiError, Session, version};

#[test]
fn version_is_the_workspace_version() {
    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn opens_an_existing_vault_as_a_new_host_and_reads_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    // The Vault is made by another host (the Mac); the mobile client has its own data dir.
    Service::init(&vault, &dir.path().join("mac")).expect("init");
    let mobile = dir.path().join("mobile");
    let session =
        Session::open(vault.display().to_string(), mobile.display().to_string()).expect("open");
    let summary = session.snapshot_summary().expect("summary");
    assert!(summary.projects.is_empty());
    assert!(!summary.vault_id.is_empty());
    assert!(session.needs().expect("needs").is_empty());
}

#[test]
fn never_creates_a_vault() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("not-a-vault");
    std::fs::create_dir(&missing).expect("mkdir");
    let error = Session::open(
        missing.display().to_string(),
        dir.path().join("mobile").display().to_string(),
    )
    .err()
    .expect("a folder without a Vault is refused");
    let FfiError::Core { code, .. } = error else {
        panic!("a core error, not a refusal: {error}");
    };
    assert!(code.starts_with("E_"), "{code}");
    assert!(!missing.join("vault.json").exists());
}

/// One project made on the Mac host: a step, an agent on it, and one open ask
/// with two options. Written as raw lines, as an agent host would.
fn vault_with_open_ask(root: &std::path::Path) -> String {
    use serde_json::json;
    let mac = "h-00000000000000000000000001";
    let session = "00000000000000000000000101";
    let agent_writer = format!("{mac}/s-{session}");
    let operator = format!("{mac}/operator");
    let agent = json!({"harness": "generic", "session": session});
    let ask = "00000000000000000000000004";
    let events = [
        (
            "00000000000000000000000001",
            &operator,
            1,
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Answer from a mobile client", "title": "Mobile"}),
        ),
        (
            "00000000000000000000000002",
            &operator,
            2,
            "plan.feature",
            json!({"node": "n0"}),
            json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s0", "title": "First step"}),
        ),
        (
            "00000000000000000000000003",
            &agent_writer,
            1,
            "work.progress",
            json!({"node": "n0"}),
            json!({"agent": agent, "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
        ),
        (
            ask,
            &agent_writer,
            2,
            "decision.ask",
            json!({"node": "n0"}),
            json!({"agent": agent, "card": {
            "already_checked": [], "domain": "arch",
            "done_when": [{"id": "bytes", "text": "Demo bytes match."}],
            "external": false, "on_timeout": {"kind": "hold"},
            "options": [
                {"consequence": "Use hooks.", "eta": {"max_s": 3600, "min_s": 1800}, "external": false,
                 "icon": "link", "id": "adapters", "label": "Use harness adapters", "reversibility": "easy"},
                {"consequence": "Use the inbox.", "eta": {"max_s": 1800, "min_s": 900}, "external": false,
                 "icon": "inbox", "id": "inbox", "label": "Use inbox on next start", "reversibility": "easy"}
            ],
            "question": "How should answers reach idle agents?", "reach": "local",
            "recommended": {"because": "It is simpler.", "confidence": "med", "option": "adapters"},
            "reversibility": "easy", "why_now": "Wake delivery is blocked."},
          "policy": {"budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [],
                                "speculative_features": 0},
                     "effective": "ask", "external": false, "heuristic_hits": [], "requested": "ask",
                     "reversibility": "easy"}}),
        ),
    ];
    let host_dir = root.join("projects/mobile").join(mac);
    std::fs::create_dir_all(&host_dir).expect("host dir");
    let now = agendar_core::vault::Clock::now(&agendar_core::vault::SystemClock);
    for (index, (id, writer, seq, kind, refs, body)) in events.into_iter().enumerate() {
        let line = json!({
            "v": 1, "id": id, "seq": seq, "writer": writer,
            "ts": now.add_seconds(index as i64 - 60).canonical(),
            "project": "mobile", "refs": refs, "kind": kind, "body": body,
        });
        let file = host_dir.join(format!(
            "{}.jsonl",
            writer.rsplit('/').next().expect("base")
        ));
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&serde_json::to_string(&line).expect("json"));
        text.push('\n');
        std::fs::write(&file, text).expect("write line");
    }
    ask.to_owned()
}

#[test]
fn answers_an_open_ask_from_the_mobile_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    Service::init(&vault, &dir.path().join("mac")).expect("init");
    let ask = vault_with_open_ask(&vault);
    let mobile = dir.path().join("mobile");
    let session =
        Session::open(vault.display().to_string(), mobile.display().to_string()).expect("open");
    session.spike_refresh().expect("refresh");

    let needs = session.needs().expect("needs");
    assert_eq!(needs.len(), 1, "{needs:?}");
    assert_eq!(needs[0].kind, "decide");
    assert_eq!(needs[0].source, ask);

    let FfiError::Core { code, .. } = session
        .answer(ask.clone(), "nope".to_owned())
        .expect_err("an option of another card is refused")
    else {
        panic!("a core error, not a refusal");
    };
    assert_eq!(code, "E_VALIDATION");

    let answer = session
        .answer(ask.clone(), "inbox".to_owned())
        .expect("answer");
    assert!(session.needs().expect("needs").is_empty());

    // The answer is the mobile operator's, in the mobile client's own host directory.
    let mut hosts: Vec<String> = std::fs::read_dir(vault.join("projects/mobile"))
        .expect("project dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    hosts.sort();
    assert_eq!(hosts.len(), 2, "{hosts:?}");
    let mobile_host = &hosts[1];
    let written = std::fs::read_to_string(
        vault
            .join("projects/mobile")
            .join(mobile_host)
            .join("operator.jsonl"),
    )
    .expect("mobile operator stream");
    let line: serde_json::Value =
        serde_json::from_str(written.lines().next().expect("one line")).expect("json");
    assert_eq!(line["id"], answer.as_str());
    assert_eq!(line["kind"], "decision.answer");
    assert_eq!(line["refs"]["ask"], ask.as_str());
    assert_eq!(line["body"]["option"], "inbox");
}

#[test]
fn cold_split_reads_every_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    Service::init(&vault, &dir.path().join("mac")).expect("init");
    vault_with_open_ask(&vault);
    let session = Session::open(
        vault.display().to_string(),
        dir.path().join("mobile").display().to_string(),
    )
    .expect("open");
    let split = session.cold_split().expect("split");
    assert_eq!(split.events, 4);
    assert_eq!(split.files, 2);
}

#[test]
fn two_hosts_converge_on_the_mobile_answers_and_see_a_stub_as_waiting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    Service::init(&vault, &dir.path().join("mac")).expect("init");
    let ask = vault_with_open_ask(&vault);
    let open = |name: &str| {
        Session::open(
            vault.display().to_string(),
            dir.path().join(name).display().to_string(),
        )
        .expect("open")
    };
    let mobile = open("mobile");
    let options = ["inbox", "adapters"];
    let answers: Vec<String> = (0..6)
        .map(|run| {
            mobile
                .answer(ask.clone(), options[run % 2].to_owned())
                .expect("answer")
        })
        .collect();
    let health = mobile.health().expect("health");
    assert_eq!(health.events, 10);
    assert!(health.waiting.is_empty() && health.strays.is_empty());

    // Another host reads the same events in the same order, with the same digest.
    let mac = open("other");
    let other = mac.health().expect("health");
    assert_ne!(other.host, health.host);
    assert_eq!(other.events, health.events);
    assert!(other.event_set_digest.is_some());
    assert_eq!(other.event_set_digest, health.event_set_digest);
    let stream = vault
        .join("projects/mobile")
        .join(&health.host)
        .join("operator.jsonl");
    let written: Vec<String> = std::fs::read_to_string(&stream)
        .expect("mobile stream")
        .lines()
        .map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).expect("json");
            line["id"].as_str().expect("id").to_owned()
        })
        .collect();
    assert_eq!(written, answers);

    // The sync app evicts the mobile client's file on the other host: it is waiting
    // there, and its events stay.
    let stub = stream.with_file_name(".operator.jsonl.icloud");
    std::fs::rename(&stream, &stub).expect("evict");
    let evicted = mac.health().expect("health");
    assert_eq!(evicted.events, health.events);
    assert_eq!(
        evicted.waiting,
        vec![format!("projects/mobile/{}/operator.jsonl", health.host)]
    );
}
