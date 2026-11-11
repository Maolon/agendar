//! A review verdict travels the answer road: one `review` packet addressed to
//! the session holding the delivered step, `wait --delivery` returns it, and
//! two hosts that receive the same events in any order agree on it.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agendar_core::error::ErrorCode;
use agendar_core::model::{
    Agent, BlobRef, CliTranscriptDemo, Delivery, Demo, Disclosure, EtaRange, EventId, Harness,
    InboxPacket, NodePath, PacketKind, PlanFeature, ProjectDeclared, ProjectSlug, ReviewVerdict,
    Seconds, SessionId, Sha256,
};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::{NoopWaitObserver, Service, WaitOptions};
use agendar_core::vault::{Clock as _, SystemClock};

fn project() -> ProjectSlug {
    ProjectSlug::parse("verdict").expect("project")
}

fn node() -> NodePath {
    NodePath::parse("core/wake").expect("node")
}

fn register(service: &Service, repo: &Path, id: &str) -> SessionId {
    let pid = std::process::id();
    register_session(
        service,
        SessionRegistration {
            project: project(),
            harness: Harness::Generic,
            harness_session_id: id.to_owned(),
            pid,
            process_start: process_identity(pid).expect("process identity"),
            repo_root: repo.to_path_buf(),
        },
    )
    .expect("register")
}

fn declare(service: &Service, session: &SessionId) {
    let patch: ProjectDeclared = serde_json::from_value(serde_json::json!({
        "title": "Verdict", "brief": "Verdicts wake agents.", "goal": "No polling."
    }))
    .expect("project");
    service
        .declare_project(&project(), session, patch, None)
        .expect("declare project");
    for name in ["core/wake", "core/other"] {
        let patch: PlanFeature = serde_json::from_value(serde_json::json!({
            "title": name, "slice": "core", "demo_kind": "cli_transcript"
        }))
        .expect("feature");
        service
            .declare_feature(
                &project(),
                session,
                NodePath::parse(name).expect("node"),
                patch,
                None,
            )
            .expect("declare feature");
    }
}

fn start(service: &Service, session: &SessionId, node: &NodePath) -> EventId {
    let started = service
        .progress_started(
            &project(),
            session,
            node,
            EtaRange {
                min_s: Seconds::new(600).expect("min"),
                max_s: Seconds::new(1_200).expect("max"),
            },
            None,
            None,
        )
        .expect("start");
    EventId::parse(started["attempt"].as_str().expect("attempt")).expect("attempt id")
}

fn deliver(service: &Service, session: &SessionId, node: &NodePath, attempt: &EventId) -> EventId {
    let bytes = b"demo transcript\n";
    let body = Delivery {
        agent: Agent {
            session: session.clone(),
            harness: Harness::Generic,
        },
        name: "Wake".to_owned(),
        demos: vec![Demo::CliTranscript(CliTranscriptDemo {
            blob: BlobRef {
                host: service.host().clone(),
                sha256: Sha256::from_bytes(bytes),
                ext: agendar_core::model::BlobExt::Txt,
                bytes: bytes.len() as u64,
            },
            check: None,
        })],
        criteria: Vec::new(),
        disclosure: Disclosure {
            proven: vec!["Ran it".to_owned()],
            not_proven_yet: Vec::new(),
            faked_for_now: Vec::new(),
        },
        replies: Vec::new(),
    };
    let delivered = service
        .deliver(&project(), session, node, attempt, None, None, body, None)
        .expect("deliver");
    EventId::parse(delivered["event_id"].as_str().expect("event")).expect("delivery id")
}

fn reviews(service: &Service, session: &SessionId) -> Vec<InboxPacket> {
    let (packets, _, _) = service
        .inbox(&project(), session, None, false)
        .expect("inbox");
    packets
        .into_iter()
        .filter(|packet| packet.kind == PacketKind::Review)
        .collect()
}

fn wait(
    service: &Service,
    session: &SessionId,
    delivery: &EventId,
    ms: u64,
) -> agendar_core::Result<InboxPacket> {
    let options = WaitOptions {
        timeout: Some(Duration::from_millis(ms)),
        ..WaitOptions::default()
    };
    service.wait_for_verdict(
        &project(),
        session,
        delivery,
        &options,
        &mut NoopWaitObserver,
    )
}

fn files(root: &Path, current: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
}

