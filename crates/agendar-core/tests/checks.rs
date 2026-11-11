//! Approved Seen checks: profile binding, bounded execution and observation
//! publication.

mod support;

use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Instant;

use serde_json::json;

use agendar_core::error::ErrorCode;
use agendar_core::model::{
    Agent, BlobRef, CheckResult, Claim, CliTranscriptDemo, CriterionResult, Delivery, Demo,
    DemoKind, Disclosure, EventId, Harness, NodePath, PlanFeature, ProjectDeclared, ProjectSlug,
    ReviewVerdict, SessionId, Sha256, UtcTime, WorkStatus,
};
#[cfg(unix)]
use agendar_core::runtime::checks::MAX_CHECK_OUTPUT_BYTES;
use agendar_core::runtime::checks::{self, CheckProfile};
use agendar_core::runtime::sessions::{RuntimeState, update_runtime};
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::runtime::{Config, LocalPaths};
use agendar_core::service::Service;

const PROJECT: &str = "demo";
const NODE: &str = "core/wake";

struct Scratch {
    _root: tempfile::TempDir,
    data: PathBuf,
    repo: PathBuf,
    service: Service,
    project: ProjectSlug,
    session: SessionId,
    node: NodePath,
    attempt: EventId,
    receipt: EventId,
    delivery: EventId,
    blob: BlobRef,
}

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("project")
}

fn node() -> NodePath {
    NodePath::parse(NODE).expect("node")
}

fn fixture_blob() -> BlobRef {
    let bytes = b"fixture check output\n";
    BlobRef {
        host: agendar_core::model::HostId::parse("h-00000000000000000000000001").expect("host"),
        sha256: Sha256::from_bytes(bytes),
        ext: agendar_core::model::BlobExt::Txt,
        bytes: bytes.len() as u64,
    }
}

fn scratch() -> Scratch {
    let root = tempfile::tempdir().expect("temporary root");
    let vault = root.path().join("vault");
    let data = root.path().join("data");
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    let (service, _) = Service::init(&vault, &data).expect("init");
    let project = project();
    service.select_config(Some(&project)).expect("select");
    Config::map_repo(&data, &repo, &project).expect("link repo");
    let pid = std::process::id();
    let session = register_session(
        &service,
        SessionRegistration {
            project: project.clone(),
            harness: Harness::Generic,
            harness_session_id: "checks-test".to_owned(),
            pid,
            process_start: agendar_core::runtime::sessions::process_identity(pid)
                .expect("process identity"),
            repo_root: repo.clone(),
        },
    )
    .expect("register session");
    service
        .declare_project(
            &project,
            &session,
            ProjectDeclared {
                agent: None,
                title: Some("Checks".to_owned()),
                brief: Some("Approved evidence checks.".to_owned()),
                goal: Some("Prove Seen provenance.".to_owned()),
                glyph: Some("radar".to_owned()),
                archived: None,
                icon: None,
            },
            None,
        )
        .expect("declare project");
    for (name, depends) in [("core/wake", vec![]), ("core/next", vec![node()])] {
        let feature_node = NodePath::parse(name).expect("node");
        service
            .declare_feature(
                &project,
                &session,
                feature_node,
                PlanFeature {
                    agent: None,
                    title: Some(name.to_owned()),
                    depends_on: Some(depends),
                    slice: Some("core".to_owned()),
                    demo_kind: Some(DemoKind::CliTranscript),
                    milestone: None,
                    deadline: None,
                    priority: None,
                    paused: None,
                    handoff: None,
                },
                None,
            )
            .expect("declare feature");
    }
    let started = service
        .progress_started(
            &project,
            &session,
            &node(),
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(600).expect("min"),
                max_s: agendar_core::model::Seconds::new(1_200).expect("max"),
            },
            None,
            None,
        )
        .expect("start work");
    let attempt = EventId::parse(started["attempt"].as_str().expect("attempt")).expect("id");
    let mut card =
        serde_json::from_value::<agendar_core::model::DecisionCard>(support::card()).expect("card");
    card.done_when = vec![agendar_core::model::Criterion {
        id: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
        text: "Demo bytes match the approved digest.".to_owned(),
    }];
    let asked = service
        .ask(
            &project,
            &session,
            &node(),
            card,
            agendar_core::model::Mode::Ask,
            None,
        )
        .expect("ask");
    let option = agendar_core::model::OptionId::parse("a").expect("option");
    let receipt = service
        .answer_decision(&project, &asked.ask_id, &option, None, None, None)
        .expect("answer");
    let blob = fixture_blob();
    let body = example_body(&session, &blob);
    let delivered = service
        .deliver(
            &project,
            &session,
            &node(),
            &attempt,
            Some(&receipt),
            None,
            body,
            Some(&ulid::Ulid::generate().to_string()),
        )
        .expect("deliver");
    let delivery = EventId::parse(delivered["event_id"].as_str().expect("event")).expect("id");
    Scratch {
        _root: root,
        data,
        repo,
        service,
        project,
        session,
        node: node(),
        attempt,
        receipt,
        delivery,
        blob,
    }
}