/// Copy every Vault event file from one copy to another in the given order.
fn sync(from: &Path, to: &Path, reverse: bool) {
    let mut paths = Vec::new();
    files(from, &from.join("projects"), &mut paths);
    paths.sort();
    if reverse {
        paths.reverse();
    }
    for relative in paths {
        let target = to.join(&relative);
        fs::create_dir_all(target.parent().expect("parent")).expect("dir");
        fs::copy(from.join(&relative), &target).expect("copy");
    }
}

#[test]
fn a_verdict_written_on_another_host_reaches_only_the_delivering_session_in_any_order() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let (vault_b, data_b) = (root.join("vault-b"), root.join("data-b"));
    // Host B runs the agents.
    let (agent_host, _) = Service::init(&vault_b, &data_b).expect("init B");
    let mine = register(&agent_host, &repo, "agent-one");
    let other = register(&agent_host, &repo, "agent-two");
    declare(&agent_host, &mine);
    let attempt = start(&agent_host, &mine, &node());
    let other_node = NodePath::parse("core/other").expect("node");
    let other_attempt = start(&agent_host, &other, &other_node);
    let delivery = deliver(&agent_host, &mine, &node(), &attempt);
    let other_delivery = deliver(&agent_host, &other, &other_node, &other_attempt);

    let status = agendar_host_status(&agent_host, &mine);
    assert_eq!(status.pending_reviews, vec![delivery.clone()]);
    assert!(reviews(&agent_host, &mine).is_empty());
    let error = wait(&agent_host, &mine, &delivery, 300).expect_err("no verdict yet");
    assert_eq!(error.code(), ErrorCode::Timeout);
    let error = wait(&agent_host, &mine, &other_delivery, 300).expect_err("not mine");
    assert_eq!(error.code(), ErrorCode::Context);

    // Host A (the human's computer) gets B's events and writes the verdicts.
    let (vault_a, data_a) = (root.join("vault-a"), root.join("data-a"));
    fs::create_dir_all(vault_a.join("projects")).expect("projects A");
    fs::copy(vault_b.join("vault.json"), vault_a.join("vault.json")).expect("manifest");
    let (human_host, _) = Service::init(&vault_a, &data_a).expect("init A");
    sync(&vault_b, &vault_a, false);
    human_host
        .review_delivery(
            &project(),
            &node(),
            &delivery,
            ReviewVerdict::SendBack,
            Some("The transcript stops early".to_owned()),
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("send back");

    // A third copy receives every file in the reverse order.
    let (vault_c, data_c) = (root.join("vault-c"), root.join("data-c"));
    fs::create_dir_all(vault_c.join("projects")).expect("projects C");
    fs::copy(vault_b.join("vault.json"), vault_c.join("vault.json")).expect("manifest");
    let (third_host, _) = Service::init(&vault_c, &data_c).expect("init C");
    sync(&vault_a, &vault_c, true);
    sync(&vault_a, &vault_b, false);

    let on_b = reviews(&agent_host, &mine);
    assert_eq!(
        on_b.len(),
        1,
        "the delivering session gets one review packet"
    );
    let packet = &on_b[0];
    assert_eq!(packet.delivery.as_ref(), Some(&delivery));
    let review = packet.review.as_ref().expect("review body");
    assert_eq!(review.verdict, ReviewVerdict::SendBack);
    assert_eq!(review.reason.as_deref(), Some("The transcript stops early"));
    assert!(
        reviews(&agent_host, &other).is_empty(),
        "nobody else is woken"
    );
    assert!(
        agendar_host_status(&agent_host, &mine)
            .pending_reviews
            .is_empty()
    );
    assert_eq!(
        agendar_host_status(&agent_host, &other).pending_reviews,
        vec![other_delivery.clone()],
        "the other delivery still waits"
    );

    // `wait --delivery` returns the same packet the inbox carries.
    let waited = wait(&agent_host, &mine, &delivery, 5_000).expect("verdict");
    assert_eq!(&waited, packet);

    // Same events, any arrival order: the same packet on every copy.
    let state_b = agent_host.export_state(SystemClock.now()).expect("B");
    let state_c = third_host.export_state(SystemClock.now()).expect("C");
    let verdict_b = agendar_core::verdict::review_packet(
        state_b.projects.get(&project()).expect("project B"),
        &agent_host
            .refresh(SystemClock.now(), false)
            .expect("fresh B")
            .resolution
            .ordered,
        &project(),
        &delivery,
    )
    .expect("read B");
    let verdict_c = agendar_core::verdict::review_packet(
        state_c.projects.get(&project()).expect("project C"),
        &third_host
            .refresh(SystemClock.now(), false)
            .expect("fresh C")
            .resolution
            .ordered,
        &project(),
        &delivery,
    )
    .expect("read C");
    assert_eq!(verdict_b, verdict_c);
    assert_eq!(verdict_b.as_ref(), Some(packet));

    // Rework leaves the inbox once the agent starts it again.
    start(&agent_host, &mine, &node());
    assert!(
        reviews(&agent_host, &mine).is_empty(),
        "acted on: no longer held"
    );
}