fn example_body(session: &SessionId, blob: &BlobRef) -> Delivery {
    Delivery {
        agent: Agent {
            session: session.clone(),
            harness: Harness::Generic,
        },
        name: "Example bytes".to_owned(),
        demos: vec![Demo::CliTranscript(CliTranscriptDemo {
            blob: blob.clone(),
            check: None,
        })],
        criteria: vec![CriterionResult {
            criterion: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
            status: CheckResult::Passed,
            claim: Claim::AgentSays,
            evidence: Some(blob.clone()),
            observation: None,
        }],
        disclosure: Disclosure {
            proven: vec!["Agent compared bytes".to_owned()],
            not_proven_yet: vec!["Independent check".to_owned()],
            faked_for_now: Vec::new(),
        },
        replies: Vec::new(),
    }
}

fn profile(scratch: &Scratch, argv: Vec<String>, timeout_s: u32) -> CheckProfile {
    CheckProfile {
        id: None,
        argv,
        args: Vec::new(),
        executable: None,
        cwd: Some(scratch.repo.to_string_lossy().into_owned()),
        repo_root: None,
        timeout_s: Some(timeout_s),
        delivery_id: None,
        criterion: None,
    }
}

fn approve(scratch: &Scratch, profile: CheckProfile) -> checks::CheckApproval {
    let local = LocalPaths::new(&scratch.data).expect("local");
    let config = Config::load_or_default(&scratch.data).expect("config");
    checks::approve(
        &local,
        scratch.service.vault(),
        &config,
        &profile,
        UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time"),
    )
    .expect("approve")
}

fn append_output(scratch: &Scratch, output: &[u8]) -> BlobRef {
    let mut reader = output;
    agendar_core::vault::blobs::put_blob(
        scratch.service.vault(),
        scratch.service.local(),
        &scratch.project,
        scratch.service.host(),
        agendar_core::model::BlobExt::Txt,
        &mut reader,
    )
    .expect("publish output")
}

/// The "not approved yet" revision is a real SHA-256 of the documented rule,
/// not a sentinel: the UI computes the same two hashes itself before it asks a
/// human to approve a profile. These goldens are also
/// asserted by the frontend test `approval payload`.
#[test]
fn the_absent_approval_revision_is_the_documented_hash() {
    let normalized = checks::NormalizedProfile {
        argv: vec![
            "/opt/agendar/bin/agendar-check".to_owned(),
            "text_equals".to_owned(),
            "--actual".to_owned(),
            "one two.txt".to_owned(),
        ],
        cwd: "repos/alpha".to_owned(),
        timeout_s: 30,
        delivery_id: None,
        criterion: None,
    };
    assert_eq!(
        checks::profile_digest(&normalized).as_str(),
        "8b5ca9c803433a44c835a7be5ada2230f3927763c603999f997a9393c803c10e"
    );
    assert_eq!(
        checks::absent_revision(&normalized).as_str(),
        "21a2a12fd04f464086beafad0b8fa952bed2ad8cc2f7fa8749b58d08055d42fe"
    );
}

#[test]
fn a_check_requires_a_linked_repository_working_directory() {
    let scratch = scratch();
    let mut profile = profile(&scratch, vec!["true".to_owned()], 10);
    profile.cwd = Some(scratch.data.to_string_lossy().into_owned());
    let local = LocalPaths::new(&scratch.data).expect("local");
    let config = Config::load_or_default(&scratch.data).expect("config");
    let error = checks::approve(
        &local,
        scratch.service.vault(),
        &config,
        &profile,
        UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time"),
    )
    .expect_err("outside a linked repo");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("check_cwd_outside_repo"));
}

#[test]
fn an_approved_check_records_seen_and_a_causal_review_accepts_it() {
    let scratch = scratch();
    let approval = approve(
        &scratch,
        profile(
            &scratch,
            vec![
                std::env::current_exe()
                    .expect("test exe")
                    .to_string_lossy()
                    .into_owned(),
                "--list".to_owned(),
            ],
            30,
        ),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let run = checks::run(&local, scratch.service.vault(), &approval).expect("run");
    assert_eq!(run.result, CheckResult::Passed);
    assert!(!run.output.is_empty());
    let output = append_output(&scratch, &run.output);
    let body = agendar_core::model::EvidenceObserved {
        criterion: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
        method: agendar_core::model::ObservationMethod::Command {
            check_id: approval.id.as_str().to_owned(),
            argv_digest: approval.revision.clone(),
        },
        started_at: run.started_at,
        finished_at: run.finished_at,
        result: run.result,
        digest: output.sha256.clone(),
        output,
        approval_id: approval.id.as_str().to_owned(),
    };
    let observation = scratch
        .service
        .append_observation(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            body,
            Vec::new(),
        )
        .expect("observation");
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let project_state = &fresh.state.projects[&scratch.project];
    let delivery = &project_state.deliveries[&scratch.delivery];
    let criterion = &delivery.criteria[0];
    assert_eq!(criterion.agent_result, CheckResult::Passed);
    assert_eq!(criterion.seen_result, Some(CheckResult::Passed));
    assert_eq!(criterion.observation.as_ref(), Some(&observation));
    assert_eq!(criterion.provenance, agendar_core::model::Provenance::Seen);
    assert!(!criterion.conflict);
    assert_eq!(delivery.review, agendar_core::model::ReviewStatus::Pending);
    assert!(
        project_state.needs_you.iter().any(|need| {
            need.kind == agendar_core::model::NeedsKind::Review && need.source == scratch.delivery
        }),
        "a pending delivery is routed to the human review queue"
    );

    // A review that does not see the observation stays pending.
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("stale review");
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    assert_eq!(
        fresh.state.projects[&scratch.project].deliveries[&scratch.delivery].review,
        agendar_core::model::ReviewStatus::Pending,
        "a review that never saw the observation is historical only"
    );

    let (review, rule) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::SendBack,
            Some("Check disagrees".to_owned()),
            Some(agendar_core::model::RuleSet {
                rule_id: agendar_core::model::RuleId::parse("check-bytes").expect("rule"),
                text: "Compare bytes before delivery".to_owned(),
                enabled: true,
            }),
            Vec::new(),
            vec![scratch.receipt.clone(), observation.clone()],
        )
        .expect("send back");
    assert!(rule.is_some());
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let project_state = &fresh.state.projects[&scratch.project];
    let delivery = &project_state.deliveries[&scratch.delivery];
    assert_eq!(delivery.review, agendar_core::model::ReviewStatus::SentBack);
    assert_eq!(delivery.review_event.as_ref(), Some(&review));
    assert_eq!(project_state.rules[0].value.rule_id.as_str(), "check-bytes");

    // A causally later explicit rereview can reverse the send-back.
    let (rereview, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone(), observation.clone(), review.clone()],
        )
        .expect("rereview");
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let delivery = &fresh.state.projects[&scratch.project].deliveries[&scratch.delivery];
    assert_eq!(delivery.review, agendar_core::model::ReviewStatus::Accepted);
    assert_eq!(delivery.review_event.as_ref(), Some(&rereview));
    assert_eq!(delivery.review_history.len(), 3);
    assert_eq!(delivery.review_frontier, vec![rereview]);
    assert!(
        fresh.state.projects[&scratch.project]
            .needs_you
            .iter()
            .all(|need| need.source != scratch.delivery),
        "an accepted delivery leaves the human review queue"
    );
}