fn agendar_host_status(
    service: &Service,
    session: &SessionId,
) -> agendar_core::service::LoopStatus {
    service
        .loop_status(&project(), session)
        .expect("loop status")
}

#[test]
fn a_superseded_delivery_names_its_replacement() {
    let temp = tempfile::tempdir().expect("temp");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let (service, _) =
        Service::init(&temp.path().join("vault"), &temp.path().join("data")).expect("init");
    let session = register(&service, &repo, "agent-one");
    declare(&service, &session);
    let attempt = start(&service, &session, &node());
    let first = deliver(&service, &session, &node(), &attempt);
    let bytes = b"demo transcript\n";
    let body = Delivery {
        agent: Agent {
            session: session.clone(),
            harness: Harness::Generic,
        },
        name: "Wake again".to_owned(),
        demos: vec![Demo::CliTranscript(CliTranscriptDemo {
            blob: BlobRef {
                host: service.host().clone(),
                sha256: Sha256::from_bytes(bytes),
                ext: agendar_core::model::BlobExt::Txt,
                bytes: bytes.len() as u64,
            },
            check: None,
        })],
        criteria: Vec::new(),
        disclosure: Disclosure {
            proven: Vec::new(),
            not_proven_yet: Vec::new(),
            faked_for_now: Vec::new(),
        },
        replies: Vec::new(),
    };
    service
        .deliver(
            &project(),
            &session,
            &node(),
            &attempt,
            None,
            Some(&first),
            body,
            None,
        )
        .expect("redeliver");
    let error = wait(&service, &session, &first, 300).expect_err("replaced");
    assert_eq!(error.code(), ErrorCode::Withdrawn);
}

/// Codex has no hooks: the steward queues the verdict to the thread once.
#[cfg(unix)]
#[test]
fn the_steward_queues_a_verdict_to_the_codex_thread_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().expect("temp");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let (service, _) =
        Service::init(&temp.path().join("vault"), &temp.path().join("data")).expect("init");
    let pid = std::process::id();
    let session = register_session(
        &service,
        SessionRegistration {
            project: project(),
            harness: Harness::Codex,
            harness_session_id: "thread-v".to_owned(),
            pid,
            process_start: process_identity(pid).expect("process identity"),
            repo_root: repo.clone(),
        },
    )
    .expect("register codex");
    declare(&service, &session);
    let attempt = start(&service, &session, &node());
    let delivery = deliver(&service, &session, &node(), &attempt);
    let script = temp.path().join("codex");
    let record = temp.path().join("messages.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$5\" >> '{}'\nprintf 'Queued message m-1 for thread thread-v.\\n'\n",
            record.display()
        ),
    )
    .expect("fake codex");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    agendar_core::runtime::Config::update(service.local().data_root(), |config| {
        config.codex_bin = Some(script.to_string_lossy().into_owned());
        Ok(())
    })
    .expect("codex path");
    let quiet = agendar_core::runtime::steward::steward_tick(&service).expect("tick");
    assert_eq!(quiet.delivered, 0, "nothing to queue before the verdict");
    service
        .review_delivery(
            &project(),
            &node(),
            &delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("accept");
    let report = agendar_core::runtime::steward::steward_tick(&service).expect("tick");
    assert_eq!(report.delivered, 1);
    let messages = fs::read_to_string(&record).expect("queued");
    assert!(messages.contains("\"kind\":\"review\""), "{messages}");
    assert!(messages.contains("\"verdict\":\"accept\""), "{messages}");
    let again = agendar_core::runtime::steward::steward_tick(&service).expect("tick");
    assert_eq!(again.delivered, 0, "queued once");
}