#[test]
fn changing_the_approved_argv_invalidates_the_binding() {
    let scratch = scratch();
    let approval = approve(
        &scratch,
        profile(
            &scratch,
            vec![
                std::env::current_exe()
                    .expect("test exe")
                    .to_string_lossy()
                    .into_owned(),
                "--list".to_owned(),
            ],
            30,
        ),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let key = format!("check:{}", approval.id.as_str());
    update_runtime(
        &local,
        scratch.service.vault(),
        |runtime: &mut RuntimeState| {
            let value = runtime.approvals.get_mut(&key).expect("approval");
            value["profile"]["argv"][1] = json!("--help");
            Ok(())
        },
    )
    .expect("mutate approval");
    let error = checks::load(&local, scratch.service.vault(), approval.id.as_str())
        .expect_err("changed argv");
    assert_eq!(error.code(), ErrorCode::StaleAnswer);
    assert_eq!(error.detail(), Some("check_profile_changed"));
}

#[cfg(unix)]
#[test]
fn a_check_timeout_kills_the_owned_child_and_records_failure() {
    let scratch = scratch();
    let approval = approve(
        &scratch,
        profile(&scratch, vec!["/bin/sleep".to_owned(), "5".to_owned()], 1),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let started = Instant::now();
    let run = checks::run(&local, scratch.service.vault(), &approval).expect("run");
    assert!(
        started.elapsed().as_secs() < 4,
        "the timeout bounds the run"
    );
    assert!(run.timed_out);
    assert_eq!(run.result, CheckResult::Failed);
}

#[cfg(unix)]
#[test]
fn check_output_is_capped_at_the_documented_limit() {
    let scratch = scratch();
    let big = scratch.repo.join("big.txt");
    fs::write(&big, vec![b'x'; MAX_CHECK_OUTPUT_BYTES * 2]).expect("big file");
    let approval = approve(
        &scratch,
        profile(
            &scratch,
            vec!["/bin/cat".to_owned(), big.to_string_lossy().into_owned()],
            30,
        ),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let run = checks::run(&local, scratch.service.vault(), &approval).expect("run");
    assert_eq!(run.result, CheckResult::Passed);
    assert!(run.truncated);
    assert_eq!(run.output.len(), MAX_CHECK_OUTPUT_BYTES);
}

#[cfg(unix)]
#[test]
fn a_check_runs_without_a_shell() {
    let scratch = scratch();
    let marker = scratch.repo.join("shell-ran");
    let approval = approve(
        &scratch,
        profile(
            &scratch,
            vec![
                "/bin/echo".to_owned(),
                format!("$(touch {})", marker.to_string_lossy()),
            ],
            30,
        ),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let run = checks::run(&local, scratch.service.vault(), &approval).expect("run");
    assert_eq!(run.result, CheckResult::Passed);
    assert!(!marker.exists(), "no shell substitution happened");
    assert!(
        String::from_utf8_lossy(&run.output).contains("$(touch"),
        "the argument is passed literally"
    );
}

#[test]
fn a_nonzero_exit_is_a_failed_observation() {
    let scratch = scratch();
    let approval = approve(
        &scratch,
        profile(
            &scratch,
            vec![
                std::env::current_exe()
                    .expect("test exe")
                    .to_string_lossy()
                    .into_owned(),
                "--definitely-not-a-libtest-flag".to_owned(),
            ],
            30,
        ),
    );
    let local = LocalPaths::new(&scratch.data).expect("local");
    let run = checks::run(&local, scratch.service.vault(), &approval).expect("run");
    assert_eq!(run.result, CheckResult::Failed);
    assert!(!run.timed_out);
}

#[test]
fn a_pending_review_blocks_downstream_until_accepted() {
    let scratch = scratch();
    let evidence = append_output(&scratch, b"agent evidence\n");
    scratch
        .service
        .progress_finished(
            &scratch.project,
            &scratch.session,
            &scratch.node,
            &scratch.attempt,
            WorkStatus::Verified,
            vec![CriterionResult {
                criterion: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
                status: CheckResult::Passed,
                claim: Claim::AgentSays,
                evidence: Some(evidence),
                observation: None,
            }],
            None,
            None,
        )
        .expect("verify x");
    let now = UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time");
    let fresh = scratch.service.refresh(now, false).expect("refresh");
    let project_state = &fresh.state.projects[&scratch.project];
    let next = NodePath::parse("core/next").expect("node");
    assert_eq!(
        project_state.features[&next].gate,
        agendar_core::model::Gate::AwaitingUpstream,
        "a pending delivery review blocks downstream work"
    );
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("accept");
    let fresh = scratch.service.refresh(now, false).expect("refresh");
    assert_eq!(
        fresh.state.projects[&scratch.project].features[&next].gate,
        agendar_core::model::Gate::Open,
        "accepted review releases the downstream gate without starting work"
    );
}

#[test]
fn concurrent_observations_are_visible_as_a_frontier() {
    let scratch = scratch();
    let mut frontier = Vec::new();
    for (result, writer_offset) in [(CheckResult::Failed, 0_i64), (CheckResult::Passed, 1)] {
        let output = append_output(&scratch, b"observation output\n");
        let body = agendar_core::model::EvidenceObserved {
            criterion: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
            method: agendar_core::model::ObservationMethod::BlobDigest {
                sha256: scratch.blob.sha256.clone(),
            },
            started_at: UtcTime::parse("2026-01-01T00:00:00.000Z")
                .expect("time")
                .add_seconds(writer_offset),
            finished_at: UtcTime::parse("2026-01-01T00:00:01.000Z")
                .expect("time")
                .add_seconds(writer_offset),
            result,
            digest: output.sha256.clone(),
            output,
            approval_id: "fixture".to_owned(),
        };
        let id = scratch
            .service
            .append_observation(
                &scratch.project,
                &scratch.node,
                &scratch.delivery,
                body,
                frontier.clone(),
            )
            .expect("observation");
        frontier.push(id);
    }
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let criterion =
        &fresh.state.projects[&scratch.project].deliveries[&scratch.delivery].criteria[0];
    assert_eq!(criterion.observation_history.len(), 2);
    assert_eq!(criterion.observation_frontier.len(), 1);
    assert_eq!(
        criterion.observation.as_ref(),
        Some(&frontier[1]),
        "the causally later rerun wins"
    );
    assert_eq!(
        criterion.seen_result,
        Some(CheckResult::Passed),
        "a causally later observation supersedes the frontier"
    );
}

// ---------------------------------------------------------------------------
// Review intents reach the step owner's inbox
// ---------------------------------------------------------------------------

fn review_packets(scratch: &Scratch) -> Vec<agendar_core::model::InboxPacket> {
    let (packets, _, _) = scratch
        .service
        .inbox(&scratch.project, &scratch.session, None, false)
        .expect("inbox");
    packets
        .into_iter()
        .filter(|packet| packet.kind == agendar_core::model::PacketKind::Review)
        .collect()
}

#[test]
fn a_send_back_with_only_comments_carries_them_in_the_review_packet() {
    use agendar_core::model::{ReviewAnchor, ReviewComment};
    let scratch = scratch();
    let comments = vec![
        ReviewComment {
            anchor: ReviewAnchor::Region {
                demo: 0,
                x: 100,
                y: 50,
                w: 300,
                h: 150,
            },
            text: "The legend is cut off".to_owned(),
        },
        ReviewComment {
            anchor: ReviewAnchor::TranscriptLine {
                demo: 0,
                line: 3,
                quote: "ok".to_owned(),
            },
            text: "Show the failing case too".to_owned(),
        },
    ];
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::SendBack,
            None,
            None,
            comments.clone(),
            vec![scratch.receipt.clone()],
        )
        .expect("send back with comments only");
    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "{packets:?}");
    let body = packets[0].review.as_ref().expect("review body");
    assert_eq!(body.reason, None);
    assert_eq!(body.comments, comments);
}

#[test]
fn a_sent_back_delivery_reaches_the_step_owner_until_it_works_on_the_step_again() {
    let scratch = scratch();
    assert!(
        review_packets(&scratch).is_empty(),
        "a delivery still waiting for review sends nothing"
    );
    let (review, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::SendBack,
            Some("The screenshot is cropped".to_owned()),
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("send back");

    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "the owner gets the verdict: {packets:?}");
    let packet = &packets[0];
    assert_eq!(packet.node, scratch.node);
    assert_eq!(packet.source, review);
    let body = packet.review.as_ref().expect("review body");
    assert_eq!(body.verdict, ReviewVerdict::SendBack);
    assert_eq!(body.reason.as_deref(), Some("The screenshot is cropped"));

    // Reading it is checkpointed with a delivered record the Vault accepts; a
    // review has no ack, so the inbox keeps showing it.
    let written = scratch
        .service
        .mark_delivered(
            &scratch.project,
            &scratch.session,
            &packets,
            agendar_core::model::DeliveredOutcome::InboxRead,
        )
        .expect("checkpoint");
    assert_eq!(written.len(), 1, "one delivered record for the review");
    let now = UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time");
    let fresh = scratch.service.refresh(now, false).expect("refresh");
    assert!(
        fresh.state.diagnostics.is_empty(),
        "the delivered record validates: {:?}",
        fresh.state.diagnostics
    );
    let runtime = scratch.service.runtime().expect("runtime");
    assert!(runtime.delivery_attempts.contains_key(&format!(
        "{}:{}",
        packet.id.as_str(),
        scratch.session.as_str()
    )));
    assert_eq!(review_packets(&scratch).len(), 1);

    // Starting the fix is the continuation that retires it.
    scratch
        .service
        .progress_started_applying(
            &scratch.project,
            &scratch.session,
            &scratch.node,
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(600).expect("min"),
                max_s: agendar_core::model::Seconds::new(1_200).expect("max"),
            },
            Some("fix the crop".to_owned()),
            std::slice::from_ref(&scratch.receipt),
            None,
        )
        .expect("restart");
    assert!(
        review_packets(&scratch).is_empty(),
        "new work on the step retires the send-back"
    );
}

#[test]
fn an_accept_reaches_the_step_owner_until_it_marks_the_step_done() {
    let scratch = scratch();
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("accept");
    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "{packets:?}");
    assert_eq!(
        packets[0].review.as_ref().map(|review| review.verdict),
        Some(ReviewVerdict::Accept)
    );
    scratch
        .service
        .progress_finished(
            &scratch.project,
            &scratch.session,
            &scratch.node,
            &scratch.attempt,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    assert!(review_packets(&scratch).is_empty());
}

#[test]
fn a_finished_step_hears_a_send_back_but_not_an_accept() {
    let scratch = scratch();
    scratch
        .service
        .progress_finished(
            &scratch.project,
            &scratch.session,
            &scratch.node,
            &scratch.attempt,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    let (review, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("accept");
    assert!(
        review_packets(&scratch).is_empty(),
        "an accept on finished work leaves nothing to do"
    );
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::SendBack,
            Some("Wrong window".to_owned()),
            None,
            Vec::new(),
            vec![scratch.receipt.clone(), review],
        )
        .expect("rereview");
    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "{packets:?}");
    assert_eq!(
        packets[0]
            .review
            .as_ref()
            .and_then(|review| review.reason.as_deref()),
        Some("Wrong window")
    );
}

// ---------------------------------------------------------------------------
// Finished work no decision asked for is delivered without a receipt
// ---------------------------------------------------------------------------

fn deliver_without_receipt(
    scratch: &Scratch,
    criteria: Vec<CriterionResult>,
    supersedes: Option<&EventId>,
) -> agendar_core::error::Result<EventId> {
    let body = Delivery {
        agent: Agent {
            session: scratch.session.clone(),
            harness: Harness::Generic,
        },
        name: "Wake shipped".to_owned(),
        demos: vec![Demo::CliTranscript(CliTranscriptDemo {
            blob: scratch.blob.clone(),
            check: None,
        })],
        criteria,
        disclosure: Disclosure {
            proven: vec!["Wake adapter runs end to end".to_owned()],
            not_proven_yet: vec!["Windows wake".to_owned()],
            faked_for_now: vec!["Sample schedule".to_owned()],
        },
        replies: Vec::new(),
    };
    let delivered = scratch.service.deliver(
        &scratch.project,
        &scratch.session,
        &scratch.node,
        &scratch.attempt,
        None,
        supersedes,
        body,
        Some(&ulid::Ulid::generate().to_string()),
    )?;
    Ok(EventId::parse(delivered["event_id"].as_str().expect("event")).expect("id"))
}

fn fresh_project(scratch: &Scratch) -> agendar_core::model::ProjectState {
    let now = UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time");
    let fresh = scratch.service.refresh(now, false).expect("refresh");
    assert!(
        fresh.state.diagnostics.is_empty(),
        "{:?}",
        fresh.state.diagnostics
    );
    fresh.state.projects[&scratch.project].clone()
}

#[test]
fn a_delivery_without_a_receipt_is_projected_pending_review() {
    let scratch = scratch();
    let delivery = deliver_without_receipt(&scratch, Vec::new(), None).expect("deliver");
    let project = fresh_project(&scratch);
    let state = &project.deliveries[&delivery];
    assert_eq!(state.receipt, None);
    assert!(state.criteria.is_empty());
    assert_eq!(state.review, agendar_core::model::ReviewStatus::Pending);
    assert_eq!(
        state.disclosure.faked_for_now,
        vec!["Sample schedule".to_owned()]
    );
}

#[test]
fn a_delivery_without_a_receipt_must_list_no_criteria() {
    let scratch = scratch();
    let error = deliver_without_receipt(
        &scratch,
        vec![CriterionResult {
            criterion: agendar_core::model::CriterionId::parse("bytes").expect("criterion"),
            status: CheckResult::Passed,
            claim: Claim::AgentSays,
            evidence: None,
            observation: None,
        }],
        None,
    )
    .expect_err("criteria without a receipt");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(error.detail(), Some("delivery_criteria_without_receipt"));
    assert_eq!(error.field(), Some("/criteria"));
}

#[test]
fn a_delivery_without_a_receipt_is_accepted_and_sent_back() {
    let scratch = scratch();
    let delivery = deliver_without_receipt(&scratch, Vec::new(), None).expect("deliver");
    let (accepted, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("accept");
    let project = fresh_project(&scratch);
    assert_eq!(
        project.deliveries[&delivery].review,
        agendar_core::model::ReviewStatus::Accepted
    );
    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "{packets:?}");
    assert_eq!(packets[0].source, accepted);

    let (sent_back, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &delivery,
            ReviewVerdict::SendBack,
            Some("Show the Windows run".to_owned()),
            None,
            Vec::new(),
            vec![accepted],
        )
        .expect("send back");
    let project = fresh_project(&scratch);
    assert_eq!(
        project.deliveries[&delivery].review,
        agendar_core::model::ReviewStatus::SentBack
    );
    let packets = review_packets(&scratch);
    assert_eq!(packets.len(), 1, "{packets:?}");
    assert_eq!(packets[0].source, sent_back);
    assert_eq!(
        packets[0]
            .review
            .as_ref()
            .and_then(|review| review.reason.as_deref()),
        Some("Show the Windows run")
    );
}

#[test]
fn a_delivery_without_a_receipt_counts_once_in_pending_slices() {
    let scratch = scratch();
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("accept the receipt delivery");
    assert_eq!(fresh_project(&scratch).summary.budget.pending_slices, 0);
    let first = deliver_without_receipt(&scratch, Vec::new(), None).expect("deliver");
    assert_eq!(fresh_project(&scratch).summary.budget.pending_slices, 1);
    deliver_without_receipt(&scratch, Vec::new(), Some(&first)).expect("redeliver");
    assert_eq!(
        fresh_project(&scratch).summary.budget.pending_slices,
        1,
        "a superseded delivery never occupies the budget"
    );
}

fn redeliver(
    scratch: &Scratch,
    supersedes: Option<&EventId>,
    replies: Vec<agendar_core::model::Reply>,
) -> agendar_core::error::Result<serde_json::Value> {
    let mut body = example_body(&scratch.session, &scratch.blob);
    body.name = "Example bytes, fixed".to_owned();
    body.replies = replies;
    scratch.service.deliver(
        &scratch.project,
        &scratch.session,
        &scratch.node,
        &scratch.attempt,
        Some(&scratch.receipt),
        supersedes,
        body,
        Some(&ulid::Ulid::generate().to_string()),
    )
}

fn reply(comment: Option<u8>, text: &str) -> agendar_core::model::Reply {
    agendar_core::model::Reply {
        comment,
        text: text.to_owned(),
    }
}

fn detail(error: &agendar_core::error::AgendarError) -> Option<&str> {
    error.detail()
}

#[test]
fn a_redelivery_replies_to_the_send_back_it_supersedes() {
    let scratch = scratch();
    // Replies need a delivery to supersede.
    let error = redeliver(&scratch, None, vec![reply(None, "Fixed")]).expect_err("no supersedes");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(detail(&error), Some("delivery_replies_without_supersedes"));
    // ... and that delivery must have been sent back.
    let error = redeliver(
        &scratch,
        Some(&scratch.delivery),
        vec![reply(None, "Fixed")],
    )
    .expect_err("no send-back");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(detail(&error), Some("replies_without_send_back"));
    assert!(error.hint().is_some(), "{error:?}");

    let region = |x: u16| agendar_core::model::ReviewComment {
        anchor: agendar_core::model::ReviewAnchor::Region {
            demo: 0,
            x,
            y: 0,
            w: 10,
            h: 10,
        },
        text: format!("Area at {x}"),
    };
    let (review, _) = scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::SendBack,
            Some("Too slow".to_owned()),
            None,
            vec![region(0), region(500)],
            vec![scratch.receipt.clone()],
        )
        .expect("send back");

    // A comment number past the send-back's comments is refused.
    let error = redeliver(
        &scratch,
        Some(&scratch.delivery),
        vec![reply(Some(3), "Which one?")],
    )
    .expect_err("past the count");
    assert_eq!(error.code(), ErrorCode::Validation);
    assert_eq!(detail(&error), Some("reply_comment_out_of_range"));
    // Wire rules: one reply per target, numbers from 1, short text.
    for (replies, code) in [
        (vec![reply(None, "a"), reply(None, "b")], "duplicate_reply"),
        (vec![reply(Some(0), "zero")], "reply_comment_out_of_range"),
        (vec![reply(Some(1), &"x".repeat(400))], "length"),
    ] {
        let error = redeliver(&scratch, Some(&scratch.delivery), replies).expect_err(code);
        assert_eq!(error.code(), ErrorCode::Validation, "{code}");
        assert_eq!(detail(&error), Some(code));
    }

    let delivered = redeliver(
        &scratch,
        Some(&scratch.delivery),
        vec![reply(None, "Cached the lookup"), reply(Some(2), "Moved it")],
    )
    .expect("redeliver");
    let redelivery = EventId::parse(delivered["event_id"].as_str().expect("event")).expect("id");
    let fresh = scratch
        .service
        .refresh(
            UtcTime::parse("2026-01-01T01:00:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let project_state = &fresh.state.projects[&scratch.project];
    let old = &project_state.deliveries[&scratch.delivery];
    let new = &project_state.deliveries[&redelivery];
    assert_eq!(old.superseded_by.as_ref(), Some(&redelivery));
    assert_eq!(old.review, agendar_core::model::ReviewStatus::Pending);
    let note = old
        .sent_back
        .as_ref()
        .expect("send-back kept after supersede");
    assert_eq!(note.review, review);
    assert_eq!(note.reason.as_deref(), Some("Too slow"));
    assert_eq!(note.comments, vec![region(0), region(500)]);
    assert_eq!(new.supersedes.as_ref(), Some(&scratch.delivery));
    assert_eq!(
        new.replies,
        vec![reply(None, "Cached the lookup"), reply(Some(2), "Moved it")]
    );
    assert!(new.sent_back.is_none());

    // The new fields are skipped when empty, so a first delivery projects
    // and stores the same bytes as before.
    let old_json = serde_json::to_value(old).expect("json");
    assert!(old_json.get("supersedes").is_none());
    assert!(old_json.get("replies").is_none());
    let new_json = serde_json::to_value(new).expect("json");
    assert!(new_json.get("sent_back").is_none());
    let first = serde_json::to_value(example_body(&scratch.session, &scratch.blob)).expect("json");
    assert!(first.get("replies").is_none(), "{first}");
}

fn accept(scratch: &Scratch) {
    scratch
        .service
        .review_delivery(
            &scratch.project,
            &scratch.node,
            &scratch.delivery,
            ReviewVerdict::Accept,
            None,
            None,
            Vec::new(),
            vec![scratch.receipt.clone()],
        )
        .expect("accept");
}

fn finish(scratch: &Scratch, status: WorkStatus) -> agendar_core::Result<serde_json::Value> {
    scratch.service.progress_finished(
        &scratch.project,
        &scratch.session,
        &scratch.node,
        &scratch.attempt,
        status,
        Vec::new(),
        None,
        None,
    )
}

#[test]
fn an_accepted_step_is_done_and_a_confirmation_says_it_was_already_accepted() {
    let scratch = scratch();
    accept(&scratch);
    let fresh = scratch
        .service
        .refresh(
            agendar_core::vault::Clock::now(&agendar_core::vault::SystemClock),
            false,
        )
        .expect("refresh");
    let feature = &fresh.state.projects[&scratch.project].features[&scratch.node];
    assert_eq!(feature.progress, agendar_core::model::Progress::Done);
    let done = finish(&scratch, WorkStatus::Done).expect("done succeeds");
    assert_eq!(done["progress"], "done");
    assert_eq!(done["already"], "accepted");
}

#[test]
fn the_service_refuses_to_drop_an_accepted_step() {
    let scratch = scratch();
    accept(&scratch);
    let error = finish(&scratch, WorkStatus::Dropped).expect_err("drop refused");
    assert_eq!(error.detail(), Some("accepted_by_owner"));
    assert!(
        error.message().contains("send it back"),
        "{}",
        error.message()
    );
}
